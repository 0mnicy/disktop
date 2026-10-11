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

Early. Per-drive graphs work today. RAID arrays get one panel with a graph
for each member disk inside it; on Linux, btrfs filesystems that span
several disks are detected and grouped this way. Data moving *between*
drives is drawn as dots marching along a route from one panel to another.
On Linux these flows are real: disktop sees which process is copying from
one drive to another. Elsewhere, `--demo` shows the flow view with
made-up data.

### How flows are found on Linux

Every sample, disktop reads each process's storage I/O counters from
`/proc`. A process that both read and wrote is a possible copy, so its open
files are checked: which ones it opened for reading and for writing, and
which drive each one is on. A process reading files on one drive and
writing files on another is drawn as a flow between them, at the smaller of
its read and write rates, and labeled with the process name. Files are
traced to a drive through the process's own mount table, so programs in
containers are covered too.

Without root, only your own processes can be inspected; the footer says so.
Run `sudo disktop` to see everything. Flows under 512 KiB/s are left out,
and so is a process reading and writing the same drive, which is usually a
database or a log rather than a copy.

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
disktop --dot-size 1M   # make every flow dot stand for 1 MiB
```

| Key | Action |
| --- | --- |
| `f` | Show or hide flows |
| `p` or `Space` | Pause |
| `q`, `Esc` or `Ctrl+C` | Quit |

### Reading the flow view

Each copy between drives gets its own color, a dim track showing its route,
and bright dots moving along it from source to destination. Every flow
leaves and enters a panel at its own point and runs in its own lane in the
channel between the columns, so streams don't pile on top of each other.

Every dot stands for the same amount of data across all flows, like the
scale on a map, and the list under the panels says how much (`each dot is
50 MiB`). Dots all move at the same speed, so a flow with twice as many
dots is moving twice as much data. The amount is a round size picked so the
busiest flow shows a readable number of dots; it only changes when the
busiest flow speeds up or slows down a lot. Use `--dot-size` to fix it.

Inside an array, each dot lands on as many disks as the array keeps copies:
every disk for a classic mirror, two neighbouring disks at a time for btrfs
RAID 1 or RAID 10, and one disk at a time for a stripe. When there isn't
room for a box per disk, member disks are drawn as compact rows.

## Platforms

Drive stats come from [sysinfo](https://crates.io/crates/sysinfo), which
supports Windows, Linux and macOS. On Linux, multi-disk btrfs filesystems
and per-disk I/O are read from `/sys` and `/proc/diskstats`, and flows from
`/proc`, none of which needs root. Tested on Windows 11 and Ubuntu 24.04.

## Roadmap

- Detect more arrays: Storage Spaces on Windows; md, LVM and ZFS on Linux
- Detect real flows on Windows (ETW)
- Show copies arriving over the network, which have no source drive
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
