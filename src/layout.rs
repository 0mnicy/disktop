//! Where each panel goes, and the routes flows follow between them.
//!
//! Panels sit in one or two columns with a narrow channel down the middle,
//! or down the right edge when there's only one column. Every panel has an
//! edge on the channel, so a flow can leave any panel, run along the channel,
//! and enter any other panel without crossing a third.
//!
//! To keep flows apart, each one gets its own port on every panel edge it
//! touches, its own lane in the channel, and its own strip inside an array.

use std::collections::HashMap;

use ratatui::layout::{Constraint, Layout, Margin, Rect};

use crate::model::{Group, GroupKind};

/// Width of the channel between the columns, in cells. Also the most lanes
/// it holds before flows have to share.
pub const CHANNEL_WIDTH: u16 = 6;
/// Width of the strip inside an array where flows reach its disks.
const BUS_WIDTH: u16 = 3;
/// Two columns only when each can be at least this wide.
const MIN_COLUMN_WIDTH: u16 = 44;
/// Rows an array uses above its member disks: capacity and status.
pub const ARRAY_HEADER_ROWS: u16 = 2;
/// Rough heights used to balance the columns.
const STANDALONE_ROWS: u16 = 7;
const MEMBER_ROWS: u16 = 6;
/// The network panel has no capacity bar, so it needs a row less.
const NETWORK_ROWS: u16 = 6;
/// How much of each right-angle turn is cut off, in cells.
const CORNER: f64 = 0.8;

/// The edge of a panel that faces the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct Panel {
    /// Index into the group list.
    pub group: usize,
    pub area: Rect,
    pub channel_side: Side,
}

#[derive(Debug, Clone, Default)]
pub struct Board {
    pub panels: Vec<Panel>,
    pub channel: Rect,
}

/// Inside an array panel: the header rows, one box per member disk, and the
/// strip between the disks and the channel-side border.
#[derive(Debug, Clone)]
pub struct ArrayParts {
    pub header: Rect,
    pub members: Vec<Rect>,
    bus: Rect,
}

impl ArrayParts {
    /// The x of the `slot`th strip lane, one per flow touching the array.
    fn bus_lane(&self, slot: usize) -> f64 {
        let lanes = usize::from(self.bus.width.max(1));
        f64::from(self.bus.x) + 0.5 + (slot % lanes) as f64
    }
}

/// A point in terminal cells, `y` pointing down.
pub type Point = (f64, f64);

/// A polyline that can be walked at constant speed.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    points: Vec<Point>,
    /// Distance from the start to each point.
    distances: Vec<f64>,
}

impl Path {
    pub fn new(points: Vec<Point>) -> Self {
        let mut distances = Vec::with_capacity(points.len());
        let mut total = 0.0;
        for (i, point) in points.iter().enumerate() {
            if i > 0 {
                total += distance(points[i - 1], *point);
            }
            distances.push(total);
        }
        Self { points, distances }
    }

    pub fn length(&self) -> f64 {
        self.distances.last().copied().unwrap_or(0.0)
    }

    /// The point `s` cells from the start, clamped to the ends.
    pub fn at(&self, s: f64) -> Point {
        let (Some(&first), Some(&last)) = (self.points.first(), self.points.last()) else {
            return (0.0, 0.0);
        };
        if s <= 0.0 {
            return first;
        }
        if s >= self.length() {
            return last;
        }
        let i = self.distances.partition_point(|&d| d < s).max(1);
        let (a, b) = (self.points[i - 1], self.points[i]);
        let segment = self.distances[i] - self.distances[i - 1];
        let t = if segment > 0.0 {
            (s - self.distances[i - 1]) / segment
        } else {
            0.0
        };
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    }

    /// Which way the path runs `s` cells from the start: the step from the
    /// start of that segment to its end.
    pub fn heading(&self, s: f64) -> Point {
        if self.points.len() < 2 {
            return (0.0, 0.0);
        }
        let i = self
            .distances
            .partition_point(|&d| d < s)
            .clamp(1, self.points.len() - 1);
        let (a, b) = (self.points[i - 1], self.points[i]);
        (b.0 - a.0, b.1 - a.1)
    }

    #[cfg(test)]
    pub fn start(&self) -> Point {
        self.at(0.0)
    }

    #[cfg(test)]
    pub fn end(&self) -> Point {
        self.at(f64::INFINITY)
    }
}

