use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use sysinfo::{Disk, DiskKind, Disks};

use crate::model::{Flow, Group, GroupKind, History, Member, Rate, Source};
use crate::sysfs::{self, Btrfs};

/// Where Linux exposes block devices and filesystems. Missing elsewhere,
/// which simply means no arrays are found.
const SYS: &str = "/sys";
const DISKSTATS: &str = "/proc/diskstats";

/// Cumulative (bytes read, bytes written) counters, keyed by what they count.
type Counters = HashMap<String, (u64, u64)>;

/// File systems that aren't drives: container image layers and similar
/// virtual mounts that sysinfo still reports.
const HIDDEN_FILE_SYSTEMS: [&str; 6] = [
    "overlay",
    "aufs",
    "nsfs",
    "ramfs",
    "fuse.lxcfs",
    "fuse.snapfuse",
];

/// Live readings from the drives on this machine.
///
/// Each volume is a standalone group, except a btrfs filesystem spanning
/// several disks on Linux, which becomes an array with a member per disk.
/// No flows are reported yet.
pub struct DriveMonitor {
    disks: Disks,
    groups: Vec<Group>,
    /// Counters from the previous sample: by mount point for volumes, and
    /// by `disk:<name>` for array members.
    counters: Counters,
    last_sample: Instant,
}

impl DriveMonitor {
    pub fn new() -> Self {
        let disks = Disks::new_with_refreshed_list();
        let mut monitor = Self {
            disks,
            groups: Vec::new(),
            counters: HashMap::new(),
            last_sample: Instant::now(),
        };
        monitor.rebuild(None);
        monitor
    }

    /// Rebuilds the group list from the current disk list, carrying each
    /// drive's history over. With `elapsed_secs`, also records a new sample.
    fn rebuild(&mut self, elapsed_secs: Option<f64>) {
        let mut previous: HashMap<String, Group> = std::mem::take(&mut self.groups)
            .into_iter()
            .map(|g| (g.id.clone(), g))
            .collect();
        let mut counters = Counters::new();
        let sample = Sample {
            elapsed_secs,
            previous: &self.counters,
        };

        // Btrfs filesystems that span several disks, and which one each disk
        // belongs to.
        let arrays: Vec<Btrfs> = sysfs::btrfs_filesystems(Path::new(SYS))
            .into_iter()
            .filter(|fs| fs.devices.len() > 1)
            .collect();
        let array_of: HashMap<&str, usize> = arrays
            .iter()
            .enumerate()
            .flat_map(|(i, fs)| fs.devices.iter().map(move |d| (d.as_str(), i)))
            .collect();
        let diskstats = if arrays.is_empty() {
            Counters::new()
        } else {
            std::fs::read_to_string(DISKSTATS)
                .map(|text| sysfs::parse_diskstats(&text))
                .unwrap_or_default()
        };

        let disks = self.disks.list();
        let facts: Vec<MountFacts> = disks
            .iter()
            .map(|d| {
                let device = d.name().to_string_lossy().into_owned();
                let kernel = kernel_name(&device);
                let array = kernel
                    .as_deref()
                    .and_then(|name| array_of.get(name).copied());
                // Mounts of one btrfs filesystem may name different member
                // devices, so they're merged by filesystem instead.
                let key = match array {
                    Some(i) => Some(format!("btrfs:{}", arrays[i].uuid)),
                    None => device.starts_with("/dev/").then_some(device),
                };
                MountFacts {
                    mount: d.mount_point().display().to_string(),
                    key,
                    file_system: d.file_system().to_string_lossy().into_owned(),
                    kernel,
                    array,
                }
            })
            .collect();
        let mounts: Vec<MountInfo> = facts
            .iter()
            .map(|f| MountInfo {
                mount: &f.mount,
                key: f.key.as_deref(),
                file_system: &f.file_system,
            })
            .collect();

        for (index, other_mounts) in pick_mounts(&mounts) {
            let disk = &disks[index];
            let id = facts[index].mount.clone();
            let old = previous.remove(&id);
            let mut group = match facts[index].array {
                Some(array) => array_group(
                    disk,
                    id,
                    &arrays[array],
                    old,
                    &diskstats,
                    &sample,
                    &mut counters,
                ),
                None => volume_group(
                    disk,
                    id,
                    facts[index].kernel.as_deref(),
                    old,
                    &sample,
                    &mut counters,
                ),
            };
            if other_mounts > 0 {
                let plural = if other_mounts == 1 { "" } else { "s" };
                group.details.push(format!("+{other_mounts} mount{plural}"));
            }
            self.groups.push(group);
        }

        self.counters = counters;
        self.groups.sort_by(|a, b| a.id.cmp(&b.id));
    }
}

