# Downloader 0.8.1 validation — 2026-11-26

Base: `a93b6f7` (0.8.0, see [validation-0.8.0.md](validation-0.8.0.md)). Scope: the disk-space precheck before a download starts now also runs on Linux and macOS (`statvfs` through `libc`). No other behaviour changed.

## Evidence (Windows 11, local)

| Check | Result | Evidence |
| --- | --- | --- |
| Format | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc fmt --all --check` |
| Strict Clippy | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc clippy -j 4 --release --workspace --all-targets --locked -- -D warnings` |
| Rust tests | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc test -j 4 --release --locked --workspace`: 251 passed, 0 failed, 2 ignored (core unit 88); new `util::tests::free_space_measures_the_volume` |
| `free_space` compiles and lints for Linux and macOS | PASS | The function, copied unchanged into a scratch crate with `libc = "=0.2.190"`, passes `cargo clippy -- -D warnings` for `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin` and `x86_64-pc-windows-msvc` |
| Precheck run on Linux and macOS | CI | `free_space_measures_the_volume` runs in the CI matrix (`ubuntu-latest`, `macos-latest`); result in the PR checks |
| Precheck on a nearly full disk | NOT_RUN | Not run against a nearly full volume |

## Limits

The precheck trusts what `statvfs` reports as available to unprivileged users (`f_bavail`); a quota smaller than the free space is not seen and is reported when a write fails. The other 0.8.0 limits are unchanged.