/// A flow to route: the groups it runs between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteEnds {
    pub from: usize,
    pub to: usize,
    /// Member disks to fan out from, or 0 for a standalone source.
    pub from_disks: usize,
    /// Member disks to fan into, or 0 for a standalone target.
    pub to_disks: usize,
    /// How many of the target's disks each dot lands on. See [`branches`].
    pub target_copies: usize,
}

/// The full shape of one flow on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutePaths {
    /// From each source disk to the source panel's port; empty for a standalone source.
    pub sources: Vec<Path>,
    /// From the source port, along the flow's lane, to the target port.
    pub trunk: Path,
    /// From the target port to each target disk; empty for a standalone target.
    pub targets: Vec<Path>,
    pub target_copies: usize,
}

impl RoutePaths {
    fn source_length(&self) -> f64 {
        self.sources.iter().map(Path::length).fold(0.0, f64::max)
    }

    fn target_length(&self) -> f64 {
        self.targets.iter().map(Path::length).fold(0.0, f64::max)
    }

    /// Distance from the farthest source disk to the farthest target disk.
    pub fn length(&self) -> f64 {
        self.source_length() + self.trunk.length() + self.target_length()
    }

    /// Where a point `s` cells along the route is drawn. Where the route
    /// branches, `dot` picks the branch a moving dot takes; `None` returns
    /// every branch, for drawing the track.
    pub fn points_at(&self, s: f64, dot: Option<u64>) -> Vec<Point> {
        self.headings_at(s, dot)
            .into_iter()
            .map(|(point, _)| point)
            .collect()
    }

    /// Where the route begins: at each source disk, or at the source
    /// panel's port.
    pub fn starts(&self) -> Vec<Point> {
        if self.sources.is_empty() {
            vec![self.trunk.at(0.0)]
        } else {
            self.sources.iter().map(|p| p.at(0.0)).collect()
        }
    }

    /// Where the route ends, at each target disk or at the target panel's
    /// port, with which way it's heading there.
    pub fn finishes(&self) -> Vec<(Point, Point)> {
        let end = |p: &Path| (p.at(p.length()), p.heading(p.length()));
        if self.targets.is_empty() {
            vec![end(&self.trunk)]
        } else {
            self.targets.iter().map(end).collect()
        }
    }

    /// Like [`RoutePaths::points_at`], with which way the route runs at
    /// each point (see [`Path::heading`]).
    pub fn headings_at(&self, s: f64, dot: Option<u64>) -> Vec<(Point, Point)> {
        let source = self.source_length();
        let trunk = self.trunk.length();
        let on = |path: &Path, t: f64| (path.at(t), path.heading(t));
        if s < source {
            // Measure back from the port so every branch meets the trunk together.
            let before_port = source - s;
            branches(&self.sources, 1, dot)
                .filter(|p| before_port <= p.length())
                .map(|p| on(p, p.length() - before_port))
                .collect()
        } else if s <= source + trunk {
            vec![on(&self.trunk, s - source)]
        } else {
            let after_port = s - source - trunk;
            branches(&self.targets, self.target_copies, dot)
                .filter(|p| after_port <= p.length())
                .map(|p| on(p, after_port))
                .collect()
        }
    }
}

/// The branches a dot takes where a route fans out to an array's disks.
/// Each dot lands on `copies` neighbouring disks, starting one group further
/// along than the dot before, so a two-copy array of eight disks fills
/// disks 0–1, then 2–3, and so on. With `copies` at least the disk count,
/// or with no dot (drawing the track), every branch is returned.
fn branches(paths: &[Path], copies: usize, dot: Option<u64>) -> impl Iterator<Item = &Path> {
    let count = paths.len();
    let copies = copies.max(1);
    let first = match dot {
        Some(id) if count > 0 && copies < count => {
            Some((id % count as u64) as usize * copies % count)
        }
        _ => None,
    };
    paths
        .iter()
        .enumerate()
        .filter(move |(i, _)| first.is_none_or(|f| (i + count - f) % count < copies))
        .map(|(_, p)| p)
}

