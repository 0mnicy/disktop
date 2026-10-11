use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::flows::FlowField;
use crate::model::Source;
use crate::ui;

/// Time between animation frames while flows are moving (about 30 fps).
const FRAME: Duration = Duration::from_millis(33);
/// Longest step the dots take in one go, so a stall doesn't teleport them.
const MAX_STEP_SECS: f64 = 0.1;
/// How long to wait for a key while paused.
const PAUSED_WAIT: Duration = Duration::from_secs(60);

pub struct App {
    pub source: Box<dyn Source>,
    pub field: FlowField,
    pub interval: Duration,
    pub show_flows: bool,
    pub paused: bool,
    /// Rows kept for the flow list. It grows to fit the most flows seen and
    /// never shrinks, so the panels above don't jump as copies come and go.
    pub flow_list_rows: u16,
    should_quit: bool,
}

impl App {
    pub fn new(source: Box<dyn Source>, interval: Duration, dot_size: Option<f64>) -> Self {
        let mut field = FlowField::new(dot_size);
        field.sync(source.flows());
        let mut app = Self {
            source,
            field,
            interval,
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
            let mut route_lengths = Vec::new();
            terminal.draw(|frame| route_lengths = ui::draw(frame, self))?;
            self.field.set_lengths(&route_lengths);

            // Sleep until the next sample, or the next frame while animating.
            let now = Instant::now();
            let mut wake = if self.paused {
                now + PAUSED_WAIT
            } else {
                next_sample
            };
            if self.is_animating() {
                wake = wake.min(now + FRAME);
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
            KeyCode::Char('p') | KeyCode::Char(' ') => self.paused = !self.paused,
            _ => {}
        }
    }
}
