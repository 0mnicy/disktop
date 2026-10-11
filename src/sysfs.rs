//! Linux storage details that sysinfo doesn't give: btrfs filesystems that
//! span several disks, per-disk I/O counters, and disk models.
//!
//! Everything is read from sysfs and procfs, which needs no root. Paths are
//! rooted at a parameter so tests can use a fake tree. On other systems the
//! paths don't exist and every function comes back empty.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::format;
use crate::model::ArrayLayout;

/// Data profiles btrfs names under `allocation/data`.
const BTRFS_PROFILES: [&str; 9] = [
    "raid0", "raid1", "raid1c3", "raid1c4", "raid10", "raid5", "raid6", "dup", "single",
];

/// A btrfs filesystem and the block devices it lives on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Btrfs {
    pub uuid: String,
    pub label: String,
    /// Kernel device names such as `sda`, in natural order.
    pub devices: Vec<String>,
    /// Data profile such as `raid10`, if the kernel reports one.
    pub profile: Option<String>,
}

/// Every mounted btrfs filesystem, from `<sys>/fs/btrfs`.
pub fn btrfs_filesystems(sys: &Path) -> Vec<Btrfs> {
    let Ok(entries) = fs::read_dir(sys.join("fs/btrfs")) else {
        return Vec::new();
    };
    let mut found: Vec<Btrfs> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let devices = names_in(&dir.join("devices"))?;
            Some(Btrfs {
                uuid: entry.file_name().to_string_lossy().into_owned(),
                label: read_trimmed(&dir.join("label")).unwrap_or_default(),
                devices,
                profile: data_profile(&dir.join("allocation/data")),
            })
        })
        .collect();
    found.sort_by(|a, b| a.uuid.cmp(&b.uuid));
    found
}

/// The data profile with the most space allocated. During a conversion
/// btrfs can have two at once; the bigger one is what most data uses.
fn data_profile(dir: &Path) -> Option<String> {
    BTRFS_PROFILES
        .iter()
        .filter_map(|&profile| {
            let path = dir.join(profile);
            path.is_dir().then(|| {
                let bytes = read_trimmed(&path.join("total_bytes"))
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                (bytes, profile)
            })
        })
        .max_by_key(|&(bytes, _)| bytes)
        .map(|(_, profile)| profile.to_string())
}

/// How a btrfs data profile lays data out over its disks.
pub fn btrfs_layout(profile: Option<&str>) -> ArrayLayout {
    let (level, summary, copies) = match profile {
        Some("raid0") => ("RAID 0", "striped", 1),
        Some("raid1") => ("RAID 1", "2 copies", 2),
        Some("raid1c3") => ("RAID 1C3", "3 copies", 3),
        Some("raid1c4") => ("RAID 1C4", "4 copies", 4),
        Some("raid10") => ("RAID 10", "2 copies, striped", 2),
        Some("raid5") => ("RAID 5", "striped with parity", 1),
        Some("raid6") => ("RAID 6", "striped with 2 parity", 1),
        Some("dup") => ("DUP", "duplicated", 1),
        Some("single") => ("single", "spanned", 1),
        Some(other) => return custom_layout(other),
        None => ("btrfs", "multi-disk", 1),
    };
    ArrayLayout {
        level: level.to_string(),
        summary: summary.to_string(),
        copies,
    }
}

fn custom_layout(profile: &str) -> ArrayLayout {
    ArrayLayout {
        level: profile.to_uppercase(),
        summary: "multi-disk".to_string(),
        copies: 1,
    }
}

/// Cumulative (bytes read, bytes written) per device, from `/proc/diskstats`.
/// The kernel counts in 512-byte sectors whatever the disk's real sector size.
pub fn parse_diskstats(text: &str) -> HashMap<String, (u64, u64)> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let name = fields.get(2)?;
            let read = fields.get(5)?.parse::<u64>().ok()?;
            let written = fields.get(9)?.parse::<u64>().ok()?;
            Some((name.to_string(), (read * 512, written * 512)))
        })
        .collect()
}

/// Model and size of a block device, such as `HGST HMS5C4040BL · 3.6 TiB`.
/// For a partition the model comes from the disk it's on.
pub fn device_details(sys: &Path, device: &str) -> String {
    let class = sys.join("class/block");
    let disk = parent_disk(&class, device);
    let mut details = Vec::new();
    if let Some(model) = read_trimmed(&class.join(&disk).join("device/model"))
        && !model.is_empty()
    {
        details.push(model);
    }
    if let Some(sectors) =
        read_trimmed(&class.join(device).join("size")).and_then(|v| v.parse::<u64>().ok())
    {
        details.push(format::bytes((sectors * 512) as f64));
    }
    details.join(" · ")
}

