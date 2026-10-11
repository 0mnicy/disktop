mod app;
mod attribution;
mod demo;
mod drives;
#[cfg(windows)]
mod etw;
mod flows;
mod format;
mod layout;
mod model;
// Used on Linux; on Windows only its tests run.
#[cfg_attr(windows, allow(dead_code))]
mod procio;
mod sysfs;
mod ui;
#[cfg(windows)]
mod win;

use std::time::Duration;

use anyhow::Result;
use clap::Parser;

use crate::app::App;
use crate::demo::Demo;
use crate::drives::DriveMonitor;
use crate::model::Source;

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

    /// Show made-up arrays and copies instead of your real drives.
    #[arg(long)]
    demo: bool,

    /// How much data each flow dot stands for, such as 1M or 512K (binary
    /// units). By default it adjusts to the busiest flow.
    #[arg(long, value_name = "SIZE", value_parser = format::parse_size)]
    dot_size: Option<f64>,

    /// Animation frames per second while flows are moving. Lower it if
    /// your terminal struggles to keep up.
    #[arg(
        long,
        default_value_t = 20,
        value_parser = clap::value_parser!(u32).range(1..=60)
    )]
    fps: u32,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let source: Box<dyn Source> = if args.demo {
        Box::new(Demo::new())
    } else {
        Box::new(DriveMonitor::new())
    };
    let mut app = App::new(
        source,
        Duration::from_millis(args.interval),
        args.dot_size,
        args.fps,
    );
    ratatui::run(|terminal| app.run(terminal))
}
