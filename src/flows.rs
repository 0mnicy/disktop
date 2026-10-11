//! Dots that carry data along each flow's route.
//!
//! Every dot stands for the same amount of data across all flows, like the
//! scale on a map: a flow showing twice as many dots as another is moving
//! twice as much data. The amount is picked automatically from round sizes
//! (1, 2, 5, 10, 20, 50 … KiB, MiB, GiB) so the busiest flow shows a readable
//! number of dots, or it can be fixed with `--dot-size`. All dots move at the
//! same speed, so how closely they're packed is what shows throughput.
//!
//! Positions are distances in cells along a route; the UI maps them to the
//! screen with the current layout. When a flow stops, its dots keep going
//! until they reach the end, so a stream drains out instead of vanishing.

use std::collections::VecDeque;

use crate::model::Flow;

/// How fast every dot moves, in cells per second.
pub const SPEED: f64 = 16.0;
/// The automatic scale keeps the busiest flow's dots per second in this
/// band, and only changes when it drifts out, so the scale doesn't flicker.
const BUSIEST_MIN_DOTS: f64 = 6.0;
const BUSIEST_MAX_DOTS: f64 = 24.0;
/// The automatic scale before any flow has been seen.
const DEFAULT_DOT_SIZE: f64 = 1024.0 * 1024.0;
/// Most dots one route emits per step, so a tiny fixed dot size on a fast
/// copy can't bog the app down. The excess is dropped.
const MAX_DOTS_PER_STEP: usize = 200;
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
    /// Bytes per second; the last value while draining.
    pub rate: f64,
    pub process: String,
    /// Index into the UI's palette. Stays the same for the life of the route.
    pub color: usize,
    /// False once the flow has stopped and its dots are draining out.
    pub running: bool,
    /// Oldest (farthest along) first.
    dots: VecDeque<Dot>,
    /// Bytes moved since the last dot was emitted.
    pending: f64,
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

#[derive(Debug)]
pub struct FlowField {
    routes: Vec<Route>,
    /// Bytes each dot stands for.
    dot_size: f64,
    /// True when the user set the dot size; otherwise it follows the flows.
    fixed: bool,
}

impl FlowField {
    /// A field whose dot size is `dot_size` bytes, or picked automatically
    /// from the flows when `None`.
    pub fn new(dot_size: Option<f64>) -> Self {
        Self {
            routes: Vec::new(),
            dot_size: dot_size.unwrap_or(DEFAULT_DOT_SIZE),
            fixed: dot_size.is_some(),
        }
    }

    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    /// Bytes each dot stands for.
    pub fn dot_size(&self) -> f64 {
        self.dot_size
    }

    pub fn is_fixed(&self) -> bool {
        self.fixed
    }

    /// True when nothing is moving.
    pub fn is_idle(&self) -> bool {
        self.routes.is_empty()
    }