/// Whether a block device spins, from its disk's `queue/rotational`.
pub fn is_rotational(sys: &Path, device: &str) -> Option<bool> {
    let class = sys.join("class/block");
    let disk = parent_disk(&class, device);
    read_trimmed(&class.join(disk).join("queue/rotational")).map(|v| v == "1")
}

/// The physical disks a block device sits on: the disk itself, the disk
/// under a partition, or the disks under a device-mapper device such as an
/// LVM or encrypted volume. A multipath device is one disk however many
/// paths it has, so it's named by its multipath name (`mpathg`).
pub fn physical_disks(sys: &Path, device: &str) -> Vec<String> {
    let class = sys.join("class/block");
    let mut disks = Vec::new();
    collect_disks(&class, device, &mut disks, 0);
    disks.sort();
    disks.dedup();
    disks
}

fn collect_disks(class: &Path, device: &str, disks: &mut Vec<String>, depth: usize) {
    // Device-mapper stacks are shallow; this only guards against a loop.
    if depth > 8 {
        return;
    }
    let dir = class.join(device);
    if read_trimmed(&dir.join("dm/uuid")).is_some_and(|uuid| uuid.starts_with("mpath-")) {
        disks.push(read_trimmed(&dir.join("dm/name")).unwrap_or_else(|| device.to_string()));
        return;
    }
    match names_in(&dir.join("slaves")) {
        Some(slaves) if !slaves.is_empty() => {
            for slave in slaves {
                collect_disks(class, &slave, disks, depth + 1);
            }
        }
        _ => disks.push(parent_disk(class, device)),
    }
}

/// The whole disk a device belongs to: itself for a disk, or the disk a
/// partition sits on (`sda1` → `sda`, `nvme0n1p2` → `nvme0n1`).
fn parent_disk(class: &Path, device: &str) -> String {
    let path = class.join(device);
    if !path.join("partition").exists() {
        return device.to_string();
    }
    // In sysfs a partition's directory sits inside its disk's directory,
    // and the disk's name starts the partition's (`sda` → `sda1`).
    let resolved = fs::canonicalize(&path).ok().and_then(|p| {
        let parent = p.parent()?.file_name()?.to_string_lossy().into_owned();
        (parent.len() < device.len() && device.starts_with(&parent)).then_some(parent)
    });
    if let Some(parent) = resolved {
        return parent;
    }
    let trimmed = device.trim_end_matches(|c: char| c.is_ascii_digit());
    match trimmed.strip_suffix('p') {
        Some(disk) if disk.ends_with(|c: char| c.is_ascii_digit()) => disk.to_string(),
        _ => trimmed.to_string(),
    }
}

