use std::collections::HashMap;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::symbols::{self, Marker};
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::widgets::{Axis, Block, BorderType, Chart, Dataset, GraphType, LineGauge, Paragraph};

use crate::app::App;
use crate::flows::{Dot, FlowField, FlowStyle, Route};
use crate::format;
use crate::layout::{self, Board, Panel, Point, RouteEnds};
use crate::model::{self, ArrayLayout, Group, GroupKind, History, Member, Rate};

const READ_COLOR: Color = Color::Cyan;
const WRITE_COLOR: Color = Color::Magenta;
const ARRAY_COLOR: Color = Color::Blue;
/// Each flow takes the next free color: bright for its dots, dim for its track.
const DOT_COLORS: [Color; 5] = [
    Color::LightYellow,
    Color::LightGreen,
    Color::LightRed,
    Color::LightBlue,
    Color::White,
];
const TRACK_COLORS: [Color; 5] = [
    Color::Yellow,
    Color::Green,
    Color::Red,
    Color::Blue,
    Color::DarkGray,
];
/// Most flows listed under the panels.
const MAX_LISTED_FLOWS: usize = 4;
/// Member disks get a box only when every box can be this tall: border,
/// rates and a two-row graph. Otherwise they're drawn as compact rows.
const MIN_BOXED_MEMBER_ROWS: u16 = 5;
/// The flow list's route column is at least this wide, so it rarely changes.
const MIN_ROUTE_WIDTH: usize = 28;
/// Distance between the dots that draw a track, in cells: far enough apart
/// that the track is dotted and a dot's solid tail stands out on it.
const TRACK_STEP: f64 = 1.0;
/// How long each dot's dim tail is, in cells, and how finely it's drawn.
const DOT_TAIL: f64 = 1.5;
const TAIL_STEP: f64 = 0.25;
/// Arrowheads for each way a flow can run. From the old PC character set,
/// so nearly every terminal font has them.
const ARROW_RIGHT: char = '►';
const ARROW_LEFT: char = '◄';
const ARROW_UP: char = '▲';
const ARROW_DOWN: char = '▼';
/// Where a dashed arrow starts.
const SOURCE_MARK: char = '●';
/// A dashed arrow repeats every this many cells: a dash, then a gap.
const DASH_PERIOD: f64 = 3.0;
const DASH_LENGTH: f64 = 2.0;
/// How finely dashes are traced along a route, in cells: fine enough to
/// catch every half cell.
const DASH_STEP: f64 = 0.25;
/// Halves of a cell a line can cover, from its middle out to each edge.
const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;
/// A thin line through the halves of a cell, indexed by those halves, with
/// rounded corners like the panels'.
const LINES: [char; 16] = [
    ' ', '╵', '╷', '│', '╴', '╯', '╮', '┤', '╶', '╰', '╭', '├', '─', '┴', '┬', '┼',
];

/// The graph's y-axis never shrinks below this, so an idle drive draws a flat
/// line instead of magnifying a few bytes of noise.
const MIN_GRAPH_SCALE: f64 = 64.0 * 1024.0;