    /// Starts, updates and stops routes to match the latest flows, and
    /// rescales the dots if the busiest flow has changed a lot.
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
                        // A full dot's worth, so the first one appears straight away.
                        pending: self.dot_size,
                        next_id: 0,
                        length: None,
                    });
                }
            }
        }

        let busiest = self
            .routes
            .iter()
            .filter(|r| r.running)
            .map(|r| r.rate)
            .fold(0.0, f64::max);
        if !self.fixed && busiest > 0.0 {
            self.dot_size = pick_dot_size(busiest, self.dot_size);
        }
        self.prune();
    }

    /// Moves every dot forward by `dt` seconds and emits a dot for each
    /// dot size worth of data each flow moved.
    pub fn step(&mut self, dt: f64) {
        let dot_size = self.dot_size;
        for route in &mut self.routes {
            for dot in &mut route.dots {
                dot.distance += SPEED * dt;
            }
            if route.running && route.rate > 0.0 {
                route.pending += route.rate * dt;
                let mut emitted = 0;
                while route.pending >= dot_size && emitted < MAX_DOTS_PER_STEP {
                    route.pending -= dot_size;
                    // This dot's data finished arriving `pending / rate`
                    // seconds ago, so it has already moved that far.
                    route.dots.push_back(Dot {
                        id: route.next_id,
                        distance: SPEED * route.pending / route.rate,
                    });
                    route.next_id += 1;
                    emitted += 1;
                }
                if emitted == MAX_DOTS_PER_STEP {
                    route.pending %= dot_size;
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

/// Round dot sizes, smallest first: 1, 2, 5, 10, 20, 50, 100, 200, 500 of
/// each binary unit from KiB to TiB.
fn round_sizes() -> impl Iterator<Item = f64> {
    (1..=4).flat_map(|power| {
        let unit = 1024f64.powi(power);
        [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0]
            .into_iter()
            .map(move |step| step * unit)
    })
}

/// Keeps `current` while the busiest flow shows a comfortable number of
/// dots per second with it. Otherwise picks the smallest round size that
/// brings the busiest flow under the top of the band; since round sizes are
/// at most 2.5× apart, that also keeps it above the bottom.
fn pick_dot_size(busiest: f64, current: f64) -> f64 {
    if (BUSIEST_MIN_DOTS..=BUSIEST_MAX_DOTS).contains(&(busiest / current)) {
        return current;
    }
    round_sizes()
        .find(|size| busiest / size <= BUSIEST_MAX_DOTS)
        .unwrap_or_else(|| round_sizes().last().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * KIB;
    const GIB: f64 = 1024.0 * MIB;

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

    fn emitted(field: &FlowField, route: usize) -> u64 {
        field.routes()[route].next_id
    }

    #[test]
    fn each_dot_is_a_fixed_amount_of_data() {
        let mut field = FlowField::new(Some(MIB));
        field.sync(&[flow("a", "b", 100.0), flow("c", "d", 50.0)]);
        field.set_lengths(&[Some(1000.0), Some(1000.0)]);
        run(&mut field, 2.0);
        // One dot per MiB, plus the first one emitted straight away. Adding
        // up 200 small steps can leave the last dot a hair short.
        assert!(
            (200..=201).contains(&emitted(&field, 0)),
            "{}",
            emitted(&field, 0)
        );
        assert!(
            (100..=101).contains(&emitted(&field, 1)),
            "{}",
            emitted(&field, 1)
        );
    }

    #[test]
    fn dots_move_at_one_speed_whatever_the_rate() {
        let mut field = FlowField::new(Some(MIB));
        field.sync(&[flow("a", "b", 1.0), flow("c", "d", 500.0)]);
        field.set_lengths(&[Some(1000.0), Some(1000.0)]);
        run(&mut field, 1.0);
        for route in field.routes() {
            let first = route.dots().find(|d| d.id == 0).unwrap();
            assert!((first.distance - SPEED).abs() < 0.2, "{}", first.distance);
        }
    }

    #[test]
    fn dots_are_evenly_spaced_by_rate() {
        let mut field = FlowField::new(Some(MIB));
        field.sync(&[flow("a", "b", 8.0)]);
        field.set_lengths(&[Some(1000.0)]);
        run(&mut field, 3.0);
        let distances: Vec<f64> = field.routes()[0].dots().map(|d| d.distance).collect();
        // 8 dots a second at 16 cells a second: one every 2 cells.
        for pair in distances.windows(2) {
            assert!((pair[0] - pair[1] - 2.0).abs() < 0.01, "{pair:?}");
        }
    }

    #[test]
    fn automatic_scale_picks_a_round_size_for_the_busiest_flow() {
        let size_for = |mib_per_sec: f64| {
            let mut field = FlowField::new(None);
            // A slower flow alongside doesn't change the scale.
            field.sync(&[
                flow("a", "b", mib_per_sec),
                flow("c", "d", mib_per_sec / 10.0),
            ]);
            field.dot_size()
        };
        assert_eq!(size_for(600.0), 50.0 * MIB, "12 dots/s");
        assert_eq!(size_for(1.6 * 1024.0), 100.0 * MIB, "16 dots/s");
        assert_eq!(size_for(40.0), 2.0 * MIB, "20 dots/s");
        assert_eq!(size_for(0.001), KIB, "nothing smaller than 1 KiB");
    }

    #[test]
    fn automatic_scale_holds_steady_for_small_changes() {
        let mut field = FlowField::new(None);
        field.sync(&[flow("a", "b", 600.0)]);
        assert_eq!(field.dot_size(), 50.0 * MIB);
        field.sync(&[flow("a", "b", 1000.0)]);
        assert_eq!(
            field.dot_size(),
            50.0 * MIB,
            "20 dots/s is still in the band"
        );
        field.sync(&[flow("a", "b", 1300.0)]);
        assert_eq!(field.dot_size(), 100.0 * MIB, "26 dots/s is too many");
        field.sync(&[]);
        assert_eq!(field.dot_size(), 100.0 * MIB, "no flows, no change");
    }

    #[test]
    fn every_round_size_keeps_the_busiest_flow_in_the_band() {
        let mut rate = 50.0 * KIB;
        while rate < 2.0 * 1024.0 * GIB {
            let dots = rate / pick_dot_size(rate, 1.0);
            assert!(
                (BUSIEST_MIN_DOTS..=BUSIEST_MAX_DOTS).contains(&dots),
                "{rate}: {dots}"
            );
            rate *= 1.13;
        }
    }

    #[test]
    fn fixed_dot_size_never_changes() {
        let mut field = FlowField::new(Some(MIB));
        field.sync(&[flow("a", "b", 2000.0)]);
        assert_eq!(field.dot_size(), MIB);
        assert!(field.is_fixed());
    }

    #[test]
    fn a_tiny_dot_size_on_a_fast_copy_is_capped() {
        let mut field = FlowField::new(Some(1.0));
        field.sync(&[flow("a", "b", 2000.0)]);
        field.set_lengths(&[Some(10.0)]);
        field.step(0.1);
        assert!(emitted(&field, 0) <= MAX_DOTS_PER_STEP as u64);
        assert!(field.routes()[0].pending < 1.0);
    }

    #[test]
    fn dots_past_the_end_are_dropped() {
        let mut field = FlowField::new(Some(MIB));
        field.sync(&[flow("a", "b", 100.0)]);
        field.set_lengths(&[Some(10.0)]);
        run(&mut field, 3.0);
        assert!(field.routes()[0].dots().all(|d| d.distance <= 10.0));
        assert!(field.routes()[0].head() <= 10.0);
    }

    #[test]
    fn stopped_flow_drains_then_disappears() {
        let mut field = FlowField::new(Some(MIB));
        field.sync(&[flow("a", "b", 20.0)]);
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
        let mut field = FlowField::new(None);
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
        let mut field = FlowField::new(None);
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
        let mut field = FlowField::new(None);
        field.sync(&[flow("a", "missing", 100.0)]);
        run(&mut field, 0.5);
        field.sync(&[]);
        assert!(field.is_idle());
    }
}
