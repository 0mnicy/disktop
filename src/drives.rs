use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use sysinfo::{Disk, DiskKind, Disks};

/// How many samples each drive keeps for its graph.
const HISTORY_LEN: usize = 600;

/// Throughput over one sample interval, in bytes per second.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rate {
    pub read: f64,
    pub write: f64,
}

#[derive(Debug)]
pub struct Drive {
    pub mount: PathBuf,
    pub label: String,
    pub file_system: String,
    pub kind: DiskKind,
    pub removable: bool,
    pub total_space: u64,
    pub available_space: u64,
    history: VecDeque<Rate>,
    total_read: u64,
    total_written: u64,
}

impl Drive {
    fn from_disk(disk: &Disk) -> Self {
        let usage = disk.usage();
        Self {
            mount: disk.mount_point().to_path_buf(),
            label: disk.name().to_string_lossy().into_owned(),
            file_system: disk.file_system().to_string_lossy().into_owned(),
            kind: disk.kind(),
            removable: disk.is_removable(),
            total_space: disk.total_space(),
            available_space: disk.available_space(),
            history: VecDeque::with_capacity(HISTORY_LEN),
            total_read: usage.total_read_bytes,
            total_written: usage.total_written_bytes,
        }
    }

    fn update(&mut self, disk: &Disk, elapsed_secs: f64) {
        let usage = disk.usage();
        if self.history.len() == HISTORY_LEN {
            self.history.pop_front();
        }
        self.history.push_back(Rate {
            read: per_second(self.total_read, usage.total_read_bytes, elapsed_secs),
            write: per_second(self.total_written, usage.total_written_bytes, elapsed_secs),
        });
        self.total_read = usage.total_read_bytes;
        self.total_written = usage.total_written_bytes;
        self.label = disk.name().to_string_lossy().into_owned();
        self.total_space = disk.total_space();
        self.available_space = disk.available_space();
    }

    pub fn used_space(&self) -> u64 {
        self.total_space.saturating_sub(self.available_space)
    }

    pub fn used_ratio(&self) -> f64 {
        if self.total_space == 0 {
            return 0.0;
        }
        self.used_space() as f64 / self.total_space as f64
    }

    /// The latest sample, or zero before the first one arrives.
    pub fn current(&self) -> Rate {
        self.history.back().copied().unwrap_or_default()
    }

    /// The most recent `count` samples, oldest first.
    pub fn recent(&self, count: usize) -> impl Iterator<Item = Rate> + '_ {
        let skip = self.history.len().saturating_sub(count);
        self.history.iter().skip(skip).copied()
    }

    pub fn has_samples(&self) -> bool {
        !self.history.is_empty()
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

pub struct DriveMonitor {
    disks: Disks,
    drives: Vec<Drive>,
    last_sample: Instant,
}

impl DriveMonitor {
    pub fn new() -> Self {
        let disks = Disks::new_with_refreshed_list();
        let mut drives: Vec<Drive> = disks.list().iter().map(Drive::from_disk).collect();
        drives.sort_by(|a, b| a.mount.cmp(&b.mount));
        Self {
            disks,
            drives,
            last_sample: Instant::now(),
        }
    }

    pub fn drives(&self) -> &[Drive] {
        &self.drives
    }

    /// Sum of the latest sample across all drives.
    pub fn total(&self) -> Rate {
        self.drives
            .iter()
            .map(Drive::current)
            .fold(Rate::default(), |sum, rate| Rate {
                read: sum.read + rate.read,
                write: sum.write + rate.write,
            })
    }

    /// Takes a new reading from every drive. Drives that were plugged in since
    /// the last sample are added, and drives that went away are dropped.
    pub fn sample(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_sample).as_secs_f64();
        self.last_sample = now;

        self.disks.refresh(true);
        let mut previous = std::mem::take(&mut self.drives);
        for disk in self.disks.list() {
            let existing = previous.iter().position(|d| d.mount == disk.mount_point());
            match existing {
                Some(index) => {
                    let mut drive = previous.swap_remove(index);
                    drive.update(disk, elapsed);
                    self.drives.push(drive);
                }
                None => self.drives.push(Drive::from_disk(disk)),
            }
        }
        self.drives.sort_by(|a, b| a.mount.cmp(&b.mount));
    }
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
}