/// Draws the whole screen. Returns the on-screen length of each flow route,
/// in the order of [`FlowField::routes`], so the engine knows where they end.
pub fn draw(frame: &mut Frame, app: &App) -> Vec<Option<f64>> {
    let groups = app.source.groups();
    let listed = listed_flows(&app.field);
    let list_rows = if app.show_flows {
        app.flow_list_rows
    } else {
        0
    };
    let [header, body, flow_list, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(list_rows),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app);
    draw_footer(frame, footer, app);

    if groups.is_empty() {
        let empty = Paragraph::new("No drives found").dark_gray().centered();
        frame.render_widget(empty, body);
        return vec![None; app.field.routes().len()];
    }

    let board = layout::arrange(body, groups);
    for panel in &board.panels {
        let group = &groups[panel.group];
        match &group.kind {
            GroupKind::Standalone => draw_standalone(frame, panel.area, group),
            GroupKind::Array(array) => draw_array(frame, panel, group, array),
            GroupKind::Network => draw_network(frame, panel.area, group),
        }
    }

    let plans = plan_routes(&board, &app.field, groups);
    if app.show_flows {
        match app.field.style() {
            FlowStyle::Dots => draw_dots(frame, body, &app.field, &plans),
            FlowStyle::Arrows => draw_arrows(frame.buffer_mut(), body, &app.field, &plans),
        }
        draw_flow_list(frame, flow_list, &listed, groups, &app.field);
    }
    plans
        .iter()
        .map(|plan| plan.as_ref().map(layout::RoutePaths::length))
        .collect()
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let groups = app.source.groups();
    // Disk totals: network traffic isn't disk I/O.
    let total: Rate = groups
        .iter()
        .filter(|g| !g.is_network())
        .map(Group::current)
        .sum();
    let disks = model::disk_count(groups);
    let arrays = groups.iter().filter(|g| g.is_array()).count();

    let mut spans = vec![
        " disktop ".bold().reversed(),
        format!("  {}", plural(disks, "disk")).into(),
    ];
    if arrays > 0 {
        spans.push(format!(" · {}", plural(arrays, "array")).into());
    }
    spans.extend([
        "  ".into(),
        rate_span("read", total.read, READ_COLOR),
        "  ".into(),
        rate_span("write", total.write, WRITE_COLOR),
    ]);
    if let Some(badge) = app.source.badge() {
        spans.extend(["  ".into(), format!(" {badge} ").black().on_yellow()]);
    }
    let left = Line::from(spans);
    // The style goes on the text rather than the line, since a line's own
    // style recolors the whole row, including what's drawn on the left.
    let right = Line::from(format!("every {:.1}s ", app.interval.as_secs_f64()).dark_gray())
        .right_aligned();

    // On a narrow terminal the interval gives way rather than overlapping.
    let fits = left.width() + right.width() < usize::from(area.width);
    frame.render_widget(left, area);
    if fits {
        frame.render_widget(right, area);
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let mut keys = vec![" q".bold(), " quit   ".dark_gray()];
    keys.extend(["f".bold(), " flows   ".dark_gray()]);
    // Names the style a press switches to.
    let other = app.field.style().toggled().name();
    keys.extend(["a".bold(), format!(" {other}   ").dark_gray()]);
    keys.extend(["p".bold(), " pause".dark_gray()]);
    let keys = Line::from(keys);

    let mut state = Vec::new();
    if !app.show_flows {
        state.push("flows hidden");
    }
    if app.paused {
        state.push("paused");
    }
    let line = if state.is_empty() {
        // With nothing more pressing to say, say what limits the flows.
        app.source
            .flow_note()
            .map(|note| Line::from(format!("{note} ").dark_gray()))
    } else {
        Some(Line::from(format!("{} ", state.join(" · ")).yellow()))
    };

    let keys_width = keys.width();
    frame.render_widget(keys, area);
    // Like the header, a narrow terminal drops the right side rather than
    // writing it over the keys.
    if let Some(line) = line
        && keys_width + line.width() < usize::from(area.width)
    {
        frame.render_widget(line.right_aligned(), area);
    }
}

/// A rounded box with a border in `color` and `title` along the top.
fn panel_block(color: Color, title: Line<'static>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        // The title is drawn over the border; without this, the parts of it
        // with no color of their own would take the border's.
        .title_style(Style::new().fg(Color::Reset))
        .title(title)
}

fn draw_standalone(frame: &mut Frame, area: Rect, group: &Group) {
    let block = panel_block(Color::DarkGray, group_title(group));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [capacity, rates, graph] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(inner);

    let Some(member) = group.members.first() else {
        return;
    };
    let samples = recent_samples(&member.history, graph);
    frame.render_widget(capacity_gauge(group), capacity);
    frame.render_widget(rates_line(member.history.current(), peak(&samples)), rates);
    draw_graph(frame, graph, &member.history, &samples);
}

/// The network: data received and sent, with a graph, and no capacity.
fn draw_network(frame: &mut Frame, area: Rect, group: &Group) {
    let block = panel_block(Color::DarkGray, group_title(group));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [rates, graph] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
    let Some(member) = group.members.first() else {
        return;
    };
    let samples = recent_samples(&member.history, graph);
    frame.render_widget(
        labelled_rates_line(member.history.current(), peak(&samples), ["down", "up"]),
        rates,
    );
    draw_graph(frame, graph, &member.history, &samples);
}

fn draw_array(frame: &mut Frame, panel: &Panel, group: &Group, array: &ArrayLayout) {
    frame.render_widget(panel_block(ARRAY_COLOR, group_title(group)), panel.area);

    let parts = layout::array_parts(panel.area, group.members.len(), panel.channel_side);
    let [capacity, status] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(parts.header);
    frame.render_widget(capacity_gauge(group), capacity);

    let total = group.current();
    let status_line = Line::from(vec![
        format!(
            "{} · {}   ",
            array.summary,
            plural(group.members.len(), "disk")
        )
        .dark_gray(),
        rate_span("read", total.read, READ_COLOR),
        "   ".into(),
        rate_span("write", total.write, WRITE_COLOR),
    ]);
    frame.render_widget(status_line, status);

    // Boxed disks when every box has room for a graph, otherwise one
    // compact row per disk, so a big array still fits.
    let boxed = parts
        .members
        .iter()
        .all(|area| area.height >= MIN_BOXED_MEMBER_ROWS);
    for (member, area) in group.members.iter().zip(&parts.members) {
        if boxed {
            draw_member(frame, *area, member);
        } else {
            draw_member_row(frame, *area, member);
        }
    }
}

fn draw_member(frame: &mut Frame, area: Rect, member: &Member) {
    let block = panel_block(
        Color::DarkGray,
        Line::from(vec![
            " ".into(),
            member.name.clone().bold(),
            "  ".into(),
            member.details.clone().dark_gray(),
            " ".into(),
        ]),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [rates, graph] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
    let samples = recent_samples(&member.history, graph);
    frame.render_widget(rates_line(member.history.current(), peak(&samples)), rates);
    draw_graph(frame, graph, &member.history, &samples);
}

/// A member disk without a box: name, rates and model on one line, and a
/// graph underneath when there's room.
fn draw_member_row(frame: &mut Frame, area: Rect, member: &Member) {
    let [text, graph] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let current = member.history.current();
    let line = Line::from(vec![
        format!("{:<5} ", member.name).bold(),
        rate_span("read", current.read, READ_COLOR),
        "  ".into(),
        rate_span("write", current.write, WRITE_COLOR),
        format!("  {}", member.details).dark_gray(),
    ]);
    frame.render_widget(line, text);
    if graph.height > 0 {
        let samples = recent_samples(&member.history, graph);
        draw_graph(frame, graph, &member.history, &samples);
    }
}

fn group_title(group: &Group) -> Line<'static> {
    Line::from(vec![
        " ".into(),
        group.title.clone().bold(),
        "  ".into(),
        group.details.join(" · ").dark_gray(),
        " ".into(),
    ])
}

fn capacity_gauge(group: &Group) -> LineGauge<'static> {
    let ratio = group.used_ratio().clamp(0.0, 1.0);
    let color = if ratio >= 0.9 {
        Color::Red
    } else if ratio >= 0.75 {
        Color::Yellow
    } else {
        Color::Green
    };
    let label = format!(
        "{:>3.0}% {} / {}",
        ratio * 100.0,
        format::bytes_fixed(group.used_space() as f64),
        format::bytes_fixed(group.total_space as f64),
    );

    LineGauge::default()
        .ratio(ratio)
        .label(label)
        .filled_symbol(symbols::line::THICK_HORIZONTAL)
        .unfilled_symbol(symbols::line::THICK_HORIZONTAL)
        .filled_style(Style::new().fg(color))
        .unfilled_style(Style::new().dark_gray())
}