/// Lays the groups out in `area`.
pub fn arrange(area: Rect, groups: &[Group]) -> Board {
    if groups.is_empty() || area.is_empty() {
        return Board::default();
    }

    let two_columns = groups.len() >= 2 && area.width >= 2 * MIN_COLUMN_WIDTH + CHANNEL_WIDTH;
    let (columns, channel) = if two_columns {
        let [left, channel, right] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(CHANNEL_WIDTH),
            Constraint::Fill(1),
        ])
        .areas(area);
        (vec![(left, Side::Right), (right, Side::Left)], channel)
    } else {
        let [column, channel] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(CHANNEL_WIDTH)])
                .areas(area);
        (vec![(column, Side::Right)], channel)
    };

    // Greedily put each group in the column with the least height so far.
    let mut assigned: Vec<Vec<usize>> = vec![Vec::new(); columns.len()];
    let mut heights = vec![0u32; columns.len()];
    for (index, group) in groups.iter().enumerate() {
        let column = (0..columns.len()).min_by_key(|&c| heights[c]).unwrap();
        assigned[column].push(index);
        heights[column] += u32::from(rows_wanted(group));
    }

    let mut panels = Vec::new();
    for ((column, side), indices) in columns.into_iter().zip(assigned) {
        let constraints = indices
            .iter()
            .map(|&i| Constraint::Fill(rows_wanted(&groups[i])));
        let areas = Layout::vertical(constraints).split(column);
        for (&group, &area) in indices.iter().zip(areas.iter()) {
            panels.push(Panel {
                group,
                area,
                channel_side: side,
            });
        }
    }
    panels.sort_by_key(|p| p.group);

    Board { panels, channel }
}

fn rows_wanted(group: &Group) -> u16 {
    match group.kind {
        GroupKind::Array(_) => 2 + ARRAY_HEADER_ROWS + MEMBER_ROWS * group.members.len() as u16,
        GroupKind::Network => NETWORK_ROWS,
        GroupKind::Standalone => STANDALONE_ROWS,
    }
}

/// Splits an array panel into its header, member boxes and bus strip.
pub fn array_parts(area: Rect, members: usize, side: Side) -> ArrayParts {
    let inner = area.inner(Margin::new(1, 1));
    let [header, body] =
        Layout::vertical([Constraint::Length(ARRAY_HEADER_ROWS), Constraint::Fill(1)]).areas(inner);
    let (stack, bus) = match side {
        Side::Right => {
            let [stack, bus] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(BUS_WIDTH)])
                    .areas(body);
            (stack, bus)
        }
        Side::Left => {
            let [bus, stack] =
                Layout::horizontal([Constraint::Length(BUS_WIDTH), Constraint::Fill(1)])
                    .areas(body);
            (stack, bus)
        }
    };
    let members = Layout::vertical(vec![Constraint::Fill(1); members])
        .split(stack)
        .to_vec();
    ArrayParts {
        header,
        members,
        bus,
    }
}

/// A route's end on a panel edge.
struct Port {
    point: Point,
    /// Order among the ports on the same panel, used to pick a strip lane.
    slot: usize,
}

impl Board {
    pub fn panel(&self, group: usize) -> Option<&Panel> {
        self.panels.iter().find(|p| p.group == group)
    }

    /// Works out a separate path for each route. The result lines up with
    /// `routes`; a route whose groups aren't on the board gets `None`.
    pub fn plan(&self, routes: &[Option<RouteEnds>]) -> Vec<Option<RoutePaths>> {
        let ports = self.assign_ports(routes);
        let lanes = self.assign_lanes(routes, &ports);

        routes
            .iter()
            .enumerate()
            .map(|(i, ends)| {
                let ends = ends.as_ref()?;
                let start = ports.get(&(i, true))?;
                let end = ports.get(&(i, false))?;
                let lane = *lanes.get(&i)?;
                let trunk = rounded(vec![
                    start.point,
                    (lane, start.point.1),
                    (lane, end.point.1),
                    end.point,
                ]);
                let sources = self
                    .fan(ends.from, ends.from_disks, start)
                    .into_iter()
                    .map(|path| reverse(&path))
                    .collect();
                let targets = self.fan(ends.to, ends.to_disks, end);
                Some(RoutePaths {
                    sources,
                    trunk,
                    targets,
                    target_copies: ends.target_copies,
                })
            })
            .collect()
    }

