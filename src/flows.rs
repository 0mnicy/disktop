//! Dots that march along each flow's route.
//!
//! Every flow emits dots at an even spacing, and they move along its route
//! at a speed that grows with throughput: a fast copy shows tightly packed,
//! quick dots and a trickle shows a few slow ones. Positions are distances in
//! cells along the route; the UI maps them to the screen with the current
//! layout. When a flow stops, its dots keep going until they reach the end,
//! so a stream drains out instead of vanishing.

use std::collections::VecDeque;

use crate::model::Flow;

const MIB: f64 = 1024.0 * 1024.0;

/// Dots never travel farther than this, even if a route's length is unknown.
const MAX_DISTANCE: f64 = 2000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dot {
    /// Increases by one per dot, so a dot keeps the same branch where its route splits.
    pub id: u64,
    /// Cells travelled from the start of the route.
    pub distance: f64,
}

/// A flow between two groups, kept until its last dot has arrived.
#[derive(Debug, Clone)]
pub struct Route {
    pub from: String,
    pub to: String,
    /// Bytes per second. Keeps its last value while draining so dots don't stall.
    pub rate: f64,
    pub process: String,
    /// Index into the UI's palette. Stays the same for the life of the route.
    pub color: usize,
    /// False once the flow has stopped and its dots are draining out.
    pub running: bool,
    /// Oldest (farthest along) first.
    dots: VecDeque<Dot>,
    /// Cells travelled since the last dot was emitted.
    since_last: f64,
    next_id: u64,
    /// Route length on screen at the last frame.
    length: Option<f64>,
}

impl Route {
    pub fn dots(&self) -> impl Iterator<Item = &Dot> {
        self.dots.iter()
    }

    /// How far the leading dot has got.
    pub fn head(&self) -> f64 {
        self.dots.front().map_or(0.0, |d| d.distance)
    }

    /// Where the stream begins: the start while running, or the last dot while draining.
    pub fn tail(&self) -> f64 {
        if self.running {
            0.0
        } else {
            self.dots.back().map_or(f64::INFINITY, |d| d.distance)
        }
    }
}

#[derive(Debug, Default)]
pub struct FlowField {
    routes: Vec<Route>,
}

impl FlowField {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    /// True when nothing is moving.
    pub fn is_idle(&self) -> bool {
        self.routes.is_empty()
    }

    /// Starts, updates and stops routes to match the latest flows.
    pub fn sync(&mut self, flows: &[Flow]) {
        for route in &mut self.routes {
            route.running = false;
        }
        for flow in flows {
            let existing = self
                .routes
                .iter_mut()
                .find(|r| r.from == flow.from && r.to == flow.to);
            match existing {
                Some(route) => {
                    route.rate = flow.rate;
                    route.process.clone_from(&flow.process);
                    route.running = true;
                }
                None => {
                    let color = self.free_color();
                    self.routes.push(Route {
                        from: flow.from.clone(),
                        to: flow.to.clone(),
                        rate: flow.rate,
                        process: flow.process.clone(),
                        color,
                        running: true,
                        dots: VecDeque::new(),
                        // Emit the first dot straight away.
                        since_last: spacing(flow.rate),
                        next_id: 0,
                        length: None,
                    });
                }
            }
        }
        self.prune();
    }

    /// Moves every dot forward by `dt` seconds and emits new ones.
    pub fn step(&mut self, dt: f64) {
        for route in &mut self.routes {
            let moved = speed(route.rate) * dt;
            for dot in &mut route.dots {
                dot.distance += moved;
            }
            if route.running {
                route.since_last += moved;
                let gap = spacing(route.rate);
                while route.since_last >= gap {
                    route.since_last -= gap;
                    route.dots.push_back(Dot {
                        id: route.next_id,
                        distance: route.since_last,
                    });
                    route.next_id += 1;
                }
            }
            let end = route.length.unwrap_or(MAX_DISTANCE).min(MAX_DISTANCE);
            while route.dots.front().is_some_and(|d| d.distance > end) {
                route.dots.pop_front();
            }
        }
        self.prune();
    }

    /// Records how long each route is on screen, in the order of
    /// [`FlowField::routes`], so dots past the end can be dropped.
    pub fn set_lengths(&mut self, lengths: &[Option<f64>]) {
        for (route, length) in self.routes.iter_mut().zip(lengths) {
            route.length = *length;
        }
    }

    /// Drops stopped routes that have nothing left to show.
    fn prune(&mut self) {
        self.routes
            .retain(|r| r.running || (!r.dots.is_empty() && r.length.is_some()));
    }

    /// The lowest palette index no other route is using.
    fn free_color(&self) -> usize {
        (0..)
            .find(|c| self.routes.iter().all(|r| r.color != *c))
            .unwrap()
    }
}