impl Source for DriveMonitor {
    /// Takes a new reading from every drive. Drives that were plugged in
    /// since the last sample are added, and drives that went away are dropped.
    fn sample(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_sample).as_secs_f64();
        self.last_sample = now;

        self.disks.refresh(true);
        self.rebuild(Some(elapsed));
    }

    fn groups(&self) -> &[Group] {
        &self.groups
    }

    fn flows(&self) -> &[Flow] {
        &[]
    }
}

/// What's known about a mount before deciding whether it gets a panel.
struct MountFacts {
    mount: String,
    key: Option<String>,
    file_system: String,
    /// The kernel's name for its device, on Linux.
    kernel: Option<String>,
    /// Index of the multi-disk btrfs filesystem it belongs to.
    array: Option<usize>,
}

/// The facts used to pick which mounts get a panel.
struct MountInfo<'a> {
    mount: &'a str,
    /// Mounts with the same key are the same storage and get one panel: the
    /// device path on Linux, or the btrfs filesystem. `None` (Windows volume
    /// labels, which can repeat) never merges.
    key: Option<&'a str>,
    file_system: &'a str,
}

/// The kernel's name for a `/dev` device, such as `sdb` for `/dev/sdb` or
/// `dm-2` for `/dev/mapper/mpathg-part1`.
fn kernel_name(device: &str) -> Option<String> {
    if !device.starts_with("/dev/") {
        return None;
    }
    let resolved = std::fs::canonicalize(device).unwrap_or_else(|_| device.into());
    resolved
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Picks the mounts to show, as indices into `mounts`, each with how many
/// other mounts of the same device it stands in for. Virtual file systems
/// are skipped, and a device mounted in several places (a bind mount or a
/// btrfs subvolume) is shown once, under its shortest mount path.
fn pick_mounts(mounts: &[MountInfo]) -> Vec<(usize, usize)> {
    let mut order: Vec<usize> = (0..mounts.len())
        .filter(|&i| !HIDDEN_FILE_SYSTEMS.contains(&mounts[i].file_system))
        .collect();
    order.sort_by_key(|&i| (mounts[i].mount.len(), mounts[i].mount));

    let mut picked: Vec<(usize, usize)> = Vec::new();
    let mut by_key: HashMap<&str, usize> = HashMap::new();
    for i in order {
        if let Some(key) = mounts[i].key {
            if let Some(&slot) = by_key.get(key) {
                picked[slot].1 += 1;
                continue;
            }
            by_key.insert(key, picked.len());
        }
        picked.push((i, 0));
    }
    picked
}

/// The time since the last sample and the counters it recorded, used to
/// turn cumulative counters into rates.
struct Sample<'a> {
    elapsed_secs: Option<f64>,
    previous: &'a Counters,
}

impl Sample<'_> {
    /// Records `now` under `key` and returns the rate since the last sample.
    fn rate(&self, key: &str, now: (u64, u64), next: &mut Counters) -> Option<Rate> {
        next.insert(key.to_string(), now);
        let elapsed = self.elapsed_secs?;
        let &(read, written) = self.previous.get(key)?;
        Some(Rate {
            read: per_second(read, now.0, elapsed),
            write: per_second(written, now.1, elapsed),
        })
    }
}