    /// Spreads the routes touching each panel along its channel-side edge,
    /// ordered by where the other end is so neighbouring routes don't cross.
    fn assign_ports(&self, routes: &[Option<RouteEnds>]) -> HashMap<(usize, bool), Port> {
        let middle = |group: usize| {
            self.panel(group)
                .map(|p| f64::from(p.area.y) + f64::from(p.area.height) / 2.0)
        };
        // (route, is_source, where the other end is) for each panel.
        let mut ends: HashMap<usize, Vec<(usize, bool, f64)>> = HashMap::new();
        for (i, route) in routes.iter().enumerate() {
            let Some(route) = route else { continue };
            let (Some(from_y), Some(to_y)) = (middle(route.from), middle(route.to)) else {
                continue;
            };
            ends.entry(route.from).or_default().push((i, true, to_y));
            ends.entry(route.to).or_default().push((i, false, from_y));
        }

        let mut ports = HashMap::new();
        for (group, mut list) in ends {
            let Some(panel) = self.panel(group) else {
                continue;
            };
            list.sort_by(|a, b| a.2.total_cmp(&b.2).then(a.0.cmp(&b.0)).then(b.1.cmp(&a.1)));
            let top = f64::from(panel.area.y) + 1.0;
            let rows = f64::from(panel.area.height.saturating_sub(2).max(1));
            let count = list.len() as f64;
            let x = edge_x(panel.area, panel.channel_side);
            for (slot, (route, is_source, _)) in list.into_iter().enumerate() {
                let row = (rows * (slot as f64 + 1.0) / (count + 1.0)).floor();
                ports.insert(
                    (route, is_source),
                    Port {
                        point: (x, top + row.min(rows - 1.0) + 0.5),
                        slot,
                    },
                );
            }
        }
        ports
    }

    /// Gives each route a lane in the channel. Routes whose vertical spans
    /// overlap get different lanes; routes that don't overlap may share.
    fn assign_lanes(
        &self,
        routes: &[Option<RouteEnds>],
        ports: &HashMap<(usize, bool), Port>,
    ) -> HashMap<usize, f64> {
        let mut spans: Vec<(usize, f64, f64)> = (0..routes.len())
            .filter_map(|i| {
                let a = ports.get(&(i, true))?.point.1;
                let b = ports.get(&(i, false))?.point.1;
                Some((i, a.min(b), a.max(b)))
            })
            .collect();
        // Longest first among routes starting together, so it takes the outer lane.
        spans.sort_by(|a, b| {
            a.1.total_cmp(&b.1)
                .then((b.2 - b.1).total_cmp(&(a.2 - a.1)))
        });

        let max_lanes = usize::from(self.channel.width.max(1));
        let mut lane_bottoms: Vec<f64> = Vec::new();
        let mut lane_of = HashMap::new();
        for (route, top, bottom) in spans {
            let free = lane_bottoms.iter().position(|&b| b + 1.0 < top);
            let lane = match free {
                Some(lane) => lane,
                None if lane_bottoms.len() < max_lanes => {
                    lane_bottoms.push(f64::NEG_INFINITY);
                    lane_bottoms.len() - 1
                }
                None => (0..lane_bottoms.len())
                    .min_by(|&a, &b| lane_bottoms[a].total_cmp(&lane_bottoms[b]))
                    .unwrap(),
            };
            lane_bottoms[lane] = lane_bottoms[lane].max(bottom);
            lane_of.insert(route, lane);
        }

        let count = lane_bottoms.len().max(1) as f64;
        let center = f64::from(self.channel.x) + f64::from(self.channel.width) / 2.0;
        let step = if count > 1.0 {
            ((f64::from(self.channel.width) - 1.0) / (count - 1.0)).min(1.0)
        } else {
            0.0
        };
        lane_of
            .into_iter()
            .map(|(route, lane)| (route, center + (lane as f64 - (count - 1.0) / 2.0) * step))
            .collect()
    }

    /// Paths from an array's port to each of its disks, through the strip
    /// lane that belongs to this port. Empty for a standalone panel.
    fn fan(&self, group: usize, disks: usize, port: &Port) -> Vec<Path> {
        let Some(panel) = self.panel(group) else {
            return Vec::new();
        };
        if disks == 0 {
            return Vec::new();
        }
        let parts = array_parts(panel.area, disks, panel.channel_side);
        let lane = parts.bus_lane(port.slot);
        parts
            .members
            .iter()
            .map(|disk| {
                let target = (
                    edge_x(*disk, panel.channel_side),
                    f64::from(disk.y) + f64::from(disk.height) / 2.0,
                );
                rounded(vec![
                    port.point,
                    (lane, port.point.1),
                    (lane, target.1),
                    target,
                ])
            })
            .collect()
    }
}