/// Cells per second. Faster flows move faster, on a log scale so a 2 GB/s
/// copy is quicker than a 2 MB/s one without being a blur.
fn speed(bytes_per_sec: f64) -> f64 {
    let mib = (bytes_per_sec / MIB).max(0.01);
    (10.0 + 5.0 * mib.log10()).clamp(6.0, 28.0)
}

/// Cells between dots. Faster flows pack them tighter.
fn spacing(bytes_per_sec: f64) -> f64 {
    let mib = (bytes_per_sec / MIB).max(0.01);
    (8.0 - 2.0 * mib.log10()).clamp(2.5, 8.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(from: &str, to: &str, mib_per_sec: f64) -> Flow {
        Flow {
            from: from.into(),
            to: to.into(),
            rate: mib_per_sec * MIB,
            process: "copy".into(),
        }
    }

    fn run(field: &mut FlowField, secs: f64) {
        for _ in 0..(secs / 0.01).round() as usize {
            field.step(0.01);
        }
    }

    #[test]
    fn faster_flows_move_faster_and_pack_tighter() {
        assert!(speed(600.0 * MIB) > speed(40.0 * MIB));
        assert!(spacing(600.0 * MIB) < spacing(40.0 * MIB));
        assert_eq!(speed(0.0), 6.0);
        assert_eq!(spacing(0.0), 8.0);
        assert_eq!(spacing(1e15), 2.5);
    }

    #[test]
    fn dots_are_evenly_spaced() {
        let mut field = FlowField::new();
        field.sync(&[flow("a", "b", 100.0)]);
        field.set_lengths(&[Some(500.0)]);
        run(&mut field, 2.0);

        let route = &field.routes()[0];
        let distances: Vec<f64> = route.dots().map(|d| d.distance).collect();
        assert!(distances.len() > 5);
        for pair in distances.windows(2) {
            let gap = pair[0] - pair[1];
            assert!((gap - spacing(route.rate)).abs() < 0.01, "gap {gap}");
        }
        let ids: Vec<u64> = route.dots().map(|d| d.id).collect();
        assert!(ids.windows(2).all(|w| w[1] == w[0] + 1));
    }

    #[test]
    fn dots_past_the_end_are_dropped() {
        let mut field = FlowField::new();
        field.sync(&[flow("a", "b", 100.0)]);
        field.set_lengths(&[Some(10.0)]);
        run(&mut field, 3.0);
        assert!(field.routes()[0].dots().all(|d| d.distance <= 10.0));
        assert!(field.routes()[0].head() <= 10.0);
    }

    #[test]
    fn stopped_flow_drains_then_disappears() {
        let mut field = FlowField::new();
        field.sync(&[flow("a", "b", 100.0)]);
        field.set_lengths(&[Some(30.0)]);
        run(&mut field, 1.0);
        let dots_before = field.routes()[0].dots().count();

        field.sync(&[]);
        assert_eq!(field.routes().len(), 1, "kept while dots are in flight");
        assert!(!field.routes()[0].running);
        run(&mut field, 0.2);
        let route = &field.routes()[0];
        assert!(route.dots().count() <= dots_before, "no new dots");
        assert!(route.tail() > 0.0, "the stream's start moves away");

        run(&mut field, 5.0);
        assert!(field.is_idle());
    }

    #[test]
    fn restarted_flow_keeps_its_route() {
        let mut field = FlowField::new();
        field.sync(&[flow("a", "b", 100.0)]);
        field.set_lengths(&[Some(100.0)]);
        run(&mut field, 0.5);
        field.sync(&[]);
        run(&mut field, 0.1);
        field.sync(&[flow("a", "b", 50.0)]);
        assert_eq!(field.routes().len(), 1);
        assert!(field.routes()[0].running);
        assert_eq!(field.routes()[0].tail(), 0.0);
    }

    #[test]
    fn new_routes_take_the_lowest_free_color() {
        let mut field = FlowField::new();
        field.sync(&[flow("a", "b", 1.0), flow("b", "c", 1.0)]);
        let colors: Vec<usize> = field.routes().iter().map(|r| r.color).collect();
        assert_eq!(colors, vec![0, 1]);

        // a→b stops and drains away; its color is free for the next flow.
        field.sync(&[flow("b", "c", 1.0)]);
        field.set_lengths(&[Some(5.0), Some(5.0)]);
        run(&mut field, 3.0);
        field.sync(&[flow("b", "c", 1.0), flow("c", "a", 1.0)]);
        let colors: Vec<(String, usize)> = field
            .routes()
            .iter()
            .map(|r| (r.from.clone(), r.color))
            .collect();
        assert_eq!(colors, vec![("b".into(), 1), ("c".into(), 0)]);
    }

    #[test]
    fn stopped_route_with_no_known_length_is_dropped() {
        let mut field = FlowField::new();
        field.sync(&[flow("a", "missing", 100.0)]);
        run(&mut field, 0.5);
        field.sync(&[]);
        assert!(field.is_idle());
    }
}
