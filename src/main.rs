mod app;
mod drives;
mod format;
mod ui;

use std::time::Duration;

use anyhow::Result;
use clap::Parser;

use crate::app::App;

/// A top-style terminal monitor for your drives.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// How often to sample the drives, in milliseconds.
    #[arg(
        short,
        long,
        default_value_t = 1000,
        value_parser = clap::value_parser!(u64).range(100..)
    )]
    interval: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut app = App::new(Duration::from_millis(args.interval));
    ratatui::run(|terminal| app.run(terminal))
}