/// A standalone volume, with I/O from sysinfo.
fn volume_group(
    disk: &Disk,
    id: String,
    kernel: Option<&str>,
    old: Option<Group>,
    sample: &Sample,
    counters: &mut Counters,
) -> Group {
    let usage = disk.usage();
    let mut history = old
        .and_then(|g| g.members.into_iter().next())
        .map(|m| m.history)
        .unwrap_or_default();
    let now = (usage.total_read_bytes, usage.total_written_bytes);
    if let Some(rate) = sample.rate(&id, now, counters) {
        history.push(rate);
    }
    let physical_disks = kernel
        .map(|name| sysfs::physical_disks(Path::new(SYS), name))
        .unwrap_or_default();
    group_for(disk, id, history, physical_disks)
}

/// A btrfs filesystem spanning several disks, with a member and its own
/// I/O for each disk.
fn array_group(
    disk: &Disk,
    id: String,
    fs: &Btrfs,
    old: Option<Group>,
    diskstats: &Counters,
    sample: &Sample,
    counters: &mut Counters,
) -> Group {
    let sys = Path::new(SYS);
    let mut histories: HashMap<String, History> = old
        .map(|g| g.members.into_iter().map(|m| (m.name, m.history)).collect())
        .unwrap_or_default();
    let members = fs
        .devices
        .iter()
        .map(|device| {
            let mut history = histories.remove(device).unwrap_or_default();
            if let Some(&now) = diskstats.get(device)
                && let Some(rate) = sample.rate(&format!("disk:{device}"), now, counters)
            {
                history.push(rate);
            }
            Member {
                name: device.clone(),
                details: sysfs::device_details(sys, device),
                history,
            }
        })
        .collect();

    let layout = sysfs::btrfs_layout(fs.profile.as_deref());
    let mut details = Vec::new();
    if !fs.label.is_empty() {
        details.push(fs.label.clone());
    }
    details.push(layout.level.clone());
    details.push("btrfs".to_string());
    let spinning: Vec<Option<bool>> = fs
        .devices
        .iter()
        .map(|d| sysfs::is_rotational(sys, d))
        .collect();
    if spinning.iter().all(|s| *s == Some(true)) {
        details.push("HDD".to_string());
    } else if spinning.iter().all(|s| *s == Some(false)) {
        details.push("SSD".to_string());
    }
    let physical_disks = fs
        .devices
        .iter()
        .flat_map(|d| sysfs::physical_disks(sys, d))
        .collect();

    Group {
        title: id.clone(),
        id,
        details,
        kind: GroupKind::Array(layout),
        total_space: disk.total_space(),
        available_space: disk.available_space(),
        members,
        physical_disks,
    }
}

fn group_for(disk: &Disk, id: String, history: History, physical_disks: Vec<String>) -> Group {
    let label = disk.name().to_string_lossy().into_owned();
    let mut details = Vec::new();
    if !label.is_empty() {
        details.push(label);
    }
    let file_system = disk.file_system().to_string_lossy().into_owned();
    if !file_system.is_empty() {
        details.push(file_system);
    }
    match disk.kind() {
        DiskKind::SSD => details.push("SSD".to_string()),
        DiskKind::HDD => details.push("HDD".to_string()),
        DiskKind::Unknown(_) => {}
    }
    if disk.is_removable() {
        details.push("removable".to_string());
    }

    Group {
        title: id.clone(),
        id,
        details,
        kind: GroupKind::Standalone,
        total_space: disk.total_space(),
        available_space: disk.available_space(),
        members: vec![Member {
            name: String::new(),
            details: String::new(),
            history,
        }],
        physical_disks,
    }
}

