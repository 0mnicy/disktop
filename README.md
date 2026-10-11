# disktop

A top-style terminal monitor for your drives. Every drive gets its own panel
with a capacity bar and a live graph of reads and writes.

```
 disktop   3 drives  ● read 0 B/s  ● write 1.6 MiB/s               every 0.5s
╭ C:\  Main Drive · NTFS · SSD ────────────────────────────────────────────╮
│ 93% 889.1 GiB / 952.6 GiB ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━│
│● read 0 B/s   ● write 1.6 MiB/s   peak 2.0 GiB/s                         │
│                                                                    ⢸     │
│                                                                    ⡆⡄    │
│                                                                   ⢸⢱⡇    │
│                                                                  ⡇⢸⢸⡇    │
│                                                           ⠠⡠⣀⢄⡠⣀⢼⢆⣀⠘⣄⢄⡠⣀⣀│
╰──────────────────────────────────────────────────────────────────────────╯
```

## Status

Early. Per-drive graphs work today. Data moving *between* drives is drawn as
dots marching along a route from one panel to another, and RAID arrays get
one panel with their member disks inside it. Both currently run on made-up
data in `--demo` mode; detecting real arrays and real copies is next.

## Install

You need Rust 1.95 or newer.

```sh
cargo install --git https://github.com/0mnicy/disktop
```

Or build from a clone:

```sh
git clone https://github.com/0mnicy/disktop
cd disktop
cargo run --release
```

## Usage

```sh
disktop                 # sample once a second
disktop --interval 250  # sample every 250 ms (minimum 100)
disktop --demo          # made-up arrays and copies, to see the flow view
```

| Key | Action |
| --- | --- |
| `f` | Show or hide flows |
| `p` or `Space` | Pause |
| `q`, `Esc` or `Ctrl+C` | Quit |

### Reading the flow view

Each copy between drives gets its own color, a dim track showing its route,
and bright dots moving along it from source to destination. Faster copies
have tighter, quicker dots. Every flow leaves and enters a panel at its own
point and runs in its own lane in the channel between the columns, so
streams don't pile on top of each other.

Inside an array, a mirror (RAID 1) copies every dot to each disk, and a
stripe (RAID 0) sends the dots to its disks in turn.

## Platforms

Drive stats come from [sysinfo](https://crates.io/crates/sysinfo), which
supports Windows, Linux and macOS. So far disktop has only been tested on
Windows 11.

## Roadmap

- Detect real arrays: Storage Spaces on Windows, md and LVM on Linux
- Detect real flows: link reads on one drive to writes on another by the
  process doing them (ETW on Windows, eBPF on Linux)
- Focus mode to highlight one flow and dim the rest
- Per-process view of who is hitting each drive
- Keyboard controls for interval, layout and hiding drives

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
