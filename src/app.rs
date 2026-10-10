use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::drives::DriveMonitor;
use crate::ui;

pub struct App {
    pub monitor: DriveMonitor,
    pub interval: Duration,
    should_quit: bool,
}

impl App {
    pub fn new(interval: Duration) -> Self {
        Self {
            monitor: DriveMonitor::new(),
            interval,
            should_quit: false,
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut next_sample = Instant::now() + self.interval;
        while !self.should_quit {
            terminal.draw(|frame| ui::draw(frame, self))?;

            let timeout = next_sample.saturating_duration_since(Instant::now());
            if event::poll(timeout)? {
                self.handle_event(event::read()?);
            }

            if Instant::now() >= next_sample {
                self.monitor.sample();
                next_sample = Instant::now() + self.interval;
            }
        }
        Ok(())
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
            _ => {}
        }
    }
}
