# Changelog

All notable changes to disktop are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Each graph has a scale on its left, so you can tell what it's showing:
  the rate at its top, a round number such as 50 MiB/s just above the
  busiest recent moment, and zero at the bottom.
- Releases are published to crates.io, so disktop installs with
  `cargo install disktop`, and `cargo binstall disktop` downloads the
  prebuilt binary for your system.

### Changed

- Graphs top out at that round number rather than a little above their
  peak, so they can be read against the scale.
- Flow dots are easier to see, even on a short hop between neighbouring
  panels: each has a thick bright head, and the tracks they run along are
  drawn in faint gray, so only what's moving is in color.

### Fixed

- Terminals that outline the cursor while they're not focused, even when
  it's hidden, no longer show it sitting on a moving flow: disktop parks it
  in the bottom-right corner after each frame.

## [0.1.0] - 2026-10-11

The first release.

### Added

- A panel for every drive, with a capacity bar and a live graph of reads and
  writes.
- RAID arrays shown as one panel with a graph for each member disk. On Linux,
  btrfs filesystems that span several disks are grouped this way.
- Copies between drives, drawn as dots or dashed arrows moving from one panel
  to another and named after the program doing the copying. They're traced
  from `/proc` on Linux and from Event Tracing for Windows.
- A Network panel, with downloads and uploads drawn as flows to and from it.
- `--demo`, which shows made-up arrays and copies.
- Options for the sample interval (`--interval`), how flows are drawn
  (`--style`), how much data each dot stands for (`--dot-size`) and the frame
  rate (`--fps`), and keys to show or hide flows, switch styles and pause.
- Prebuilt binaries for Windows and Linux, on x86-64 and ARM64.

[Unreleased]: https://github.com/0mnicy/disktop/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/0mnicy/disktop/releases/tag/v0.1.0
