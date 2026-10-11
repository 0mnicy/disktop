//! A scripted demo with made-up arrays and copies, so the layout and the flow
//! animation can be seen without admin rights or RAID hardware.

use std::time::Instant;

use crate::model::{ArrayLevel, Flow, Group, GroupKind, History, Member, Rate, Source};

const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = 1024.0 * MIB;
const TB: u64 = 1_000_000_000_000;

/// The script repeats every this many seconds.
const LOOP_SECS: f64 = 60.0;
/// How long a copy takes to ramp up to full speed, and back down.
const RAMP_SECS: f64 = 1.5;
/// Samples of made-up history to start with, so graphs aren't empty.
const PREFILL_SAMPLES: u32 = 60;

struct ScriptedFlow {
    from: &'static str,
    to: &'static str,
    start: f64,
    end: f64,
    rate: f64,
    process: &'static str,
}

const SCRIPT: [ScriptedFlow; 4] = [
    ScriptedFlow {
        from: "D:\\",
        to: "M:\\",
        start: 0.0,
        end: 24.0,
        rate: 600.0 * MIB,
        process: "robocopy.exe",
    },
    ScriptedFlow {
        from: "C:\\",
        to: "E:\\",
        start: 6.0,
        end: 30.0,
        rate: 40.0 * MIB,
        process: "explorer.exe",
    },
    ScriptedFlow {
        from: "S:\\",
        to: "D:\\",
        start: 22.0,
        end: 44.0,
        rate: 1.6 * GIB,
        process: "ffmpeg.exe",
    },
    ScriptedFlow {
        from: "M:\\",
        to: "S:\\",
        start: 36.0,
        end: 58.0,
        rate: 350.0 * MIB,
        process: "resolve.exe",
    },
];

#[derive(Clone, Copy)]
enum Direction {
    Read,
    Write,
}

pub struct Demo {
    groups: Vec<Group>,
    flows: Vec<Flow>,
    started: Instant,
    rng: u64,
}

impl Demo {
    pub fn new() -> Self {
        let mut demo = Self {
            groups: demo_groups(),
            flows: Vec::new(),
            started: Instant::now(),
            rng: 0x9E37_79B9_7F4A_7C15,
        };
        // Replay the end of the previous loop so the graphs start full.
        for i in (1..=PREFILL_SAMPLES).rev() {
            demo.advance_to(LOOP_SECS - f64::from(i));
        }
        demo.advance_to(0.0);
        demo
    }

    /// Sets the flows and records disk rates for `t` seconds into the script.
    pub(crate) fn advance_to(&mut self, t: f64) {
        let t = t.rem_euclid(LOOP_SECS);

        let mut flows = Vec::new();
        for scripted in &SCRIPT {
            let level = ramp(t, scripted.start, scripted.end);
            if level > 0.0 {
                flows.push(Flow {
                    from: scripted.from.to_string(),
                    to: scripted.to.to_string(),
                    rate: scripted.rate * level * self.jitter(),
                    process: scripted.process.to_string(),
                });
            }
        }

        let mut rates: Vec<Vec<Rate>> = self
            .groups
            .iter()
            .map(|g| vec![Rate::default(); g.members.len()])
            .collect();
        for flow in &flows {
            if let Some(i) = self.groups.iter().position(|g| g.id == flow.from) {
                spread(
                    self.groups[i].kind,
                    &mut rates[i],
                    flow.rate,
                    Direction::Read,
                );
            }
            if let Some(i) = self.groups.iter().position(|g| g.id == flow.to) {
                spread(
                    self.groups[i].kind,
                    &mut rates[i],
                    flow.rate,
                    Direction::Write,
                );
            }
        }

        // Background chatter: the system drive is always a little busy.
        let chatter: Vec<f64> = self
            .groups
            .iter()
            .map(|g| if g.id == "C:\\" { 6.0 * MIB } else { 0.3 * MIB })
            .collect();
        for (busy, group_rates) in chatter.into_iter().zip(&mut rates) {
            for rate in group_rates.iter_mut() {
                rate.read += busy * self.noise();
                rate.write += busy * self.noise();
            }
        }

        for (group, group_rates) in self.groups.iter_mut().zip(rates) {
            for (member, rate) in group.members.iter_mut().zip(group_rates) {
                member.history.push(rate);
            }
        }
        self.flows = flows;
    }

    /// A random number in 0..1 (xorshift64*).
    fn next_random(&mut self) -> f64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let bits = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        bits as f64 / (1u64 << 53) as f64
    }

    /// A factor near 1 so copy speeds wobble like real ones.
    fn jitter(&mut self) -> f64 {
        0.9 + 0.2 * self.next_random()
    }

    /// Mostly quiet with the occasional burst.
    fn noise(&mut self) -> f64 {
        self.next_random().powi(4)
    }
}

impl Source for Demo {
    fn sample(&mut self) {
        let t = self.started.elapsed().as_secs_f64();
        self.advance_to(t);
    }

    fn groups(&self) -> &[Group] {
        &self.groups
    }

    fn flows(&self) -> &[Flow] {
        &self.flows
    }

    fn badge(&self) -> Option<&str> {
        Some("demo")
    }
}