/// Bytes per second between two readings of a cumulative counter.
///
/// A counter that goes backwards (a drive was reset or re-mounted) reads as
/// zero rather than a huge spike.
fn per_second(previous: u64, current: u64, elapsed_secs: f64) -> f64 {
    if elapsed_secs <= 0.0 {
        return 0.0;
    }
    current.saturating_sub(previous) as f64 / elapsed_secs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_is_bytes_over_elapsed_time() {
        assert_eq!(per_second(1_000, 3_000, 2.0), 1_000.0);
    }

    #[test]
    fn counter_going_backwards_reads_as_zero() {
        assert_eq!(per_second(5_000, 1_000, 1.0), 0.0);
    }

    #[test]
    fn zero_elapsed_time_reads_as_zero() {
        assert_eq!(per_second(0, 1_000, 0.0), 0.0);
    }

    /// A mount keyed the way the monitor keys it: by `/dev` path, and not at
    /// all for a Windows volume label.
    fn mount<'a>(mount: &'a str, device: &'a str, file_system: &'a str) -> MountInfo<'a> {
        MountInfo {
            mount,
            key: device.starts_with("/dev/").then_some(device),
            file_system,
        }
    }

    fn picked_mounts<'a>(mounts: &'a [MountInfo<'a>]) -> Vec<(&'a str, usize)> {
        let mut picked: Vec<(&str, usize)> = pick_mounts(mounts)
            .into_iter()
            .map(|(i, others)| (mounts[i].mount, others))
            .collect();
        picked.sort();
        picked
    }

    #[test]
    fn container_layers_are_hidden() {
        let mounts = [
            mount("/", "/dev/nvme1n1p2", "ext4"),
            mount(
                "/main/docker/data/rootfs/overlayfs/0ded2a",
                "overlay",
                "overlay",
            ),
            mount(
                "/main/docker/data/rootfs/overlayfs/fef18f",
                "overlay",
                "overlay",
            ),
        ];
        assert_eq!(picked_mounts(&mounts), vec![("/", 0)]);
    }

    #[test]
    fn a_device_mounted_twice_is_shown_once_under_its_shortest_path() {
        let mounts = [
            mount("/export/MainMediaShare", "/dev/sdb", "btrfs"),
            mount("/main", "/dev/sdb", "btrfs"),
            mount("/mnt/Media01", "/dev/mapper/mpathg-part1", "ext4"),
        ];
        assert_eq!(
            picked_mounts(&mounts),
            vec![("/main", 1), ("/mnt/Media01", 0)]
        );
    }

    #[test]
    fn mounts_of_one_btrfs_filesystem_merge_even_through_different_disks() {
        let mounts = [
            MountInfo {
                mount: "/main",
                key: Some("btrfs:a609a9a7"),
                file_system: "btrfs",
            },
            MountInfo {
                mount: "/export/MainMediaShare",
                key: Some("btrfs:a609a9a7"),
                file_system: "btrfs",
            },
            mount("/mnt/SSD1", "/dev/nvme0n1p1", "ext4"),
        ];
        assert_eq!(picked_mounts(&mounts), vec![("/main", 1), ("/mnt/SSD1", 0)]);
    }

    #[test]
    fn sample_turns_counters_into_rates() {
        let mut previous = Counters::new();
        previous.insert("disk:sda".into(), (1_000, 5_000));
        let sample = Sample {
            elapsed_secs: Some(2.0),
            previous: &previous,
        };
        let mut next = Counters::new();
        let rate = sample.rate("disk:sda", (3_000, 9_000), &mut next);
        assert_eq!(
            rate,
            Some(Rate {
                read: 1_000.0,
                write: 2_000.0
            })
        );
        assert_eq!(
            sample.rate("disk:sdb", (1, 1), &mut next),
            None,
            "first sight"
        );
        assert_eq!(next.len(), 2, "both counters kept for next time");
    }

    #[test]
    fn non_dev_names_have_no_kernel_name() {
        assert_eq!(kernel_name("Main Drive"), None);
        assert_eq!(kernel_name("overlay"), None);
    }

    #[test]
    fn volumes_sharing_a_windows_label_are_kept_apart() {
        let mounts = [
            mount("D:\\", "Backup", "NTFS"),
            mount("F:\\", "Backup", "NTFS"),
        ];
        assert_eq!(picked_mounts(&mounts), vec![("D:\\", 0), ("F:\\", 0)]);
    }
}