fn rates_line(current: Rate, peak: f64) -> Line<'static> {
    labelled_rates_line(current, peak, ["read", "write"])
}

/// A rates line with its own names for the two directions, such as
/// `down` and `up` for the network.
fn labelled_rates_line(current: Rate, peak: f64, names: [&str; 2]) -> Line<'static> {
    Line::from(vec![
        rate_span(names[0], current.read, READ_COLOR),
        "   ".into(),
        rate_span(names[1], current.write, WRITE_COLOR),
        format!("   peak {}", format::rate_fixed(peak)).dark_gray(),
    ])
}

fn rate_span(name: &str, bytes_per_sec: f64, color: Color) -> Span<'static> {
    Span::styled(
        format!("● {name} {}", format::rate_fixed(bytes_per_sec)),
        Style::new().fg(color),
    )
}

/// The samples that fit in a graph: braille packs two into each column.
fn recent_samples(history: &History, graph: Rect) -> Vec<Rate> {
    history.recent(usize::from(graph.width) * 2).collect()
}

fn peak(samples: &[Rate]) -> f64 {
    samples
        .iter()
        .map(|rate| rate.read.max(rate.write))
        .fold(0.0, f64::max)
}

fn draw_graph(frame: &mut Frame, area: Rect, history: &History, samples: &[Rate]) {
    if area.height == 0 {
        return;
    }
    if history.is_empty() {
        let waiting = Paragraph::new("collecting samples…").dark_gray().centered();
        frame.render_widget(waiting, area);
        return;
    }

    // Right-align the samples so the newest one always sits at the right edge.
    let window = usize::from(area.width) * 2;
    let offset = window.saturating_sub(samples.len());
    let points = |pick: fn(&Rate) -> f64| -> Vec<(f64, f64)> {
        samples
            .iter()
            .enumerate()
            .map(|(i, rate)| ((offset + i) as f64, pick(rate)))
            .collect()
    };
    let reads = points(|rate| rate.read);
    let writes = points(|rate| rate.write);

    let chart = Chart::new(vec![
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(READ_COLOR))
            .data(&reads),
        Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(WRITE_COLOR))
            .data(&writes),
    ])
    .x_axis(Axis::default().bounds([0.0, window.saturating_sub(1) as f64]))
    .y_axis(Axis::default().bounds([0.0, peak(samples).max(MIN_GRAPH_SCALE) * 1.15]));

    frame.render_widget(chart, area);
}

/// Lays out a path for every route, in the order of [`FlowField::routes`].
fn plan_routes(
    board: &Board,
    field: &FlowField,
    groups: &[Group],
) -> Vec<Option<layout::RoutePaths>> {
    let index_of = |id: &str| groups.iter().position(|g| g.id == id);
    let disks = |group: &Group| {
        if group.is_array() {
            group.members.len()
        } else {
            0
        }
    };
    let ends: Vec<Option<RouteEnds>> = field
        .routes()
        .iter()
        .map(|route| {
            let from = index_of(&route.from)?;
            let to = index_of(&route.to)?;
            let target_copies = match &groups[to].kind {
                GroupKind::Array(array) => array.copies,
                GroupKind::Standalone | GroupKind::Network => 1,
            };
            Some(RouteEnds {
                from,
                to,
                from_disks: disks(&groups[from]),
                to_disks: disks(&groups[to]),
                target_copies,
            })
        })
        .collect();
    board.plan(&ends)
}

/// Draws each flow's track as a dim dotted line and its dots in a bright
/// color, each trailing a short solid tail in the dim one, so it points the
/// way it's going.
fn draw_dots(
    frame: &mut Frame,
    body: Rect,
    field: &FlowField,
    plans: &[Option<layout::RoutePaths>],
) {
    let to_canvas = |(x, y): Point| (x - f64::from(body.x), f64::from(body.bottom()) - y);
    let mut tracks: Vec<Vec<(f64, f64)>> = vec![Vec::new(); TRACK_COLORS.len()];
    let mut dots: Vec<Vec<(f64, f64)>> = vec![Vec::new(); DOT_COLORS.len()];

    for (route, plan) in field.routes().iter().zip(plans) {
        let Some(plan) = plan else {
            continue;
        };
        let color = route.color % DOT_COLORS.len();

        // The track covers the part of the route the stream occupies, so it
        // grows out from the source as a copy starts and drains when it ends.
        let (start, end) = track_span(route, plan.length());
        let mut s = (start / TRACK_STEP).ceil() * TRACK_STEP;
        while s <= end {
            tracks[color].extend(plan.points_at(s, None).into_iter().map(to_canvas));
            s += TRACK_STEP;
        }

        for dot in route.dots() {
            let (head, tail) = comet(plan, dot);
            dots[color].extend(head.into_iter().map(to_canvas));
            // The tail is drawn with the track, in the dim color.
            tracks[color].extend(tail.into_iter().map(to_canvas));
        }
    }

    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, f64::from(body.width)])
        .y_bounds([0.0, f64::from(body.height)])
        .paint(|ctx| {
            for (color, coords) in TRACK_COLORS.iter().zip(&tracks) {
                ctx.draw(&Points {
                    coords,
                    color: *color,
                });
            }
            // Heads go on their own layer so they replace what's under them.
            ctx.layer();
            for (color, coords) in DOT_COLORS.iter().zip(&dots) {
                ctx.draw(&Points {
                    coords,
                    color: *color,
                });
            }
        });
    frame.render_widget(canvas, body);
}

