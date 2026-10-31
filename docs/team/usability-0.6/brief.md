# Usability pass 0.6.0 — shared brief

Goal: make the desktop app easier for everyday humans: faster common actions,
clear feedback, keyboard use, less clutter. No core/protocol behaviour changes.

## Shared contract (already committed in `gui/ui/app.slint`, AppWindow)

```slint
in property <bool> notice-is-error;      // banner red only for errors, neutral for info
in-out property <string> search;         // list search text
callback search-changed(string);         // Rust re-filters (name or URL, case-insensitive)
callback clear-completed();              // remove all Completed from list, keep files
callback open-file();                    // open selected completed file with default app
callback select-relative(int);           // -1 up / +1 down in visible list
```
Existing callbacks stay (add-clicked, start-clicked, pause-clicked, del-open,
settings-clicked, open-folder, copy-url, row-clicked, row-double-clicked, ...).
Do not rename or remove any existing property/callback/accessible-label: tests
in `gui/src/tests/` look up "Compact window", "Minimize to tray", "Exit",
"Open folder" by accessible label.

## Owners (only touch your own files)

| Owner | Files |
|---|---|
| A slint-ui | `gui/ui/app.slint`, new icons under `gui/ui/icons/`, `gui/src/tests/lifecycle.rs` (only if a layout test must follow the new layout) |
| B rust-gui | `gui/src/main.rs`, `gui/src/view.rs`, `gui/src/desktop.rs`, `gui/src/reveal.rs`, new `gui/src/actions.rs`, `gui/src/tests/model_sync.rs` |
| C i18n/docs (wave 2) | `gui/lang/**/downloader.po`, `README.md`, `docs/usability.zh-TW.md` |
| Lead | `CHANGELOG.md`, Cargo versions, commits |

## Hard rules
- No `git add/commit/stash/checkout/reset`. Lead integrates.
- No new required dependencies. Keep files < 800 lines (main.rs is ~700: put new handlers in `actions.rs`).
- New user-visible text uses `@tr(...)` in slint; Rust text via `Tr` global functions only.
- Build and test only the GUI crate: `cargo test --release --locked -p downloader`
  and `cargo clippy --release -p downloader --all-targets --locked -- -D warnings`,
  `cargo fmt -p downloader --check`. Use your own `CARGO_TARGET_DIR` if told.
- Launching processes: argv only, never through a shell string.

## Report format
Files changed, what each change does, commands run with pass/fail output tail,
anything not done or unverified.
