use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::cursor::MoveTo;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::crossterm::{execute, queue};

use crate::flows::FlowField;
use crate::model::Source;
use crate::ui;

/// Longest step the dots take in one go, so a stall doesn't teleport them.
const MAX_STEP_SECS: f64 = 0.1;
/// Longest wait between frames when the terminal is slow to keep up.
const SLOWEST_FRAME: Duration = Duration::from_millis(500);
/// How long to wait for a key while paused.
const PAUSED_WAIT: Duration = Duration::from_secs(60);

pub struct App {
    pub source: Box<dyn Source>,
    pub field: FlowField,
    pub interval: Duration,
    /// Time between animation frames while flows are moving.
    frame: Duration,
    pub show_flows: bool,
    pub paused: bool,
    /// Rows kept for the flow list. It grows to fit the most flows seen and
    /// never shrinks, so the panels above don't jump as copies come and go.
    pub flow_list_rows: u16,
    should_quit: bool,
}

impl App {
    pub fn new(
        source: Box<dyn Source>,
        interval: Duration,
        dot_size: Option<f64>,
        fps: u32,
    ) -> Self {
        let mut field = FlowField::new(dot_size);
        field.sync(source.flows());
        let mut app = Self {
            source,
            field,
            interval,
            frame: Duration::from_secs(1) / fps.max(1),
            show_flows: true,
            paused: false,
            flow_list_rows: 0,
            should_quit: false,
        };
        app.fit_flow_list();
        app
    }

    /// Grows the flow list to fit the flows running now.
    fn fit_flow_list(&mut self) {
        self.flow_list_rows = self.flow_list_rows.max(ui::flow_list_rows(&self.field));
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut next_sample = Instant::now() + self.interval;
        let mut last_step = Instant::now();
        while !self.should_quit {
            // Each frame goes out as one update, so terminals that support it
            // show whole frames rather than half-drawn ones. Others ignore it.
            let drawing = Instant::now();
            let _ = queue!(terminal.backend_mut(), BeginSynchronizedUpdate);
            let mut route_lengths = Vec::new();
            let mut screen = ratatui::layout::Rect::default();
            terminal.draw(|frame| {
                screen = frame.area();
                route_lengths = ui::draw(frame, self);
            })?;
            // The cursor is hidden, but some terminals still outline it when
            // they're not focused, and it's left wherever the frame last
            // drew, often on a moving flow. Park it in the bottom-right
            // corner, at the empty end of the footer.
            let corner = MoveTo(
                screen.right().saturating_sub(1),
                screen.bottom().saturating_sub(1),
            );
            let _ = queue!(terminal.backend_mut(), corner);
            let _ = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
            self.field.set_lengths(&route_lengths);
            let drew_for = drawing.elapsed();

            // Sleep until the next sample, or the next frame while animating.
            let now = Instant::now();
            let mut wake = if self.paused {
                now + PAUSED_WAIT
            } else {
                next_sample
            };
            if self.is_animating() {
                wake = wake.min(now + frame_delay(self.frame, drew_for));
            }
            if event::poll(wake.saturating_duration_since(now))? {
                self.handle_event(event::read()?);
            }

            let now = Instant::now();
            if !self.paused && now >= next_sample {
                self.source.sample();
                self.field.sync(self.source.flows());
                self.fit_flow_list();
                next_sample = now + self.interval;
            }
            let dt = now
                .duration_since(last_step)
                .as_secs_f64()
                .min(MAX_STEP_SECS);
            last_step = now;
            if self.is_animating() {
                self.field.step(dt);
            }
        }
        Ok(())
    }

    fn is_animating(&self) -> bool {
        self.show_flows && !self.paused && !self.field.is_idle()
    }
}

/// How long to wait before the next frame. Writing to a terminal blocks
/// when it can't keep up, so a slow draw means a slow terminal: then wait
/// twice as long as the draw took, rather than piling on more output.
fn frame_delay(frame: Duration, drew_for: Duration) -> Duration {
    frame.max(drew_for * 2).min(SLOWEST_FRAME)
}

impl App {
    fn handle_event(&mut self, event: Event) {
        // Windows reports both press and release; only act on presses.
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            self.handle_key(key);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('f') => self.show_flows = !self.show_flows,
            KeyCode::Char('a') => self.field.set_style(self.field.style().toggled()),
            KeyCode::Char('p') | KeyCode::Char(' ') => self.paused = !self.paused,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(50);

    #[test]
    fn a_fast_terminal_gets_the_normal_frame_rate() {
        assert_eq!(frame_delay(FRAME, Duration::from_millis(2)), FRAME);
    }

    #[test]
    fn a_slow_terminal_gets_fewer_frames() {
        assert_eq!(
            frame_delay(FRAME, Duration::from_millis(80)),
            Duration::from_millis(160)
        );
        assert_eq!(frame_delay(FRAME, Duration::from_secs(3)), SLOWEST_FRAME);
    }
}