/// Draws each flow as a dashed line from a dot where the data leaves to an
/// arrowhead where it arrives, in line characters half a cell at a time.
/// The dashes march toward the arrowhead, every flow at the same speed, and
/// each dash is bright at the front and dim at the back, so it points the
/// way it's going.
fn draw_arrows(
    buffer: &mut Buffer,
    body: Rect,
    field: &FlowField,
    plans: &[Option<layout::RoutePaths>],
) {
    for (route, plan) in field.routes().iter().zip(plans) {
        let Some(plan) = plan else {
            continue;
        };
        let color = route.color % DOT_COLORS.len();
        let style = Style::new().fg(DOT_COLORS[color]);
        let length = plan.length();
        // Like the dots' track, the line grows out from the source as a
        // copy starts and drains into the target when it ends.
        let (start, end) = track_span(route, length);

        // The halves of each cell the dashes cover, and whether a dash's
        // front is in it.
        let mut cells: HashMap<(u16, u16), (u8, bool)> = HashMap::new();
        let mut s = (start / DASH_STEP).ceil() * DASH_STEP;
        while s <= end {
            // How far into its dash this point is, from the back.
            let into_dash = (s - route.marched()).rem_euclid(DASH_PERIOD);
            if into_dash < DASH_LENGTH {
                for (point, heading) in plan.headings_at(s, None) {
                    if let Some(cell) = cell_at(body, point) {
                        let (halves, front) = cells.entry(cell).or_default();
                        *halves |= half_of(point, heading);
                        *front |= into_dash >= DASH_LENGTH / 2.0;
                    }
                }
            }
            s += DASH_STEP;
        }
        for (cell, (halves, front)) in cells {
            let style = if front {
                style.bold()
            } else {
                Style::new().fg(TRACK_COLORS[color])
            };
            buffer[cell]
                .set_char(LINES[usize::from(halves)])
                .set_style(style);
        }

        let mut mark = |point: Point, symbol: char| {
            if let Some(cell) = cell_at(body, point) {
                buffer[cell].set_char(symbol).set_style(style.bold());
            }
        };
        if route.running {
            for point in plan.starts() {
                mark(point, SOURCE_MARK);
            }
        }
        if end >= length {
            for (point, heading) in plan.finishes() {
                mark(point, arrowhead(heading));
            }
        }
    }
}

/// The cell a point is in, if it's inside `area`.
fn cell_at(area: Rect, (x, y): Point) -> Option<(u16, u16)> {
    let (column, row) = (x.floor(), y.floor());
    let inside = column >= f64::from(area.left())
        && column < f64::from(area.right())
        && row >= f64::from(area.top())
        && row < f64::from(area.bottom());
    inside.then_some((column as u16, row as u16))
}

/// Which half of its cell a line running along `heading` covers at `point`.
/// A cell is about twice as tall as it is wide, so a step of one row counts
/// as much as two columns.
fn half_of((x, y): Point, (dx, dy): Point) -> u8 {
    if dx.abs() >= dy.abs() * 2.0 {
        if x - x.floor() < 0.5 { LEFT } else { RIGHT }
    } else if y - y.floor() < 0.5 {
        UP
    } else {
        DOWN
    }
}

/// The arrowhead for a flow running along `heading`, a step in cells,
/// judged the same way as [`half_of`].
fn arrowhead((dx, dy): Point) -> char {
    if dx.abs() >= dy.abs() * 2.0 {
        if dx >= 0.0 { ARROW_RIGHT } else { ARROW_LEFT }
    } else if dy > 0.0 {
        ARROW_DOWN
    } else {
        ARROW_UP
    }
}

/// The points that draw a dot: its head, the front half cell, and its tail
/// behind that. The tail runs straight back and stops at the first bend,
/// so on a route's short twisty legs, such as inside an array, it doesn't
/// wrap around the corners into a lump.
fn comet(plan: &layout::RoutePaths, dot: &Dot) -> (Vec<Point>, Vec<Point>) {
    let fronts = plan.headings_at(dot.distance, Some(dot.id));
    let straight_on = |heading: Point| fronts.iter().any(|&(_, front)| same_way(front, heading));
    let (mut head, mut tail) = (Vec::new(), Vec::new());
    let mut behind = 0.0;
    while behind <= DOT_TAIL && dot.distance - behind >= 0.0 {
        let points: Vec<Point> = plan
            .headings_at(dot.distance - behind, Some(dot.id))
            .into_iter()
            .filter(|&(_, heading)| straight_on(heading))
            .map(|(point, _)| point)
            .collect();
        if points.is_empty() {
            break;
        }
        if behind < 0.5 {
            head.extend(points);
        } else {
            tail.extend(points);
        }
        behind += TAIL_STEP;
    }
    (head, tail)
}

/// Whether two headings point the same way.
fn same_way((ax, ay): Point, (bx, by): Point) -> bool {
    let lengths = ax.hypot(ay) * bx.hypot(by);
    lengths > 0.0 && (ax * bx + ay * by) / lengths > 0.99
}

