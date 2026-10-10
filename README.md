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

Early. Per-drive graphs work today. The goal is to show data moving *between*
drives, animated, so you can watch a copy from `C:` to `D:` as it happens.

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
```

Press `q`, `Esc` or `Ctrl+C` to quit.

## Platforms

Drive stats come from [sysinfo](https://crates.io/crates/sysinfo), which
supports Windows, Linux and macOS. So far disktop has only been tested on
Windows 11.

## Roadmap

- Drive-to-drive flow: link reads on one drive to writes on another by the
  process doing them (ETW on Windows, eBPF on Linux)
- Animated particles along the flow between drive panels
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
