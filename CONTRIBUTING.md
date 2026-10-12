# Contributing to disktop

Thanks for helping. Bug reports, ideas and pull requests are all welcome. For
anything big, open an issue first so we can agree on the approach.

## Building and testing

You need Rust 1.95 or newer, the version in `rust-version` in `Cargo.toml`.
CI runs these on Linux, Windows and macOS, so run them before sending a pull
request:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Platform-specific code is behind `cfg(windows)` or reads Linux's `/proc` and
`/sys`. Tests for the Linux side use small fake directory trees, so they run
everywhere. To lint the Linux code from Windows, add the target with
`rustup target add x86_64-unknown-linux-gnu` and pass
`--target x86_64-unknown-linux-gnu` to `cargo clippy`.

Every `unsafe` block needs a `// SAFETY:` comment saying why it's sound;
Clippy enforces this.

## Checking flows on a real machine

A few ignored tests print what disktop sees as text, which is handy over SSH
or from an Administrator window where watching the app isn't practical:

```sh
# The demo at one moment. COLS and ROWS set the size, AT the second of the
# demo, and STYLE=arrows draws arrows.
cargo test print_demo -- --ignored --nocapture

# This machine's real drives and flows, once a second for SECS seconds.
# Run with sudo on Linux, or as Administrator on Windows, to see every flow.
cargo test print_live -- --ignored --nocapture

# Windows, as Administrator: what the kernel reports about one command's
# file I/O, for working out how a program's copies look to ETW.
CMD="copy a b" cargo test print_events -- --ignored --nocapture
```

## Recording the animations

The GIFs in the README are drawn by the app's own UI code from `--demo`, on a
simulated clock, so they're the same every time. After changing how anything
looks, record them again:

```sh
python scripts/record.py
```

This needs Python 3.9 or newer, [Pillow](https://pypi.org/project/pillow/),
ffmpeg, and a monospace font such as Cascadia Mono (pass another with
`--font`). The scenes, theme and size are at the top of `scripts/record.py`.

## Releasing

Releases are built and published by the [release workflow](.github/workflows/release.yml)
when a version tag is pushed.

1. On a new branch, set the new version in `Cargo.toml` and run
   `cargo check` to update `Cargo.lock`.
2. In `CHANGELOG.md`, rename `Unreleased` to the version and today's date,
   start a new empty `Unreleased` section, and update the links at the bottom.
3. Commit as `Release 0.2.0` and open a pull request. `main` only takes
   changes that pass CI, so merge it once the checks are green.
4. Tag the merged commit and push the tag:

   ```sh
   git switch main
   git pull
   git tag -a v0.2.0 -m "disktop 0.2.0"
   git push origin v0.2.0
   ```

The workflow checks that the tag matches `Cargo.toml`, builds static binaries
for Windows and Linux on x86-64 and ARM64, and publishes them with checksums,
build provenance and that version's notes from the changelog. A version with a
suffix, such as `0.2.0-rc.1`, is published as a prerelease.

## License

Unless you say otherwise, anything you submit for inclusion in disktop is
dual licensed under the MIT and Apache 2.0 licenses, as described in the
[README](README.md#license), without any additional terms or conditions.
