use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::symbols::{self, Marker};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, BorderType, Chart, Dataset, GraphType, LineGauge, Paragraph};
use sysinfo::DiskKind;

use crate::app::App;
use crate::drives::{Drive, Rate};
use crate::format;

const READ_COLOR: Color = Color::Cyan;
const WRITE_COLOR: Color = Color::Magenta;

/// Narrowest a drive panel gets before the grid drops a column.
const MIN_PANEL_WIDTH: u16 = 60;

/// The graph's y-axis never shrinks below this, so an idle drive draws a flat
/// line instead of magnifying a few bytes of noise.
const MIN_GRAPH_SCALE: f64 = 64.0 * 1024.0;

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app);
    draw_drives(frame, body, app.monitor.drives());
    draw_footer(frame, footer);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let total = app.monitor.total();
    let count = app.monitor.drives().len();
    let left = Line::from(vec![
        " disktop ".bold().reversed(),
        format!("  {count} drive{}  ", if count == 1 { "" } else { "s" }).into(),
        rate_span("read", total.read, READ_COLOR),
        "  ".into(),
        rate_span("write", total.write, WRITE_COLOR),
    ]);
    let right = Line::from(format!("every {:.1}s ", app.interval.as_secs_f64()))
        .dark_gray()
        .right_aligned();

    frame.render_widget(left, area);
    frame.render_widget(right, area);
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    let keys = Line::from(vec![" q".bold(), " quit".dark_gray()]);
    frame.render_widget(keys, area);
}

fn draw_drives(frame: &mut Frame, area: Rect, drives: &[Drive]) {
    if drives.is_empty() {
        let empty = Paragraph::new("No drives found").dark_gray().centered();
        frame.render_widget(empty, area);
        return;
    }

    let columns = usize::from(area.width / MIN_PANEL_WIDTH).clamp(1, drives.len());
    let rows = drives.len().div_ceil(columns);
    let row_areas = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(area);

    for (row_area, row_drives) in row_areas.iter().zip(drives.chunks(columns)) {
        let cells = Layout::horizontal(vec![Constraint::Ratio(1, columns as u32); columns])
            .split(*row_area);
        for (cell, drive) in cells.iter().zip(row_drives) {
            draw_drive(frame, *cell, drive);
        }
    }
}

fn draw_drive(frame: &mut Frame, area: Rect, drive: &Drive) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().dark_gray())
        .title(drive_title(drive));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [capacity, rates, graph] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    // Braille packs two samples into each terminal column.
    let samples: Vec<Rate> = drive.recent(usize::from(graph.width) * 2).collect();
    let peak = samples
        .iter()
        .map(|rate| rate.read.max(rate.write))
        .fold(0.0, f64::max);

    frame.render_widget(capacity_gauge(drive), capacity);
    frame.render_widget(rates_line(drive.current(), peak), rates);
    draw_graph(frame, graph, drive, &samples, peak);
}

fn drive_title(drive: &Drive) -> Line<'static> {
    let mut details = Vec::new();
    if !drive.label.is_empty() {
        details.push(drive.label.clone());
    }
    if !drive.file_system.is_empty() {
        details.push(drive.file_system.clone());
    }
    match drive.kind {
        DiskKind::SSD => details.push("SSD".to_string()),
        DiskKind::HDD => details.push("HDD".to_string()),
        DiskKind::Unknown(_) => {}
    }
    if drive.removable {
        details.push("removable".to_string());
    }

    Line::from(vec![
        " ".into(),
        drive.mount.display().to_string().bold(),
        "  ".into(),
        details.join(" · ").dark_gray(),
        " ".into(),
    ])
}

fn capacity_gauge(drive: &Drive) -> LineGauge<'static> {
    let ratio = drive.used_ratio().clamp(0.0, 1.0);
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
        format::bytes(drive.used_space() as f64),
        format::bytes(drive.total_space as f64),
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
        format!("   peak {}", format::rate(peak)).dark_gray(),
    ])
}

fn rate_span(name: &str, bytes_per_sec: f64, color: Color) -> Span<'static> {
    Span::styled(
        format!("● {name} {}", format::rate(bytes_per_sec)),
        Style::new().fg(color),
    )
}

fn draw_graph(frame: &mut Frame, area: Rect, drive: &Drive, samples: &[Rate], peak: f64) {
    if area.height == 0 {
        return;
    }
    if !drive.has_samples() {
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
    .y_axis(Axis::default().bounds([0.0, peak.max(MIN_GRAPH_SCALE) * 1.15]));

    frame.render_widget(chart, area);
}