/// The stretch of a route to draw a track along.
fn track_span(route: &Route, length: f64) -> (f64, f64) {
    let start = route.tail().min(length);
    let end = if route.running {
        route.head().min(length)
    } else {
        length
    };
    (start, end)
}

struct ListedFlow {
    from: String,
    to: String,
    rate: f64,
    process: String,
    color: Color,
}

/// Running flows, fastest first.
fn listed_flows(field: &FlowField) -> Vec<ListedFlow> {
    let mut flows: Vec<ListedFlow> = field
        .routes()
        .iter()
        .filter(|r| r.running)
        .map(|r| ListedFlow {
            from: r.from.clone(),
            to: r.to.clone(),
            rate: r.rate,
            process: r.process.clone(),
            color: DOT_COLORS[r.color % DOT_COLORS.len()],
        })
        .collect();
    flows.sort_by(|a, b| b.rate.total_cmp(&a.rate));
    flows
}

/// Rows the flow list needs for the flows running now: a heading and a
/// line per flow, up to a limit. None when nothing is running.
pub fn flow_list_rows(field: &FlowField) -> u16 {
    let running = field.routes().iter().filter(|r| r.running).count();
    if running == 0 {
        0
    } else {
        running.min(MAX_LISTED_FLOWS) as u16 + 1
    }
}