/// How far a copy running from `start` to `end` is ramped up at time `t`,
/// from 0 (not running) to 1 (full speed).
fn ramp(t: f64, start: f64, end: f64) -> f64 {
    if t < start || t >= end {
        return 0.0;
    }
    let edge = ((t - start) / RAMP_SECS)
        .min((end - t) / RAMP_SECS)
        .min(1.0);
    edge * edge * (3.0 - 2.0 * edge)
}

/// Adds a group-level transfer to its member disks the way the array lays
/// data out: a mirror writes everything to every disk, a stripe splits it.
fn spread(kind: GroupKind, members: &mut [Rate], rate: f64, direction: Direction) {
    let count = members.len().max(1) as f64;
    let per_member = match (kind, direction) {
        (GroupKind::Standalone, _) => rate,
        (GroupKind::Array(ArrayLevel::Mirror), Direction::Write) => rate,
        (GroupKind::Array(_), _) => rate / count,
    };
    for member in members {
        match direction {
            Direction::Read => member.read += per_member,
            Direction::Write => member.write += per_member,
        }
    }
}

fn demo_groups() -> Vec<Group> {
    vec![
        standalone("C:\\", &["System", "NTFS", "SSD"], TB, 0.62),
        standalone("D:\\", &["Games", "NTFS", "SSD"], 2 * TB, 0.71),
        array(
            "M:\\",
            "Media",
            "ReFS",
            ArrayLevel::Mirror,
            8 * TB,
            0.54,
            &[
                ("Disk 2", "WD Red Plus 8 TB"),
                ("Disk 3", "WD Red Plus 8 TB"),
            ],
        ),
        array(
            "S:\\",
            "Scratch",
            "NTFS",
            ArrayLevel::Stripe,
            4 * TB,
            0.23,
            &[
                ("Disk 4", "Samsung 990 Pro 2 TB"),
                ("Disk 5", "Samsung 990 Pro 2 TB"),
            ],
        ),
        standalone("E:\\", &["Backup", "exFAT", "removable"], TB, 0.40),
    ]
}

fn standalone(mount: &str, details: &[&str], total: u64, used: f64) -> Group {
    Group {
        id: mount.to_string(),
        title: mount.to_string(),
        details: details.iter().map(|d| d.to_string()).collect(),
        kind: GroupKind::Standalone,
        total_space: total,
        available_space: (total as f64 * (1.0 - used)) as u64,
        members: vec![Member {
            name: String::new(),
            details: String::new(),
            history: History::default(),
        }],
    }
}

fn array(
    mount: &str,
    label: &str,
    file_system: &str,
    level: ArrayLevel,
    total: u64,
    used: f64,
    disks: &[(&str, &str)],
) -> Group {
    Group {
        id: mount.to_string(),
        title: format!("{mount}  {label}"),
        details: vec![level.label().to_string(), file_system.to_string()],
        kind: GroupKind::Array(level),
        total_space: total,
        available_space: (total as f64 * (1.0 - used)) as u64,
        members: disks
            .iter()
            .map(|(name, model)| Member {
                name: name.to_string(),
                details: model.to_string(),
                history: History::default(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member_rates(demo: &Demo, id: &str) -> Vec<Rate> {
        demo.groups
            .iter()
            .find(|g| g.id == id)
            .unwrap()
            .members
            .iter()
            .map(|m| m.history.current())
            .collect()
    }

    #[test]
    fn ramp_is_zero_outside_and_full_in_the_middle() {
        assert_eq!(ramp(-1.0, 0.0, 10.0), 0.0);
        assert_eq!(ramp(10.0, 0.0, 10.0), 0.0);
        assert_eq!(ramp(5.0, 0.0, 10.0), 1.0);
        let rising = ramp(0.5, 0.0, 10.0);
        assert!(rising > 0.0 && rising < 1.0);
    }

    #[test]
    fn script_flows_match_the_timeline() {
        let mut demo = Demo::new();
        demo.advance_to(10.0);
        let routes: Vec<(&str, &str)> = demo
            .flows
            .iter()
            .map(|f| (f.from.as_str(), f.to.as_str()))
            .collect();
        assert_eq!(routes, vec![("D:\\", "M:\\"), ("C:\\", "E:\\")]);

        demo.advance_to(59.5);
        assert!(demo.flows.is_empty());
    }

    #[test]
    fn mirror_writes_the_full_rate_to_every_disk() {
        let mut demo = Demo::new();
        demo.advance_to(10.0);
        let source_read = member_rates(&demo, "D:\\")[0].read;
        for disk in member_rates(&demo, "M:\\") {
            assert!(
                (disk.write - source_read).abs() < 5.0 * MIB,
                "{disk:?} vs {source_read}"
            );
        }
    }

    #[test]
    fn stripe_reads_are_split_across_disks() {
        let mut demo = Demo::new();
        demo.advance_to(30.0);
        let target_write = member_rates(&demo, "D:\\")[0].write;
        let striped: f64 = member_rates(&demo, "S:\\").iter().map(|r| r.read).sum();
        assert!((striped - target_write).abs() < 5.0 * MIB);
        for disk in member_rates(&demo, "S:\\") {
            assert!(disk.read < target_write * 0.6);
        }
    }

    #[test]
    fn graphs_start_with_history() {
        let demo = Demo::new();
        let history = &demo.groups[0].members[0].history;
        assert_eq!(
            history.recent(usize::MAX).count(),
            PREFILL_SAMPLES as usize + 1
        );
    }
}