/// The center of the border column on the channel side.
fn edge_x(area: Rect, side: Side) -> f64 {
    match side {
        Side::Right => f64::from(area.right()) - 0.5,
        Side::Left => f64::from(area.x) + 0.5,
    }
}

fn distance(a: Point, b: Point) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

/// The point `d` cells from `from` toward `to`.
fn toward(from: Point, to: Point, d: f64) -> Point {
    let length = distance(from, to);
    if length == 0.0 {
        return from;
    }
    (
        from.0 + (to.0 - from.0) * d / length,
        from.1 + (to.1 - from.1) * d / length,
    )
}

/// Builds a path from right-angled corner points, cutting each corner so
/// turns read as curves.
fn rounded(corners: Vec<Point>) -> Path {
    let mut points: Vec<Point> = Vec::with_capacity(corners.len() * 2);
    for (i, &corner) in corners.iter().enumerate() {
        let (Some(&before), Some(&after)) = (
            i.checked_sub(1).and_then(|j| corners.get(j)),
            corners.get(i + 1),
        ) else {
            points.push(corner);
            continue;
        };
        let cut = CORNER
            .min(distance(before, corner) / 2.0)
            .min(distance(corner, after) / 2.0);
        if cut > 0.0 {
            points.push(toward(corner, before, cut));
            points.push(toward(corner, after, cut));
        } else {
            points.push(corner);
        }
    }
    points.dedup();
    Path::new(points)
}

