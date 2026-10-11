use std::collections::HashMap;
use std::time::Instant;

use sysinfo::{Disk, DiskKind, Disks};

use crate::model::{Flow, Group, GroupKind, History, Member, Rate, Source};

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
/// For now every volume is a standalone group and no flows are reported.
/// Array detection and flow tracing come later.
pub struct DriveMonitor {
    disks: Disks,
    groups: Vec<Group>,
    /// Cumulative (read, written) byte counters from the previous sample.
    counters: HashMap<String, (u64, u64)>,
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
        let mut counters = HashMap::new();

        let disks = self.disks.list();
        let facts: Vec<[String; 3]> = disks
            .iter()
            .map(|d| {
                [
                    d.mount_point().display().to_string(),
                    d.name().to_string_lossy().into_owned(),
                    d.file_system().to_string_lossy().into_owned(),
                ]
            })
            .collect();
        let mounts: Vec<MountInfo> = facts
            .iter()
            .map(|[mount, device, file_system]| MountInfo {
                mount,
                device,
                file_system,
            })
            .collect();

        for (index, other_mounts) in pick_mounts(&mounts) {
            let disk = &disks[index];
            let id = facts[index][0].clone();
            let usage = disk.usage();
            let now = (usage.total_read_bytes, usage.total_written_bytes);

            let mut history = previous
                .remove(&id)
                .and_then(|g| g.members.into_iter().next())
                .map(|m| m.history)
                .unwrap_or_default();
            if let (Some(elapsed), Some(&(read, written))) = (elapsed_secs, self.counters.get(&id))
            {
                history.push(Rate {
                    read: per_second(read, now.0, elapsed),
                    write: per_second(written, now.1, elapsed),
                });
            }

            counters.insert(id.clone(), now);
            let mut group = group_for(disk, id, history);
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

/// The facts used to pick which mounts get a panel.
struct MountInfo<'a> {
    mount: &'a str,
    /// A device path such as `/dev/sdb` on Linux, or a volume label on Windows.
    device: &'a str,
    file_system: &'a str,
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
    let mut by_device: HashMap<&str, usize> = HashMap::new();
    for i in order {
        let device = mounts[i].device;
        // Only real device paths are merged. Windows reports volume labels
        // here, and two volumes can share a label.
        if device.starts_with("/dev/") {
            if let Some(&slot) = by_device.get(device) {
                picked[slot].1 += 1;
                continue;
            }
            by_device.insert(device, picked.len());
        }
        picked.push((i, 0));
    }
    picked
}

fn group_for(disk: &Disk, id: String, history: History) -> Group {
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

    fn mount<'a>(mount: &'a str, device: &'a str, file_system: &'a str) -> MountInfo<'a> {
        MountInfo {
            mount,
            device,
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
    fn volumes_sharing_a_windows_label_are_kept_apart() {
        let mounts = [
            mount("D:\\", "Backup", "NTFS"),
            mount("F:\\", "Backup", "NTFS"),
        ];
        assert_eq!(picked_mounts(&mounts), vec![("D:\\", 0), ("F:\\", 0)]);
    }
}
