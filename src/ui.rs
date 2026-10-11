use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::symbols::{self, Marker};
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::widgets::{Axis, Block, BorderType, Chart, Dataset, GraphType, LineGauge, Paragraph};

use crate::app::App;
use crate::flows::{FlowField, Route};
use crate::format;
use crate::layout::{self, Board, Panel, Point, RouteEnds, Split};
use crate::model::{ArrayLevel, Group, GroupKind, History, Member, Rate};

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
/// The flow list's route column is at least this wide, so it rarely changes.
const MIN_ROUTE_WIDTH: usize = 28;
/// Distance between the dots that draw a track, in cells.
const TRACK_STEP: f64 = 0.5;
/// How far behind each dot its tail sits, in cells.
const DOT_TAIL: f64 = 0.5;

/// The graph's y-axis never shrinks below this, so an idle drive draws a flat
/// line instead of magnifying a few bytes of noise.
const MIN_GRAPH_SCALE: f64 = 64.0 * 1024.0;

/// Draws the whole screen. Returns the on-screen length of each flow route,
/// in the order of [`FlowField::routes`], so the engine knows where they end.
pub fn draw(frame: &mut Frame, app: &App) -> Vec<Option<f64>> {
    let groups = app.source.groups();
    let listed = listed_flows(&app.field);
    let list_rows = if app.show_flows && !listed.is_empty() {
        listed.len().min(MAX_LISTED_FLOWS) as u16 + 1
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
        match group.kind {
            GroupKind::Standalone => draw_standalone(frame, panel.area, group),
            GroupKind::Array(level) => draw_array(frame, panel, group, level),
        }
    }

    let plans = plan_routes(&board, &app.field, groups);
    if app.show_flows {
        draw_flows(frame, body, &app.field, &plans);
        draw_flow_list(frame, flow_list, &listed, groups);
    }
    plans
        .iter()
        .map(|plan| plan.as_ref().map(layout::RoutePaths::length))
        .collect()
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let groups = app.source.groups();
    let total: Rate = groups.iter().map(Group::current).sum();
    let disks: usize = groups.iter().map(|g| g.members.len()).sum();
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
    let right = Line::from(format!("every {:.1}s ", app.interval.as_secs_f64()))
        .dark_gray()
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
    keys.extend(["p".bold(), " pause".dark_gray()]);
    frame.render_widget(Line::from(keys), area);

    let mut state = Vec::new();
    if !app.show_flows {
        state.push("flows hidden");
    }
    if app.paused {
        state.push("paused");
    }
    if !state.is_empty() {
        let line = Line::from(format!("{} ", state.join(" · ")))
            .yellow()
            .right_aligned();
        frame.render_widget(line, area);
    }
}

fn draw_standalone(frame: &mut Frame, area: Rect, group: &Group) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().dark_gray())
        .title(group_title(group));
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

fn draw_array(frame: &mut Frame, panel: &Panel, group: &Group, level: ArrayLevel) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ARRAY_COLOR))
        .title(group_title(group));
    frame.render_widget(block, panel.area);

    let parts = layout::array_parts(panel.area, group.members.len(), panel.channel_side);
    let [capacity, status] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(parts.header);
    frame.render_widget(capacity_gauge(group), capacity);

    let layout_word = match level {
        ArrayLevel::Mirror => "mirror",
        ArrayLevel::Stripe => "stripe",
    };
    let total = group.current();
    let status_line = Line::from(vec![
        format!("{layout_word} · {}   ", plural(group.members.len(), "disk")).dark_gray(),
        rate_span("read", total.read, READ_COLOR),
        "   ".into(),
        rate_span("write", total.write, WRITE_COLOR),
    ]);
    frame.render_widget(status_line, status);

    for (member, area) in group.members.iter().zip(&parts.members) {
        draw_member(frame, *area, member);
    }
}

fn draw_member(frame: &mut Frame, area: Rect, member: &Member) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().dark_gray())
        .title(Line::from(vec![
            " ".into(),
            member.name.clone().bold(),
            "  ".into(),
            member.details.clone().dark_gray(),
            " ".into(),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [rates, graph] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
    let samples = recent_samples(&member.history, graph);
    frame.render_widget(rates_line(member.history.current(), peak(&samples)), rates);
    draw_graph(frame, graph, &member.history, &samples);
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
    Line::from(vec![
        rate_span("read", current.read, READ_COLOR),
        "   ".into(),
        rate_span("write", current.write, WRITE_COLOR),
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
            let target_split = match groups[to].kind {
                GroupKind::Array(ArrayLevel::Mirror) => Split::All,
                _ => Split::Alternate,
            };
            Some(RouteEnds {
                from,
                to,
                from_disks: disks(&groups[from]),
                to_disks: disks(&groups[to]),
                target_split,
            })
        })
        .collect();
    board.plan(&ends)
}

/// Draws each flow's track in a dim color and its dots in a bright one, on
/// top of the panels.
fn draw_flows(
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
            for distance in [dot.distance, dot.distance - DOT_TAIL] {
                if distance >= 0.0 {
                    dots[color].extend(
                        plan.points_at(distance, Some(dot.id))
                            .into_iter()
                            .map(to_canvas),
                    );
                }
            }
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
            // Dots go on their own layer so they replace the track under them.
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

fn draw_flow_list(frame: &mut Frame, area: Rect, flows: &[ListedFlow], groups: &[Group]) {
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

    let mut lines = vec![Line::from(" flows".dark_gray())];
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

    fn demo_app_at(t: f64) -> App {
        let mut demo = Demo::new();
        demo.advance_to(t);
        App::new(Box::new(demo), Duration::from_secs(1))
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
    fn renders_at_any_size_without_panicking() {
        for (width, height) in [(160, 50), (78, 40), (30, 10), (5, 3), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            animate(&mut terminal, &mut demo_app_at(30.0), 10);
        }
    }
}

#[cfg(test)]
mod snapshot {
    /// Prints the demo as text. Run with
    /// `cargo test print_demo -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_demo() {
        use std::time::Duration;

        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let width = std::env::var("COLS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(160);
        let height = std::env::var("ROWS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);
        let at = std::env::var("AT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(23.0);

        let mut demo = crate::demo::Demo::new();
        demo.advance_to(at);
        let mut app = crate::app::App::new(Box::new(demo), Duration::from_secs(1));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        for _ in 0..120 {
            let mut lengths = Vec::new();
            terminal
                .draw(|frame| lengths = super::draw(frame, &app))
                .unwrap();
            app.field.set_lengths(&lengths);
            app.field.step(0.033);
        }
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let line: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            println!("{line}");
        }
    }
}
