//! Real flows on Linux, worked out from `/proc`.
//!
//! Every sample, each process's storage I/O counters show how much it read
//! and wrote. A process that did both is a candidate copy: its open files
//! say which drives it was reading from and writing to, and the flow between
//! them is the smaller of the two rates. Files are traced to a drive
//! through the mount they're on, using the process's own mount table, so
//! programs inside containers map to the right drive too.
//!
//! Without root this only sees processes run by the same user. Paths are
//! rooted at a parameter so tests can use a fake tree; where `/proc` doesn't
//! exist nothing is found.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::Flow;

/// Copies slower than this aren't shown, so logs and databases ticking
/// over don't clutter the screen.
pub const MIN_FLOW_RATE: f64 = 512.0 * 1024.0;

/// Maps a kernel block device name (`sdb`, `dm-3`) to the panel it belongs to.
pub type DeviceGroups = HashMap<String, String>;

/// What was read from one process last time.
#[derive(Debug, Clone)]
struct Seen {
    read: u64,
    written: u64,
    /// File offsets by descriptor, to weigh which files moved the data.
    positions: HashMap<u32, u64>,
}

/// One open file that counts towards a process's reads or writes.
#[derive(Debug, Clone, PartialEq)]
struct OpenFile {
    group: String,
    writes: bool,
    /// Bytes this descriptor's offset moved since last time.
    moved: u64,
}

pub struct ProcFlows {
    proc: PathBuf,
    seen: HashMap<u32, Seen>,
}

impl ProcFlows {
    pub fn new(proc: impl Into<PathBuf>) -> Self {
        Self {
            proc: proc.into(),
            seen: HashMap::new(),
        }
    }

    /// True when every process can be inspected, which takes root.
    pub fn sees_everything(&self) -> bool {
        fs::read(self.proc.join("1/io")).is_ok()
    }

    /// The flows between groups since the last call, `elapsed_secs` ago.
    pub fn sample(&mut self, elapsed_secs: Option<f64>, devices: &DeviceGroups) -> Vec<Flow> {
        let Ok(entries) = fs::read_dir(&self.proc) else {
            return Vec::new();
        };
        let mut seen = HashMap::new();
        let mut flows: Vec<Flow> = Vec::new();

        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let dir = entry.path();
            let Some((read, written)) = read_text(&dir.join("io")).and_then(|t| parse_io(&t))
            else {
                continue;
            };
            let previous = self.seen.get(&pid);
            let mut now = Seen {
                read,
                written,
                positions: HashMap::new(),
            };

            if let (Some(elapsed), Some(before)) = (elapsed_secs, previous)
                && elapsed > 0.0
            {
                let read_rate = read.saturating_sub(before.read) as f64 / elapsed;
                let write_rate = written.saturating_sub(before.written) as f64 / elapsed;
                if read_rate.min(write_rate) >= MIN_FLOW_RATE {
                    let files = open_files(&dir, devices, &before.positions, &mut now.positions);
                    let process = read_text(&dir.join("comm"))
                        .map(|c| c.trim().to_string())
                        .unwrap_or_else(|| pid.to_string());
                    for (from, to, rate) in attribute(&files, read_rate.min(write_rate)) {
                        add_flow(&mut flows, from, to, rate, &process);
                    }
                }
            }
            seen.insert(pid, now);
        }

        self.seen = seen;
        flows.retain(|f| f.rate >= MIN_FLOW_RATE);
        flows.sort_by(|a, b| b.rate.total_cmp(&a.rate));
        flows
    }
}

/// Adds a process's share of a flow, merging with other processes copying
/// between the same two groups.
fn add_flow(flows: &mut Vec<Flow>, from: String, to: String, rate: f64, process: &str) {
    match flows.iter_mut().find(|f| f.from == from && f.to == to) {
        Some(flow) => {
            flow.rate += rate;
            if !flow.process.split(", ").any(|p| p == process) {
                flow.process = format!("{}, {process}", flow.process);
            }
        }
        None => flows.push(Flow {
            from,
            to,
            rate,
            process: process.to_string(),
        }),
    }
}