/// Names in a directory, in natural order (`sdb` before `sdaa`), or `None`
/// when it can't be read.
fn names_in(dir: &Path) -> Option<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    Some(names)
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A throwaway directory tree standing in for `/sys`.
    struct FakeSys(PathBuf);

    impl FakeSys {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("disktop-sysfs-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn file(&self, path: &str, contents: &str) -> &Self {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
            self
        }

        fn dir(&self, path: &str) -> &Self {
            fs::create_dir_all(self.0.join(path)).unwrap();
            self
        }
    }

    impl Drop for FakeSys {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The god-server's array: btrfs RAID 10 over eight disks.
    fn raid10(sys: &FakeSys) {
        let fs = "fs/btrfs/a609a9a7";
        sys.file(&format!("{fs}/label"), "raid10hhds\n");
        for disk in ["sdh", "sda", "sdc", "sdb", "sde", "sdd", "sdg", "sdf"] {
            sys.file(&format!("{fs}/devices/{disk}"), "");
        }
        sys.file(
            &format!("{fs}/allocation/data/raid10/total_bytes"),
            "11282879086592\n",
        );
        sys.file(
            &format!("{fs}/allocation/data/total_bytes"),
            "11282879086592\n",
        );
        sys.dir("fs/btrfs/features");
    }

    #[test]
    fn finds_a_multi_disk_btrfs_filesystem() {
        let sys = FakeSys::new("raid10");
        raid10(&sys);
        let found = btrfs_filesystems(&sys.0);
        assert_eq!(found.len(), 1, "the features directory is not a filesystem");
        let fs = &found[0];
        assert_eq!(fs.label, "raid10hhds");
        assert_eq!(fs.profile.as_deref(), Some("raid10"));
        assert_eq!(
            fs.devices,
            vec!["sda", "sdb", "sdc", "sdd", "sde", "sdf", "sdg", "sdh"]
        );
    }

    #[test]
    fn picks_the_bigger_profile_during_a_conversion() {
        let sys = FakeSys::new("convert");
        sys.file("data/raid1/total_bytes", "100\n");
        sys.file("data/raid10/total_bytes", "900\n");
        assert_eq!(data_profile(&sys.0.join("data")).as_deref(), Some("raid10"));
    }

    #[test]
    fn no_btrfs_means_nothing_found() {
        let sys = FakeSys::new("empty");
        assert!(btrfs_filesystems(&sys.0).is_empty());
        assert!(btrfs_filesystems(Path::new("/definitely/not/here")).is_empty());
    }

    #[test]
    fn profiles_map_to_copies() {
        let copies = |p| btrfs_layout(Some(p)).copies;
        assert_eq!(copies("raid10"), 2);
        assert_eq!(copies("raid1"), 2);
        assert_eq!(copies("raid1c3"), 3);
        assert_eq!(copies("raid0"), 1);
        assert_eq!(btrfs_layout(Some("raid10")).level, "RAID 10");
        assert_eq!(btrfs_layout(Some("raid7")).level, "RAID7");
        assert_eq!(btrfs_layout(None).copies, 1);
    }

    #[test]
    fn diskstats_are_read_in_bytes() {
        let text = "\
   8       0 sda 72592 64681 11363472 1874670 421114 206094 23779408 7060859 1 4048542 10683721 0 0 0 0 94785 1748191
 259       1 nvme0n1p1 10 0 80 1 5 0 40 1 0 2 2
   short line
";
        let stats = parse_diskstats(text);
        assert_eq!(stats["sda"], (11363472 * 512, 23779408 * 512));
        assert_eq!(stats["nvme0n1p1"], (80 * 512, 40 * 512));
        assert_eq!(stats.len(), 2);
    }

    #[test]
    fn device_details_show_model_and_size() {
        let sys = FakeSys::new("details");
        sys.file("class/block/sda/device/model", "HGST HMS5C4040BL  \n")
            .file("class/block/sda/size", "7814037168\n")
            .file("class/block/sda/queue/rotational", "1\n");
        assert_eq!(device_details(&sys.0, "sda"), "HGST HMS5C4040BL · 3.6 TiB");
        assert_eq!(is_rotational(&sys.0, "sda"), Some(true));
        assert_eq!(device_details(&sys.0, "sdz"), "");
    }

    #[test]
    fn physical_disks_see_through_partitions_and_multipath() {
        // The god-server's layout: two partitions on one NVMe, and an ext4
        // partition on a multipath disk reached through sdi.
        let sys = FakeSys::new("physical");
        sys.file("class/block/nvme1n1p1/partition", "1\n")
            .file("class/block/nvme1n1p2/partition", "2\n")
            .file("class/block/dm-1/dm/uuid", "mpath-35000cca26a2b6fd4\n")
            .file("class/block/dm-1/dm/name", "mpathg\n")
            .file("class/block/dm-1/slaves/sdi", "")
            .file(
                "class/block/dm-2/dm/uuid",
                "part1-mpath-35000cca26a2b6fd4\n",
            )
            .file("class/block/dm-2/dm/name", "mpathg-part1\n")
            .file("class/block/dm-2/slaves/dm-1", "")
            .dir("class/block/sda");
        let disks = |device| physical_disks(&sys.0, device);
        assert_eq!(disks("nvme1n1p1"), vec!["nvme1n1"]);
        assert_eq!(disks("nvme1n1p2"), vec!["nvme1n1"]);
        assert_eq!(disks("dm-2"), vec!["mpathg"]);
        assert_eq!(disks("sda"), vec!["sda"]);
    }

    #[test]
    fn physical_disks_of_a_volume_spanning_two_disks() {
        // An LVM volume over partitions on two disks.
        let sys = FakeSys::new("lvm");
        sys.file("class/block/dm-0/dm/uuid", "LVM-abc\n")
            .file("class/block/dm-0/slaves/sdb1", "")
            .file("class/block/dm-0/slaves/sdc1", "")
            .file("class/block/sdb1/partition", "1\n")
            .file("class/block/sdc1/partition", "1\n");
        assert_eq!(physical_disks(&sys.0, "dm-0"), vec!["sdb", "sdc"]);
    }

    #[test]
    fn partitions_take_their_disks_model() {
        let sys = FakeSys::new("partition");
        sys.file(
            "class/block/nvme0n1/device/model",
            "Samsung SSD 970 EVO 1TB\n",
        )
        .file("class/block/nvme0n1/queue/rotational", "0\n")
        .file("class/block/nvme0n1p1/partition", "1\n")
        .file("class/block/nvme0n1p1/size", "1953521664\n")
        .file("class/block/sdj1/partition", "1\n");
        assert_eq!(
            device_details(&sys.0, "nvme0n1p1"),
            "Samsung SSD 970 EVO 1TB · 931.5 GiB"
        );
        assert_eq!(is_rotational(&sys.0, "nvme0n1p1"), Some(false));
        assert_eq!(parent_disk(&sys.0.join("class/block"), "sdj1"), "sdj");
    }
}
