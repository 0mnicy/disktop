//! What disktop shows, independent of where the numbers come from.
//!
//! A [`Source`] (real drives or the demo) produces [`Group`]s and [`Flow`]s.
//! A group is one panel on screen: either a standalone drive or an array with
//! its member disks. A flow is data moving from one group to another.

use std::collections::VecDeque;

/// How many samples each disk keeps for its graph.
const HISTORY_LEN: usize = 600;

/// Throughput over one sample interval, in bytes per second.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rate {
    pub read: f64,
    pub write: f64,
}

impl std::ops::Add for Rate {
    type Output = Rate;

    fn add(self, other: Rate) -> Rate {
        Rate {
            read: self.read + other.read,
            write: self.write + other.write,
        }
    }
}

impl std::iter::Sum for Rate {
    fn sum<I: Iterator<Item = Rate>>(iter: I) -> Rate {
        iter.fold(Rate::default(), |sum, rate| sum + rate)
    }
}

/// A rolling window of samples for one disk.
#[derive(Debug, Default, Clone)]
pub struct History {
    samples: VecDeque<Rate>,
}

impl History {
    pub fn push(&mut self, rate: Rate) {
        if self.samples.len() == HISTORY_LEN {
            self.samples.pop_front();
        }
        self.samples.push_back(rate);
    }

    /// The latest sample, or zero before the first one arrives.
    pub fn current(&self) -> Rate {
        self.samples.back().copied().unwrap_or_default()
    }

    /// The most recent `count` samples, oldest first.
    pub fn recent(&self, count: usize) -> impl Iterator<Item = Rate> + '_ {
        let skip = self.samples.len().saturating_sub(count);
        self.samples.iter().skip(skip).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// One disk with its own graph. A standalone drive has exactly one member;
/// an array has one per physical disk.
#[derive(Debug, Clone)]
pub struct Member {
    pub name: String,
    pub details: String,
    pub history: History,
}

/// How an array spreads data over its members. `copies` decides how flow
/// dots split when they reach the array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayLayout {
    /// Shown in the panel title, such as `RAID 10`.
    pub level: String,
    /// A few words on how data is stored, such as `2 copies, striped`.
    pub summary: String,
    /// How many disks each write lands on: every disk for a classic
    /// mirror, two for btrfs RAID 1 or 10, one for a stripe.
    pub copies: usize,
}

impl ArrayLayout {
    /// Classic RAID 1: every disk holds a full copy.
    pub fn mirror(disks: usize) -> Self {
        Self {
            level: "RAID 1".into(),
            summary: "mirror".into(),
            copies: disks,
        }
    }

    /// RAID 0: data is split across the disks with no copies.
    pub fn stripe() -> Self {
        Self {
            level: "RAID 0".into(),
            summary: "stripe".into(),
            copies: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKind {
    Standalone,
    Array(ArrayLayout),
}

/// One panel on screen.
#[derive(Debug, Clone)]
pub struct Group {
    /// Stable key that flows refer to, such as a mount point.
    pub id: String,
    /// Bold part of the panel title, such as `C:\` or `M:\  Media`.
    pub title: String,
    /// Dimmed facts after the title, such as the file system and disk type.
    pub details: Vec<String>,
    pub kind: GroupKind,
    pub total_space: u64,
    pub available_space: u64,
    pub members: Vec<Member>,
    /// The physical disks this group lives on, such as `nvme1n1` for both
    /// `/` and `/boot/efi`. Empty when unknown, in which case each member
    /// counts as its own disk.
    pub physical_disks: Vec<String>,
}

/// How many physical disks the groups sit on, counting a disk shared by
/// several groups (two partitions of one drive) once.
pub fn disk_count(groups: &[Group]) -> usize {
    let mut known: Vec<&str> = Vec::new();
    let mut unknown = 0;
    for group in groups {
        if group.physical_disks.is_empty() {
            unknown += group.members.len();
        } else {
            known.extend(group.physical_disks.iter().map(String::as_str));
        }
    }
    known.sort_unstable();
    known.dedup();
    known.len() + unknown
}

impl Group {
    pub fn used_space(&self) -> u64 {
        self.total_space.saturating_sub(self.available_space)
    }

    pub fn used_ratio(&self) -> f64 {
        if self.total_space == 0 {
            return 0.0;
        }
        self.used_space() as f64 / self.total_space as f64
    }

    /// Latest physical I/O summed over every member disk.
    pub fn current(&self) -> Rate {
        self.members.iter().map(|m| m.history.current()).sum()
    }

    pub fn is_array(&self) -> bool {
        matches!(self.kind, GroupKind::Array(_))
    }
}

/// Data moving from one group to another, attributed to a process.
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub from: String,
    pub to: String,
    /// Bytes per second.
    pub rate: f64,
    pub process: String,
}

/// Where the numbers come from: the real drives, or the scripted demo.
pub trait Source {
    /// Takes a new reading. Called once per sample interval.
    fn sample(&mut self);
    fn groups(&self) -> &[Group];
    fn flows(&self) -> &[Flow];

    /// Shown in the header so a demo is never mistaken for real data.
    fn badge(&self) -> Option<&str> {
        None
    }

    /// A caveat about which flows can be seen, shown in the footer.
    fn flow_note(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_the_newest_samples() {
        let mut history = History::default();
        for i in 0..HISTORY_LEN + 5 {
            history.push(Rate {
                read: i as f64,
                write: 0.0,
            });
        }
        assert_eq!(history.recent(usize::MAX).count(), HISTORY_LEN);
        assert_eq!(history.current().read, (HISTORY_LEN + 4) as f64);
        let last_two: Vec<f64> = history.recent(2).map(|r| r.read).collect();
        assert_eq!(
            last_two,
            vec![(HISTORY_LEN + 3) as f64, (HISTORY_LEN + 4) as f64]
        );
    }

    #[test]
    fn empty_history_reads_as_zero() {
        assert_eq!(History::default().current(), Rate::default());
    }

    #[test]
    fn disks_shared_by_groups_count_once() {
        let group = |disks: &[&str], members: usize| Group {
            id: String::new(),
            title: String::new(),
            details: vec![],
            kind: GroupKind::Standalone,
            total_space: 0,
            available_space: 0,
            members: (0..members)
                .map(|_| Member {
                    name: String::new(),
                    details: String::new(),
                    history: History::default(),
                })
                .collect(),
            physical_disks: disks.iter().map(|d| d.to_string()).collect(),
        };
        let groups = [
            // `/` and `/boot/efi` on one NVMe.
            group(&["nvme1n1"], 1),
            group(&["nvme1n1"], 1),
            group(&["sda", "sdb"], 2),
            // A volume whose disks aren't known counts its members.
            group(&[], 1),
        ];
        assert_eq!(disk_count(&groups), 4);
    }

    #[test]
    fn group_rate_sums_its_members() {
        let member = |read, write| {
            let mut history = History::default();
            history.push(Rate { read, write });
            Member {
                name: String::new(),
                details: String::new(),
                history,
            }
        };
        let group = Group {
            id: "m".into(),
            title: "M:\\".into(),
            details: vec![],
            kind: GroupKind::Array(ArrayLayout::mirror(2)),
            total_space: 100,
            available_space: 25,
            members: vec![member(1.0, 10.0), member(2.0, 10.0)],
            physical_disks: Vec::new(),
        };
        assert_eq!(
            group.current(),
            Rate {
                read: 3.0,
                write: 20.0
            }
        );
        assert_eq!(group.used_ratio(), 0.75);
    }
}