fn draw_flow_list(
    frame: &mut Frame,
    area: Rect,
    flows: &[ListedFlow],
    groups: &[Group],
    field: &FlowField,
) {
    if area.is_empty() {
        return;
    }
    let title_of = |id: &str| {
        groups
            .iter()
            .find(|g| g.id == id)
            .map_or_else(|| id.to_string(), |g| g.title.clone())
    };

    let shown = flows.len().min(MAX_LISTED_FLOWS);
    let routes: Vec<String> = flows
        .iter()
        .take(shown)
        .map(|f| format!("{} → {}", title_of(&f.from), title_of(&f.to)))
        .collect();
    // One width for the whole list, so the rate column lines up on every row.
    let route_width = routes
        .iter()
        .map(|r| r.chars().count())
        .max()
        .unwrap_or(0)
        .max(MIN_ROUTE_WIDTH);

    // The scale, like the one on a map: what one dot stands for.
    let fixed = if field.is_fixed() { " (fixed)" } else { "" };
    let scale = match field.style() {
        FlowStyle::Dots => format!(
            " flows · each dot is {}{fixed}",
            format::size_label(field.dot_size())
        ),
        // Dashes all march at one speed, so they have no scale.
        FlowStyle::Arrows => " flows".to_string(),
    };
    let mut lines = vec![Line::from(scale.dark_gray())];
    if flows.is_empty() {
        // The list keeps its space, so say why it's empty.
        lines.push(Line::from(
            " no copies between drives right now".dark_gray(),
        ));
    }
    for (i, (flow, route)) in flows.iter().zip(&routes).enumerate() {
        let mut spans = vec![
            Span::styled(" ━━▶ ", Style::new().fg(flow.color)),
            format!("{route:<route_width$} ").into(),
            format::rate_fixed(flow.rate).bold(),
            format!("   {}", flow.process).dark_gray(),
        ];
        if i == shown - 1 && flows.len() > shown {
            spans.push(format!("   +{} more", flows.len() - shown).dark_gray());
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::demo::Demo;
    use crate::model::Source;

    fn demo_app_at(t: f64) -> App {
        let mut demo = Demo::new();
        demo.advance_to(t);
        App::new(Box::new(demo), Duration::from_secs(1), None, 20)
    }

    /// Draws `frames` frames, stepping the flows between them as the app does.
    fn animate(terminal: &mut Terminal<TestBackend>, app: &mut App, frames: usize) {
        for _ in 0..frames {
            let mut lengths = Vec::new();
            terminal.draw(|frame| lengths = draw(frame, app)).unwrap();
            app.field.set_lengths(&lengths);
            app.field.step(0.033);
        }
    }

    fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn numbers_keep_their_columns_as_values_change() {
        let values = [0.0, 821.0, 39.3 * 1024.0 * 1024.0, 553.5e6, 1.7e9];
        let layouts: Vec<(Option<usize>, Option<usize>, usize)> = values
            .iter()
            .flat_map(|&read| values.iter().map(move |&write| (read, write)))
            .map(|(read, write)| {
                let line = text(&rates_line(Rate { read, write }, read.max(write)));
                (
                    line.find("● write"),
                    line.find("peak"),
                    line.chars().count(),
                )
            })
            .collect();
        assert!(layouts.windows(2).all(|w| w[0] == w[1]), "{layouts:?}");
    }

    #[test]
    fn header_stays_put_as_rates_change() {
        let header_at = |t: f64| {
            let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
            animate(&mut terminal, &mut demo_app_at(t), 1);
            let line = screen(&terminal)[0].clone();
            (line.find("● write"), line.find("demo"))
        };
        // Idle, a slow copy, and two fast ones at once.
        assert_eq!(header_at(59.5), header_at(10.0));
        assert_eq!(header_at(10.0), header_at(23.0));
    }

    #[test]
    fn redraw_after_a_new_flow_keeps_every_character() {
        let mut terminal = Terminal::new(TestBackend::new(78, 120)).unwrap();
        animate(&mut terminal, &mut demo_app_at(3.0), 30);
        animate(&mut terminal, &mut demo_app_at(10.0), 30);
        let lines = screen(&terminal);
        assert!(lines.iter().any(|l| l.contains(r"C:\ → E:\")));
        assert!(lines.iter().any(|l| l.contains("explorer.exe")));
    }

    #[test]
    fn header_drops_the_interval_instead_of_overlapping() {
        let mut terminal = Terminal::new(TestBackend::new(78, 60)).unwrap();
        animate(&mut terminal, &mut demo_app_at(10.0), 1);
        let header = &screen(&terminal)[0];
        assert!(!header.contains("every"), "{header}");
        assert!(header.contains("write"));

        let mut terminal = Terminal::new(TestBackend::new(160, 60)).unwrap();
        animate(&mut terminal, &mut demo_app_at(10.0), 1);
        assert!(screen(&terminal)[0].contains("every 1.0s"));
    }

    /// The demo with a caveat about flows, like a run without root.
    struct Caveated(Demo);

    impl Source for Caveated {
        fn sample(&mut self) {
            self.0.sample();
        }

        fn groups(&self) -> &[Group] {
            self.0.groups()
        }

        fn flows(&self) -> &[model::Flow] {
            self.0.flows()
        }

        fn flow_note(&self) -> Option<&str> {
            Some("flows: your processes only · run with sudo to see all")
        }
    }

    #[test]
    fn footer_drops_the_note_instead_of_overlapping_the_keys() {
        let footer_at = |width: u16| {
            let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
            let mut app = App::new(
                Box::new(Caveated(Demo::new())),
                Duration::from_secs(1),
                None,
                20,
            );
            animate(&mut terminal, &mut app, 1);
            screen(&terminal).last().unwrap().clone()
        };
        let narrow = footer_at(80);
        assert!(
            narrow.contains("q quit") && narrow.contains("p pause"),
            "{narrow}"
        );
        assert!(!narrow.contains("sudo"), "{narrow}");
        assert!(footer_at(120).contains("run with sudo to see all"));
    }

    #[test]
    fn text_keeps_its_own_colors() {
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        let mut app = demo_app_at(10.0);
        app.paused = true;
        animate(&mut terminal, &mut app, 1);
        let lines = screen(&terminal);
        let buffer = terminal.backend().buffer();
        let cell_at = |row: usize, text: &str| {
            let line = &lines[row];
            let column = line[..line.find(text).unwrap()].chars().count();
            buffer[(column as u16, row as u16)].clone()
        };

        // The interval at the right of the header doesn't recolor the rates.
        assert_eq!(cell_at(0, "● read").fg, READ_COLOR);
        assert_eq!(cell_at(0, "● write").fg, WRITE_COLOR);
        assert_eq!(cell_at(0, "demo").fg, Color::Black);
        // Nor does "paused" at the right of the footer recolor the keys.
        let footer = lines.len() - 1;
        assert_eq!(cell_at(footer, "q quit").fg, Color::Reset);
        assert_eq!(cell_at(footer, "paused").fg, Color::Yellow);
        // A panel's title stands out from its border and details.
        let title = lines.iter().position(|l| l.contains("Network")).unwrap();
        assert_eq!(cell_at(title, "Network").fg, Color::Reset);
        assert_eq!(cell_at(title, "Ethernet").fg, Color::DarkGray);
    }

    #[test]
    fn flows_draw_dots_in_the_channel() {
        let mut terminal = Terminal::new(TestBackend::new(160, 60)).unwrap();
        let mut app = demo_app_at(10.0);
        animate(&mut terminal, &mut app, 90);
        let buffer = terminal.backend().buffer();
        let dot_cells = buffer
            .content()
            .iter()
            .filter(|cell| DOT_COLORS.contains(&cell.fg))
            .filter(|cell| {
                cell.symbol()
                    .chars()
                    .all(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
            })
            .count();
        assert!(dot_cells > 10, "{dot_cells} dot cells");
    }

    #[test]
    fn arrows_are_dashed_lines_from_a_dot_to_an_arrowhead() {
        let mut terminal = Terminal::new(TestBackend::new(160, 60)).unwrap();
        let mut app = demo_app_at(10.0);
        app.field.set_style(FlowStyle::Arrows);
        // Long enough for every line to reach its target.
        animate(&mut terminal, &mut app, 200);
        let flow_cells: Vec<char> = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|cell| DOT_COLORS.contains(&cell.fg))
            .filter_map(|cell| cell.symbol().chars().next())
            .collect();
        let count = |symbols: &[char]| flow_cells.iter().filter(|c| symbols.contains(c)).count();
        assert!(count(&LINES[1..]) > 20, "{flow_cells:?}");
        // Dashes are bright at the front and dim at the back.
        let dashes = |colors: &[Color]| {
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .filter(|cell| colors.contains(&cell.fg))
                .filter(|cell| LINES[1..].iter().any(|&l| cell.symbol().starts_with(l)))
                .count()
        };
        assert!(dashes(&DOT_COLORS) > 5 && dashes(&TRACK_COLORS) > 5);
        assert!(count(&[SOURCE_MARK]) >= 2, "{flow_cells:?}");
        assert!(
            count(&[ARROW_RIGHT, ARROW_LEFT, ARROW_UP, ARROW_DOWN]) >= 2,
            "{flow_cells:?}"
        );
        let lines = screen(&terminal);
        assert!(!lines.iter().any(|l| l.contains("each dot is")));
        assert!(lines.last().unwrap().contains("a dots"));
    }

    #[test]
    fn dashes_cover_the_halves_of_each_cell_they_pass() {
        let across = (1.0, 0.0);
        let down = (0.0, 1.0);
        assert_eq!(half_of((4.2, 7.5), across), LEFT);
        assert_eq!(half_of((4.7, 7.5), across), RIGHT);
        assert_eq!(half_of((4.5, 7.2), down), UP);
        assert_eq!(half_of((4.5, 7.9), down), DOWN);
        assert_eq!(LINES[usize::from(LEFT | RIGHT)], '─');
        assert_eq!(LINES[usize::from(UP | DOWN)], '│');
        assert_eq!(LINES[usize::from(UP | RIGHT)], '╰');
        assert_eq!(LINES[usize::from(DOWN | LEFT)], '╮');
    }

    #[test]
    fn dot_tails_stop_at_a_bend() {
        // Right for 10 cells, then down for 10.
        let plan = layout::RoutePaths {
            sources: Vec::new(),
            trunk: layout::Path::new(vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]),
            targets: Vec::new(),
            target_copies: 1,
        };
        let at = |distance| comet(&plan, &Dot { id: 0, distance });

        // On a straight run, the whole tail.
        let (head, tail) = at(5.0);
        assert_eq!(head.len(), 2);
        assert_eq!(tail.len(), 5);
        assert!(tail.iter().all(|&(_, y)| y == 0.0));

        // Just round the corner, only the part below it.
        let (head, tail) = at(10.75);
        assert!(!head.is_empty());
        assert!(tail.iter().all(|&(x, y)| x == 10.0 && y > 0.0), "{tail:?}");
        assert!(tail.len() < 5);
    }

    #[test]
    fn arrowheads_follow_the_heading() {
        assert_eq!(arrowhead((1.0, 0.0)), ARROW_RIGHT);
        assert_eq!(arrowhead((-3.0, 0.5)), ARROW_LEFT);
        assert_eq!(arrowhead((0.0, 2.0)), ARROW_DOWN);
        assert_eq!(arrowhead((0.5, -1.0)), ARROW_UP);
        // One row is about as far as two columns.
        assert_eq!(arrowhead((1.9, 1.0)), ARROW_DOWN);
    }

    #[test]
    fn flow_list_shows_what_each_dot_stands_for() {
        let mut terminal = Terminal::new(TestBackend::new(160, 60)).unwrap();
        animate(&mut terminal, &mut demo_app_at(10.0), 1);
        let lines = screen(&terminal);
        // The busiest demo copy is about 600 MiB/s, which gets 100 MiB dots.
        assert!(
            lines
                .iter()
                .any(|l| l.contains("flows · each dot is 100 MiB")),
            "{:?}",
            lines.iter().find(|l| l.contains("flows"))
        );
    }

    #[test]
    fn panels_stay_put_when_flows_stop() {
        let mut terminal = Terminal::new(TestBackend::new(160, 60)).unwrap();
        let mut app = demo_app_at(10.0);
        animate(&mut terminal, &mut app, 1);
        let busy = screen(&terminal);

        // Every copy stops: the list keeps its rows and says so.
        app.field.sync(&[]);
        animate(&mut terminal, &mut app, 1);
        let quiet = screen(&terminal);

        let panel_tops = |lines: &[String]| -> Vec<usize> {
            lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.starts_with('╭'))
                .map(|(i, _)| i)
                .collect()
        };
        assert_eq!(panel_tops(&busy), panel_tops(&quiet));
        assert!(
            quiet
                .iter()
                .any(|l| l.contains("no copies between drives right now"))
        );
    }

    #[test]
    fn renders_at_any_size_without_panicking() {
        for (width, height) in [(160, 50), (78, 40), (30, 10), (5, 3), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            animate(&mut terminal, &mut demo_app_at(30.0), 10);
        }
    }
}

#[cfg(test)]
mod snapshot {
    use crate::model::{Flow, Group, Source};

    /// Prints the demo as text. Run with
    /// `cargo test print_demo -- --ignored --nocapture`; COLS and ROWS set
    /// the size, AT the second of the demo, and STYLE=arrows draws arrows.
    #[test]
    #[ignore]
    fn print_demo() {
        use std::time::Duration;

        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (width, height) = (var("COLS", 160), var("ROWS", 50));
        let mut demo = crate::demo::Demo::new();
        demo.advance_to(var("AT", 23.0));
        let mut app = crate::app::App::new(Box::new(demo), Duration::from_secs(1), None, 20);
        if std::env::var("STYLE").is_ok_and(|style| style == "arrows") {
            app.field.set_style(crate::flows::FlowStyle::Arrows);
        }
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        animate(&mut terminal, &mut app, 120);
        print_screen(&terminal);
    }

    /// Prints this machine's real drives and flows as text, once a second,
    /// for checking flows where there's no screen to watch, such as over
    /// SSH or from an Administrator window. Run with
    /// `cargo test print_live -- --ignored --nocapture`; SECS sets how long
    /// (default 30).
    #[test]
    #[ignore]
    fn print_live() {
        use std::time::Duration;

        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (width, height) = (var("COLS", 160), var("ROWS", 50));
        let monitor = crate::drives::DriveMonitor::new();
        let mut app = crate::app::App::new(Box::new(monitor), Duration::from_secs(1), None, 20);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        for second in 1..=var("SECS", 30) {
            std::thread::sleep(app.interval);
            app.source.sample();
            app.field.sync(app.source.flows());
            app.flow_list_rows = app.flow_list_rows.max(super::flow_list_rows(&app.field));
            animate(&mut terminal, &mut app, 20);
            println!("--- {second}s");
            print_screen(&terminal);
        }
    }

    /// Records the demo frame by frame, for the animations in the README.
    /// Time is simulated, so every run gives the same frames. Run with
    /// `cargo test record_demo -- --ignored`; OUT names the file to write,
    /// COLS and ROWS set the size, AT the second of the demo to start at,
    /// SECS how long to record, FPS how many frames a second, and
    /// STYLE=arrows draws arrows. `scripts/record.py` runs this and turns
    /// the frames into GIFs.
    ///
    /// The file has a JSON line with the size and frame rate, then a JSON
    /// line per frame: for each row, runs of cells drawn alike, as
    /// `[text, foreground, background, modifier bits]`.
    #[test]
    #[ignore]
    fn record_demo() {
        use std::fmt::Write as _;
        use std::io::Write as _;
        use std::time::Duration;

        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        use crate::flows::FlowStyle;

        /// Seconds of demo played before recording starts. Enough to fill
        /// the graphs and run through every flow, so the flow list already
        /// has its full height and doesn't push the panels up mid-recording.
        const WARM_UP_SECS: u32 = 150;
        /// The last part of the warm-up is animated, so the flows are in
        /// full swing when recording starts.
        const ANIMATED_WARM_UP_SECS: u32 = 10;

        let out = std::env::var("OUT").expect("OUT names the file to write");
        let (width, height) = (var("COLS", 120), var("ROWS", 40));
        let (start, secs, fps) = (var("AT", 8u32), var("SECS", 10u32), var("FPS", 25u32));
        let dt = 1.0 / f64::from(fps);

        let warm_up_from = f64::from(start) - f64::from(WARM_UP_SECS);
        let source = Scripted {
            demo: crate::demo::Demo::new(),
            at: warm_up_from - 1.0,
        };
        let mut app = crate::app::App::new(Box::new(source), Duration::from_secs(1), None, fps);
        if std::env::var("STYLE").is_ok_and(|style| style == "arrows") {
            app.field.set_style(FlowStyle::Arrows);
        }
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut file = std::io::BufWriter::new(std::fs::File::create(&out).unwrap());
        writeln!(file, r#"{{"cols":{width},"rows":{height},"fps":{fps}}}"#).unwrap();

        let sample = |app: &mut crate::app::App| {
            app.source.sample();
            app.field.sync(app.source.flows());
            app.flow_list_rows = app.flow_list_rows.max(super::flow_list_rows(&app.field));
        };
        for _ in 0..WARM_UP_SECS - ANIMATED_WARM_UP_SECS {
            sample(&mut app);
        }
        let animated = ANIMATED_WARM_UP_SECS + secs;
        for frame in 0..animated * fps {
            // A new sample at the start of each second, as the app takes them.
            if frame % fps == 0 {
                sample(&mut app);
            }
            let mut lengths = Vec::new();
            terminal
                .draw(|frame| lengths = super::draw(frame, &app))
                .unwrap();
            app.field.set_lengths(&lengths);
            app.field.step(dt);

            if frame >= ANIMATED_WARM_UP_SECS * fps {
                let buffer = terminal.backend().buffer();
                let mut rows = Vec::new();
                for y in 0..buffer.area.height {
                    let mut runs: Vec<(String, String)> = Vec::new();
                    for x in 0..buffer.area.width {
                        let cell = &buffer[(x, y)];
                        let style = format!(
                            r#""{:?}","{:?}",{}"#,
                            cell.fg,
                            cell.bg,
                            cell.modifier.bits()
                        );
                        let symbol = cell.symbol().chars().next().unwrap_or(' ');
                        match runs.last_mut() {
                            Some((text, last)) if *last == style => text.push(symbol),
                            _ => runs.push((symbol.to_string(), style)),
                        }
                    }
                    let mut row = String::from("[");
                    for (i, (text, style)) in runs.iter().enumerate() {
                        let comma = if i == 0 { "" } else { "," };
                        write!(row, "{comma}[{},{style}]", json_string(text)).unwrap();
                    }
                    row.push(']');
                    rows.push(row);
                }
                writeln!(file, "[{}]", rows.join(",")).unwrap();
            }
        }
        file.flush().unwrap();
    }

    /// The demo on a simulated clock that moves a second per sample.
    struct Scripted {
        demo: crate::demo::Demo,
        at: f64,
    }

    impl Source for Scripted {
        fn sample(&mut self) {
            self.at += 1.0;
            self.demo.advance_to(self.at);
        }

        fn groups(&self) -> &[Group] {
            self.demo.groups()
        }

        fn flows(&self) -> &[Flow] {
            self.demo.flows()
        }

        fn badge(&self) -> Option<&str> {
            self.demo.badge()
        }
    }

    /// `text` as a JSON string.
    fn json_string(text: &str) -> String {
        let mut json = String::from('"');
        for c in text.chars() {
            match c {
                '"' => json.push_str("\\\""),
                '\\' => json.push_str("\\\\"),
                c if u32::from(c) < 0x20 => json.push_str(&format!("\\u{:04x}", u32::from(c))),
                c => json.push(c),
            }
        }
        json.push('"');
        json
    }

    /// An environment variable, or `default` when it's missing or invalid.
    fn var<T: std::str::FromStr>(name: &str, default: T) -> T {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// Draws `frames` animation frames, 50 ms apart.
    fn animate(
        terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
        app: &mut crate::app::App,
        frames: usize,
    ) {
        for _ in 0..frames {
            let mut lengths = Vec::new();
            terminal
                .draw(|frame| lengths = super::draw(frame, app))
                .unwrap();
            app.field.set_lengths(&lengths);
            app.field.step(0.05);
        }
    }

    fn print_screen(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) {
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let line: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            println!("{line}");
        }
    }
}