/// The process's open files on known drives, with how far each one's
/// offset moved since last time. Records the new offsets in `positions`.
fn open_files(
    dir: &Path,
    devices: &DeviceGroups,
    before: &HashMap<u32, u64>,
    positions: &mut HashMap<u32, u64>,
) -> Vec<OpenFile> {
    let Some(mounts) = read_text(&dir.join("mountinfo")).map(|t| parse_mountinfo(&t)) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir.join("fdinfo")) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(info) = read_text(&entry.path()).and_then(|t| parse_fdinfo(&t)) else {
            continue;
        };
        // Sockets, pipes and the like sit on mounts that aren't listed, and
        // files on /proc or tmpfs don't map to a drive, so both drop out here.
        let Some(group) = mounts
            .get(&info.mount_id)
            .and_then(|source| devices.get(&kernel_name(source)))
        else {
            continue;
        };
        positions.insert(fd, info.position);
        let moved = before
            .get(&fd)
            .map_or(0, |&old| info.position.saturating_sub(old));
        files.push(OpenFile {
            group: group.clone(),
            writes: info.writes,
            moved,
        });
    }
    files
}

/// Splits `rate` across every pair of a group read from and a different
/// group written to, in proportion to how much each side's files moved.
/// When offsets don't move (programs using positioned reads and writes),
/// each open file counts equally.
fn attribute(files: &[OpenFile], rate: f64) -> Vec<(String, String, f64)> {
    let shares = |writes: bool| -> Vec<(String, f64)> {
        let side: Vec<&OpenFile> = files.iter().filter(|f| f.writes == writes).collect();
        let total_moved: u64 = side.iter().map(|f| f.moved).sum();
        let mut shares: Vec<(String, f64)> = Vec::new();
        for file in &side {
            let weight = if total_moved > 0 {
                file.moved as f64 / total_moved as f64
            } else {
                1.0 / side.len() as f64
            };
            match shares.iter_mut().find(|(g, _)| *g == file.group) {
                Some((_, share)) => *share += weight,
                None => shares.push((file.group.clone(), weight)),
            }
        }
        shares
    };
    let reads = shares(false);
    let writes = shares(true);

    let mut flows = Vec::new();
    for (from, read_share) in &reads {
        for (to, write_share) in &writes {
            // Reading and writing the same drive is a database or a log,
            // not a copy between drives.
            if from != to {
                flows.push((from.clone(), to.clone(), rate * read_share * write_share));
            }
        }
    }
    flows
}

/// Storage-level (bytes read, bytes written) from `/proc/<pid>/io`. These
/// count what actually reached or came from disk, not cache hits or sockets.
fn parse_io(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    Some((field("read_bytes:")?, field("write_bytes:")?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FdInfo {
    position: u64,
    /// Opened for writing (write-only or read-write).
    writes: bool,
    mount_id: u32,
}

/// The parts of `/proc/<pid>/fdinfo/<fd>` that matter here.
fn parse_fdinfo(text: &str) -> Option<FdInfo> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
    };
    let flags = u32::from_str_radix(field("flags:")?, 8).ok()?;
    Some(FdInfo {
        position: field("pos:")?.parse().ok()?,
        // The low two bits are the access mode: 0 read-only, 1 write-only, 2 both.
        writes: flags & 0o3 != 0,
        mount_id: field("mnt_id:")?.parse().ok()?,
    })
}

/// Mount ID → mount source from `/proc/<pid>/mountinfo`. A line looks like
/// `36 35 98:0 /data /mnt rw shared:1 - ext4 /dev/sdb1 rw`; the source is
/// the second field after the ` - ` separator.
fn parse_mountinfo(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let id = before.split_whitespace().next()?.parse().ok()?;
            let source = after.split_whitespace().nth(1)?;
            Some((id, source.to_string()))
        })
        .collect()
}

