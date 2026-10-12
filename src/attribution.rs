//! Turning what one process moved into flows, the same way on every system.
//!
//! The system-specific tracers (`/proc` on Linux, ETW on Windows) work out
//! how many bytes a process read from and wrote to each group, and how much
//! its network received and sent. From that:
//!
//! - Reading one group while writing another is a copy, at the smaller of
//!   the two totals, split by how much each group took part.
//! - What it wrote beyond the copies came from the network, up to what it
//!   received: a download.
//! - What it read beyond the copies went out, up to what it sent: an upload.
//!
//! Reading and writing the same group is a database or a log rather than a
//! copy between drives, so it isn't a flow.

use crate::model::{Flow, NETWORK};

/// Flows slower than this aren't shown, so logs and databases ticking over
/// don't clutter the screen.
pub const MIN_FLOW_RATE: f64 = 512.0 * 1024.0;

/// What one process moved during a sample, in bytes per second.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ProcessIo {
    /// Read from each group.
    pub reads: Vec<(String, f64)>,
    /// Written to each group.
    pub writes: Vec<(String, f64)>,
    /// Received from other machines.
    pub received: f64,
    /// Sent to other machines.
    pub sent: f64,
}

impl ProcessIo {
    /// Whether this process moved enough to be worth working out.
    pub fn is_busy(&self) -> bool {
        let read: f64 = self.reads.iter().map(|(_, r)| r).sum();
        let written: f64 = self.writes.iter().map(|(_, w)| w).sum();
        read.max(written) >= MIN_FLOW_RATE
    }
}

/// Adds the flows a process made to `flows`.
pub fn add_process_flows(flows: &mut Vec<Flow>, process: &str, io: &ProcessIo) {
    let read: f64 = io.reads.iter().map(|(_, r)| r).sum();
    let written: f64 = io.writes.iter().map(|(_, w)| w).sum();

    let mut copied = 0.0;
    let copy_rate = read.min(written);
    if copy_rate >= MIN_FLOW_RATE {
        for (from, from_rate) in &io.reads {
            for (to, to_rate) in &io.writes {
                if from != to {
                    let rate = copy_rate * (from_rate / read) * (to_rate / written);
                    copied += rate;
                    add_flow(flows, from.clone(), to.clone(), rate, process);
                }
            }
        }
    }

    let downloaded = (written - copied).min(io.received);
    if downloaded >= MIN_FLOW_RATE {
        for (to, to_rate) in &io.writes {
            let rate = downloaded * to_rate / written;
            add_flow(flows, NETWORK.into(), to.clone(), rate, process);
        }
    }
    let uploaded = (read - copied).min(io.sent);
    if uploaded >= MIN_FLOW_RATE {
        for (from, from_rate) in &io.reads {
            let rate = uploaded * from_rate / read;
            add_flow(flows, from.clone(), NETWORK.into(), rate, process);
        }
    }
}

/// Adds a process's share of a flow, merging with other processes moving
/// data between the same two groups.
pub fn add_flow(flows: &mut Vec<Flow>, from: String, to: String, rate: f64, process: &str) {
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

/// Drops flows that ended up too slow to show, fastest first.
pub fn finish(mut flows: Vec<Flow>) -> Vec<Flow> {
    flows.retain(|f| f.rate >= MIN_FLOW_RATE);
    flows.sort_by(|a, b| b.rate.total_cmp(&a.rate));
    flows
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: f64 = 1024.0 * 1024.0;

    fn io(reads: &[(&str, f64)], writes: &[(&str, f64)], received: f64, sent: f64) -> ProcessIo {
        let groups = |side: &[(&str, f64)]| {
            side.iter()
                .map(|(group, mib)| (group.to_string(), mib * MIB))
                .collect()
        };
        ProcessIo {
            reads: groups(reads),
            writes: groups(writes),
            received: received * MIB,
            sent: sent * MIB,
        }
    }

    fn flows_of(io: &ProcessIo) -> Vec<(String, String, f64)> {
        let mut flows = Vec::new();
        add_process_flows(&mut flows, "p", io);
        finish(flows)
            .into_iter()
            .map(|f| (f.from, f.to, f.rate / MIB))
            .collect()
    }

    #[test]
    fn a_copy_between_two_drives_is_one_flow() {
        let flows = flows_of(&io(&[("/main", 100.0)], &[("/mnt/a", 100.0)], 0.0, 0.0));
        assert_eq!(flows, vec![("/main".into(), "/mnt/a".into(), 100.0)]);
    }

    #[test]
    fn a_copy_is_split_by_how_much_each_drive_took_part() {
        let flows = flows_of(&io(
            &[("/main", 75.0), ("/mnt/SSD1", 25.0)],
            &[("/", 100.0)],
            0.0,
            0.0,
        ));
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
        assert!(flows_of(&io(&[("/main", 100.0)], &[("/main", 100.0)], 0.0, 0.0)).is_empty());
    }

    #[test]
    fn writes_beyond_a_copy_are_a_download_up_to_what_was_received() {
        // A sync program receiving 50 and sending 30, all on one drive.
        let flows = flows_of(&io(&[("/main", 30.0)], &[("/main", 50.0)], 80.0, 30.0));
        assert_eq!(
            flows,
            vec![
                (NETWORK.into(), "/main".into(), 50.0),
                ("/main".into(), NETWORK.into(), 30.0)
            ]
        );
        // Writing a database while barely using the network isn't a download.
        assert!(flows_of(&io(&[], &[("/main", 50.0)], 0.01, 0.0)).is_empty());
    }

    #[test]
    fn a_copy_is_not_also_counted_as_network_traffic() {
        let flows = flows_of(&io(&[("/main", 80.0)], &[("/", 80.0)], 80.0, 80.0));
        assert_eq!(flows, vec![("/main".into(), "/".into(), 80.0)]);
    }

    #[test]
    fn processes_moving_data_between_the_same_groups_merge() {
        let mut flows = Vec::new();
        add_flow(&mut flows, "a".into(), "b".into(), 10.0, "rsync");
        add_flow(&mut flows, "a".into(), "b".into(), 5.0, "Sonarr");
        add_flow(&mut flows, "a".into(), "b".into(), 1.0, "rsync");
        add_flow(&mut flows, "b".into(), "a".into(), 2.0, "cp");
        assert_eq!(flows.len(), 2);
        assert_eq!(flows[0].rate, 16.0);
        assert_eq!(flows[0].process, "rsync, Sonarr");
    }

    #[test]
    fn busy_means_moving_enough_either_way() {
        assert!(io(&[("/main", 1.0)], &[], 0.0, 0.0).is_busy());
        assert!(!io(&[("/main", 0.1)], &[("/", 0.1)], 9.0, 9.0).is_busy());
    }
}