fn reverse(path: &Path) -> Path {
    let mut points = path.points.clone();
    points.reverse();
    Path::new(points)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArrayLayout, GroupKind, History, Member};

    fn group(kind: GroupKind, members: usize) -> Group {
        Group {
            id: String::new(),
            title: String::new(),
            details: vec![],
            kind,
            total_space: 0,
            available_space: 0,
            members: (0..members)
                .map(|_| Member {
                    name: String::new(),
                    details: String::new(),
                    history: History::default(),
                })
                .collect(),
            physical_disks: Vec::new(),
        }
    }

    fn mixed_groups() -> Vec<Group> {
        vec![
            group(GroupKind::Standalone, 1),
            group(GroupKind::Standalone, 1),
            group(GroupKind::Array(ArrayLayout::mirror(2)), 2),
            group(GroupKind::Array(ArrayLayout::stripe()), 2),
            group(GroupKind::Standalone, 1),
        ]
    }

    fn standalone(from: usize, to: usize) -> Option<RouteEnds> {
        Some(RouteEnds {
            from,
            to,
            from_disks: 0,
            to_disks: 0,
            target_copies: 1,
        })
    }

    fn touches_channel(panel: &Panel, channel: Rect) -> bool {
        match panel.channel_side {
            Side::Right => panel.area.right() == channel.x,
            Side::Left => panel.area.x == channel.right(),
        }
    }

    fn inside(point: Point, area: Rect) -> bool {
        point.0 >= f64::from(area.x)
            && point.0 <= f64::from(area.right())
            && point.1 >= f64::from(area.y)
            && point.1 <= f64::from(area.bottom())
    }

    #[test]
    fn wide_terminal_gets_two_columns_around_the_channel() {
        let area = Rect::new(0, 1, 160, 40);
        let board = arrange(area, &mixed_groups());
        assert_eq!(board.panels.len(), 5);
        assert_eq!(board.channel.width, CHANNEL_WIDTH);
        assert!(board.channel.x > 40 && board.channel.right() < 120);
        assert!(board.panels.iter().any(|p| p.channel_side == Side::Left));
        assert!(board.panels.iter().any(|p| p.channel_side == Side::Right));
        for panel in &board.panels {
            assert!(touches_channel(panel, board.channel), "{panel:?}");
        }
    }

    #[test]
    fn narrow_terminal_gets_one_column_with_the_channel_on_the_right() {
        let area = Rect::new(0, 0, 80, 40);
        let board = arrange(area, &mixed_groups());
        assert_eq!(board.channel.right(), area.right());
        for panel in &board.panels {
            assert_eq!(panel.channel_side, Side::Right);
            assert!(touches_channel(panel, board.channel));
        }
    }

    #[test]
    fn columns_are_balanced_by_height() {
        let board = arrange(Rect::new(0, 0, 160, 60), &mixed_groups());
        let side_of = |g: usize| board.panel(g).unwrap().channel_side;
        // The two arrays are the tallest, so they land in different columns.
        assert_ne!(side_of(2), side_of(3));
    }

    #[test]
    fn panels_in_a_column_stack_without_gaps() {
        let area = Rect::new(0, 0, 80, 40);
        let board = arrange(area, &mixed_groups());
        let mut tops: Vec<(u16, u16)> = board
            .panels
            .iter()
            .map(|p| (p.area.y, p.area.bottom()))
            .collect();
        tops.sort();
        assert_eq!(tops.first().unwrap().0, area.y);
        assert_eq!(tops.last().unwrap().1, area.bottom());
        for pair in tops.windows(2) {
            assert_eq!(pair[0].1, pair[1].0);
        }
    }

    #[test]
    fn path_is_walked_by_distance() {
        let path = Path::new(vec![(0.0, 0.0), (3.0, 0.0), (3.0, 4.0)]);
        assert_eq!(path.length(), 7.0);
        assert_eq!(path.at(-1.0), (0.0, 0.0));
        assert_eq!(path.at(1.5), (1.5, 0.0));
        assert_eq!(path.at(5.0), (3.0, 2.0));
        assert_eq!(path.at(99.0), (3.0, 4.0));
    }

    #[test]
    fn heading_follows_each_segment() {
        let path = Path::new(vec![(0.0, 0.0), (4.0, 0.0), (4.0, 3.0)]);
        assert_eq!(path.heading(1.0), (4.0, 0.0));
        assert_eq!(path.heading(5.0), (0.0, 3.0));
        // Past either end, the nearest segment.
        assert_eq!(path.heading(-1.0), (4.0, 0.0));
        assert_eq!(path.heading(99.0), (0.0, 3.0));
        assert_eq!(Path::new(vec![(1.0, 1.0)]).heading(0.0), (0.0, 0.0));
    }

    #[test]
    fn rounded_corners_keep_the_ends_and_shorten_the_path() {
        let corners = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0)];
        let path = rounded(corners);
        assert_eq!(path.start(), (0.0, 0.0));
        assert_eq!(path.end(), (4.0, 4.0));
        assert!(path.length() < 8.0);
        assert!(!path.points.contains(&(4.0, 0.0)));
    }

    #[test]
    fn routes_get_their_own_ports_and_lanes() {
        let groups = mixed_groups();
        let board = arrange(Rect::new(0, 0, 160, 50), &groups);
        // Two flows leave group 0 and two arrive at group 1.
        let routes = [standalone(0, 1), standalone(0, 4), standalone(4, 1)];
        let plans = board.plan(&routes);
        let trunks: Vec<&Path> = plans.iter().map(|p| &p.as_ref().unwrap().trunk).collect();

        assert_ne!(
            trunks[0].start(),
            trunks[1].start(),
            "separate exits from group 0"
        );
        assert_ne!(
            trunks[0].end(),
            trunks[2].end(),
            "separate entries into group 1"
        );

        // 0→4 and 4→1 both span most of the height, so they need different lanes.
        let ports = board.assign_ports(&routes);
        let lanes = board.assign_lanes(&routes, &ports);
        assert_ne!(lanes[&1], lanes[&2]);
        for x in lanes.values() {
            assert!(*x > f64::from(board.channel.x) && *x < f64::from(board.channel.right()));
        }
    }

    #[test]
    fn trunk_runs_from_panel_edge_to_panel_edge() {
        let groups = mixed_groups();
        let board = arrange(Rect::new(0, 0, 160, 40), &groups);
        let plan = board.plan(&[standalone(0, 1)]).remove(0).unwrap();
        let source = board.panel(0).unwrap().area;
        let target = board.panel(1).unwrap().area;
        assert!(inside(plan.trunk.start(), source));
        assert!(inside(plan.trunk.end(), target));
        assert!(plan.sources.is_empty() && plan.targets.is_empty());
        assert_eq!(plan.length(), plan.trunk.length());
    }

    #[test]
    fn array_ends_fan_out_to_every_disk() {
        let groups = mixed_groups();
        let board = arrange(Rect::new(0, 0, 160, 50), &groups);
        let route = Some(RouteEnds {
            from: 3,
            to: 2,
            from_disks: 2,
            to_disks: 2,
            target_copies: 2,
        });
        let plan = board.plan(&[route]).remove(0).unwrap();
        assert_eq!(plan.sources.len(), 2);
        assert_eq!(plan.targets.len(), 2);

        let source_panel = board.panel(3).unwrap();
        let source_disks = array_parts(source_panel.area, 2, source_panel.channel_side).members;
        for (path, disk) in plan.sources.iter().zip(&source_disks) {
            assert!(inside(path.start(), *disk), "source hop starts on its disk");
            assert_eq!(path.end(), plan.trunk.start(), "and meets the trunk");
        }
        let target_panel = board.panel(2).unwrap();
        let target_disks = array_parts(target_panel.area, 2, target_panel.channel_side).members;
        for (path, disk) in plan.targets.iter().zip(&target_disks) {
            assert_eq!(path.start(), plan.trunk.end());
            assert!(inside(path.end(), *disk));
        }
    }

    #[test]
    fn points_along_a_route_follow_the_split() {
        let groups = mixed_groups();
        let board = arrange(Rect::new(0, 0, 160, 50), &groups);
        let ends = |copies| RouteEnds {
            from: 0,
            to: 2,
            from_disks: 0,
            to_disks: 2,
            target_copies: copies,
        };
        // Just past the target port, where every branch is still running.
        let past_port = |plan: &RoutePaths| plan.length() - plan.target_length() + 0.1;

        let mirror = board.plan(&[Some(ends(2))]).remove(0).unwrap();
        assert_eq!(
            mirror.points_at(past_port(&mirror), Some(7)).len(),
            2,
            "mirror copies"
        );
        assert_eq!(mirror.points_at(past_port(&mirror), None).len(), 2);
        assert_eq!(mirror.points_at(1.0, Some(7)).len(), 1, "one trunk");

        let stripe = board.plan(&[Some(ends(1))]).remove(0).unwrap();
        // Near the end of the shorter branch, where the two have parted.
        let shorter = stripe
            .targets
            .iter()
            .map(Path::length)
            .fold(f64::MAX, f64::min);
        let s = stripe.length() - stripe.target_length() + shorter - 0.2;
        let first = stripe.points_at(s, Some(0));
        let second = stripe.points_at(s, Some(1));
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first, second, "stripe dots take turns");
    }

    #[test]
    fn dots_land_on_copies_neighbouring_disks_in_turn() {
        let paths: Vec<Path> = (0..8)
            .map(|i| Path::new(vec![(0.0, f64::from(i)), (1.0, f64::from(i))]))
            .collect();
        let landed = |copies, dot| -> Vec<f64> {
            branches(&paths, copies, Some(dot))
                .map(|p| p.at(0.0).1)
                .collect()
        };
        // Two copies over eight disks: 0–1, then 2–3, …, then back to 0–1.
        assert_eq!(landed(2, 0), vec![0.0, 1.0]);
        assert_eq!(landed(2, 1), vec![2.0, 3.0]);
        assert_eq!(landed(2, 3), vec![6.0, 7.0]);
        assert_eq!(landed(2, 4), vec![0.0, 1.0]);
        // One copy takes turns; as many copies as disks lands on all of them.
        assert_eq!(landed(1, 5), vec![5.0]);
        assert_eq!(landed(8, 5).len(), 8);
        assert_eq!(
            branches(&paths, 2, None).count(),
            8,
            "tracks show every branch"
        );
    }

    #[test]
    fn routes_to_missing_groups_are_skipped() {
        let board = arrange(Rect::new(0, 0, 160, 40), &mixed_groups());
        let plans = board.plan(&[standalone(0, 9), None, standalone(0, 1)]);
        assert!(plans[0].is_none());
        assert!(plans[1].is_none());
        assert!(plans[2].is_some());
    }

    #[test]
    fn array_parts_stay_inside_the_border() {
        let area = Rect::new(10, 5, 50, 20);
        for side in [Side::Left, Side::Right] {
            let parts = array_parts(area, 3, side);
            assert_eq!(parts.members.len(), 3);
            for disk in &parts.members {
                assert!(disk.x > area.x && disk.right() < area.right());
                assert!(disk.y > area.y && disk.bottom() < area.bottom());
            }
            for slot in 0..4 {
                let x = parts.bus_lane(slot);
                assert!(x > f64::from(area.x) && x < f64::from(area.right()));
            }
        }
    }

    #[test]
    fn empty_input_gives_an_empty_board() {
        assert!(arrange(Rect::new(0, 0, 100, 40), &[]).panels.is_empty());
    }
}