/// The kernel's name for a mount source: `sdb` for `/dev/sdb`, `dm-3` for
/// `/dev/mapper/mpathh-part1`. Non-device sources come back as they are.
fn kernel_name(source: &str) -> String {
    let resolved = fs::canonicalize(source).unwrap_or_else(|_| PathBuf::from(source));
    resolved
        .file_name()
        .map_or_else(|| source.to_string(), |n| n.to_string_lossy().into_owned())
}

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn io_counters_are_the_storage_ones() {
        let text = "rchar: 999\nwchar: 888\nsyscr: 1\nsyscw: 2\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n";
        assert_eq!(parse_io(text), Some((4096, 8192)));
        assert_eq!(parse_io("rchar: 1\n"), None);
    }

    #[test]
    fn fdinfo_gives_offset_mode_and_mount() {
        let read_only = "pos:\t1048576\nflags:\t0100000\nmnt_id:\t36\nino:\t42\n";
        assert_eq!(
            parse_fdinfo(read_only),
            Some(FdInfo {
                position: MIB,
                writes: false,
                mount_id: 36
            })
        );
        let write_only = "pos:\t0\nflags:\t0101101\nmnt_id:\t40\n";
        assert!(parse_fdinfo(write_only).unwrap().writes);
        let read_write = "pos:\t0\nflags:\t02\nmnt_id:\t40\n";
        assert!(parse_fdinfo(read_write).unwrap().writes);
        assert_eq!(parse_fdinfo("pos:\t0\n"), None);
    }

    #[test]
    fn mountinfo_maps_ids_to_sources() {
        let text = "\
29 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme1n1p2 rw
36 29 0:45 / /main rw,relatime shared:7 - btrfs /dev/sdb rw,space_cache=v2
41 29 252:3 / /mnt/MediaArchive3 rw shared:9 - ext4 /dev/mapper/mpathh-part1 rw
50 29 0:5 / /proc rw - proc proc rw
";
        let mounts = parse_mountinfo(text);
        assert_eq!(mounts[&29], "/dev/nvme1n1p2");
        assert_eq!(mounts[&36], "/dev/sdb");
        assert_eq!(mounts[&41], "/dev/mapper/mpathh-part1");
        assert_eq!(mounts[&50], "proc");
    }

    fn file(group: &str, writes: bool, moved: u64) -> OpenFile {
        OpenFile {
            group: group.into(),
            writes,
            moved,
        }
    }

    #[test]
    fn a_copy_between_two_drives_is_one_flow() {
        let files = [file("/main", false, 0), file("/mnt/MediaArchive3", true, 0)];
        assert_eq!(
            attribute(&files, 100.0),
            vec![("/main".into(), "/mnt/MediaArchive3".into(), 100.0)]
        );
    }

    #[test]
    fn rate_is_split_by_how_far_each_file_moved() {
        let files = [
            file("/main", false, 3 * MIB),
            file("/mnt/SSD1", false, MIB),
            file("/", true, 0),
        ];
        let flows = attribute(&files, 100.0);
        assert_eq!(
            flows,
            vec![
                ("/main".into(), "/".into(), 75.0),
                ("/mnt/SSD1".into(), "/".into(), 25.0)
            ]
        );
    }

    #[test]
    fn reading_and_writing_one_drive_is_not_a_flow() {
        let files = [file("/main", false, 0), file("/main", true, 0)];
        assert!(attribute(&files, 100.0).is_empty());
        assert!(attribute(&[file("/main", false, 0)], 100.0).is_empty());
    }

    #[test]
    fn processes_copying_between_the_same_drives_merge() {
        let mut flows = Vec::new();
        add_flow(&mut flows, "a".into(), "b".into(), 10.0, "rsync");
        add_flow(&mut flows, "a".into(), "b".into(), 5.0, "Sonarr");
        add_flow(&mut flows, "a".into(), "b".into(), 1.0, "rsync");
        add_flow(&mut flows, "b".into(), "a".into(), 2.0, "cp");
        assert_eq!(flows.len(), 2);
        assert_eq!(flows[0].rate, 16.0);
        assert_eq!(flows[0].process, "rsync, Sonarr");
    }

    /// A throwaway directory tree standing in for `/proc`.
    struct FakeProc(PathBuf);

    impl FakeProc {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("disktop-proc-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn write(&self, path: &str, contents: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        /// A process copying from fd 3 to fd 4, having read and written
        /// `progress` bytes so far.
        fn copier(&self, pid: u32, comm: &str, progress: u64) {
            self.write(&format!("{pid}/comm"), &format!("{comm}\n"));
            self.write(
                &format!("{pid}/io"),
                &format!("read_bytes: {progress}\nwrite_bytes: {progress}\n"),
            );
            self.write(
                &format!("{pid}/mountinfo"),
                "36 29 0:45 / /main rw - btrfs /dev/sdb rw\n\
                 41 29 252:3 / /mnt/MediaArchive3 rw - ext4 /dev/mapper/mpathh-part1 rw\n",
            );
            self.write(
                &format!("{pid}/fdinfo/3"),
                &format!("pos:\t{progress}\nflags:\t0100000\nmnt_id:\t36\n"),
            );
            self.write(
                &format!("{pid}/fdinfo/4"),
                &format!("pos:\t{progress}\nflags:\t0101101\nmnt_id:\t41\n"),
            );
            // A socket: its mount isn't in mountinfo, so it's ignored.
            self.write(
                &format!("{pid}/fdinfo/5"),
                "pos:\t0\nflags:\t02\nmnt_id:\t8\n",
            );
        }
    }

    impl Drop for FakeProc {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn god_server_devices() -> DeviceGroups {
        [("sdb", "/main"), ("mpathh-part1", "/mnt/MediaArchive3")]
            .into_iter()
            .map(|(d, g)| (d.to_string(), g.to_string()))
            .collect()
    }

    #[test]
    fn a_copying_process_shows_up_as_a_flow() {
        let proc = FakeProc::new("copy");
        let devices = god_server_devices();
        let mut tracer = ProcFlows::new(&proc.0);

        proc.copier(4242, "Sonarr", 100 * MIB);
        assert!(
            tracer.sample(None, &devices).is_empty(),
            "nothing to compare yet"
        );

        proc.copier(4242, "Sonarr", 300 * MIB);
        let flows = tracer.sample(Some(2.0), &devices);
        assert_eq!(
            flows,
            vec![Flow {
                from: "/main".into(),
                to: "/mnt/MediaArchive3".into(),
                rate: (100 * MIB) as f64,
                process: "Sonarr".into(),
            }]
        );
    }

    #[test]
    fn slow_or_one_sided_processes_are_not_flows() {
        let proc = FakeProc::new("quiet");
        let devices = god_server_devices();
        let mut tracer = ProcFlows::new(&proc.0);
        proc.copier(1, "trickle", 0);
        tracer.sample(None, &devices);
        proc.copier(1, "trickle", 100 * 1024);
        assert!(
            tracer.sample(Some(1.0), &devices).is_empty(),
            "under the minimum"
        );

        // Only reading: a media server streaming a file isn't a copy.
        proc.write(
            "1/io",
            &format!("read_bytes: {}\nwrite_bytes: {}\n", 900 * MIB, 100 * 1024),
        );
        assert!(tracer.sample(Some(1.0), &devices).is_empty());
    }

    #[test]
    fn missing_proc_finds_nothing() {
        let mut tracer = ProcFlows::new("/definitely/not/proc");
        assert!(tracer.sample(Some(1.0), &DeviceGroups::new()).is_empty());
        assert!(!tracer.sees_everything());
    }
}
