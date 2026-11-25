# Downloader 0.8.0 team plan (adviser: Fable 5.2)

Goal: close the 0.7.0 "Known limits": HTTP mirror fallback, hook events from `dlx`, `--mirror` flags, disk precheck advice.
Two implementers work in the SAME tree concurrently with disjoint files. Each sets its own CARGO_TARGET_DIR.

## Findings (line refs at d107c81)
- F1 `engine::run` (engine.rs:129) loops `run_once` (188) + `verify::verify_task`. `run_once` reads only `t.resolved_url.unwrap_or(t.url)` (196);
  `Task.mirrors` (task.rs:124) is never read by the engine. Errors are plain `String`s, so "may a mirror help?" cannot be told from the text alone.
- F2 Error sites in `run_once`: probe (236), conflict (283), mkdir (326), disk (342), open (353), workers (395-399), gaps (409/414), `file exists:` (435),
  size mismatch (447), rename (453). Workers: `FetchErr::Retry` exhausted (worker.rs:224), `FetchErr::Fatal` (95-140, 172, 190).
- F3 Resume check `changed` (engine.rs:287-296) clears segments when ETag / Last-Modified differ. Mirrors have other validators, so the check as written
  would wipe a `.part` on every mirror, and storing the mirror's ETag (311) would wipe it again on the next run against the primary.
- F4 Isolation today: cookie/Basic only if `same_origin(t.url, effective)` (218); user + module headers go to the effective origin via `with_extras`
  (http.rs:320) and are dropped after a cross-origin redirect (248); workers use the no-redirect client when headers exist (worker.rs:57, 94).
- F5 **Leak:** `Registry::prepare` appends resolver mirrors to the persisted `t.mirrors` (modules/mod.rs:362-367). They survive a restart, a revoke and
  `export` (manager/transfer.rs:69), and grow on every run. Harmless in 0.7 (never tried); with fallback a revoked module would keep steering downloads.
- F6 `remote.rs` `AddDownload` (266) has `deny_unknown_fields` and no `mirrors`; `NewDownload.mirrors` (models.rs:56) is reachable only from import.
  `Manager::try_add` caps user mirrors at 8 and runs `util::validate_source` only (manager.rs:289-294): an `ftp://` mirror can be stored.
- F7 `Registry::notify` (mod.rs:381-411) spawns detached tasks; `dlx` ends through `finish()` -> `process::exit` (dlx.rs:99-106), which would kill them
  (and skips `kill_on_drop`). `external::deliver` (external.rs:79-108) is already bounded: `timeout_ms` (<= 60 s) + 5 s kill wait, 4 permits.
- F8 The scheduler builds `HookEvent` inline (scheduler.rs:123-149): no event on cancel; empty `path` for `file exists:` or an unnamed file.
- F9 Sizes: dlxctl.rs 713, cli.rs 532, modules.rs 431, engine.rs 459, worker.rs 246, server/mod.rs 255. `core/tests/external.rs` forbids `Command::new`
  under engine/** and protocols/**. `Ctx` and `engine::run` signatures must not change (struct literals in dlx.rs, scheduler.rs, tests).
- F10 `Task.log` is persisted and bounded by `append_log` (task.rs:167, 100 lines). `site::url_origin` (site.rs:48) gives `scheme://host[:port]`.

## Design 1: mirror fallback (A)
- New `core/src/engine/mirrors.rs`: `struct Source { url, referer, cookie, authorization, headers, user_agent, mirror: Option<usize> }`,
  `fn sources(t: &Task) -> Vec<Source>`, `enum Fail { Source(String), Local(String) }`, `fn mirror_worthy(err: &str) -> bool`.
- `run_once` keeps its signature and becomes the loop; today's body moves to `run_source(task, ctx, &Source) -> Result<(Outcome, Option<String>), Fail>`.
  `Shared::worker` returns `Result<(), Fail>`. When several workers fail, a `Local` error wins over a `Source` error.
- Candidates: `[effective]`, then `t.mirrors`, then `t.module_mirrors` (only when `t.resolved_url.is_some()`); trimmed, de-duplicated, not equal to the
  effective URL, http(s) only, passing `util::validate_source`, not `protocols::handles`; at most 8 mirrors are tried. Bad entries are skipped silently.
- Fallback only on `Fail::Source`: probe error or worker `Retry` exhausted (network, 408, 429, 5xx), and (decision D1) other HTTP statuses such as 404/410.
  Never on: 401/403/407, `Google Drive:` errors, `file exists:`, `not enough disk space:`, mkdir/open/write/rename errors, Content-Range or
  "file changed" errors, size mismatch, gaps, verification failure, cancel (`Outcome::Cancelled` returns at once).
  D1 alternative if the lead wants the strict reading: make `mirror_worthy` false for every `permanent()` status. One-line switch.
- Mirror acceptance, decided right after the mirror's probe and BEFORE any task mutation:
  R1 known `t.total = n`: the mirror must report exactly `n`; otherwise skip it (refuse, do not restart).
  R2 resumable partial (`downloaded() > 0`, no open-ended segment, `.part` exists): the mirror must also support ranges; otherwise skip it. The `.part` stays.
  R3 otherwise (nothing downloaded, or size unknown / not resumable): start from byte 0 on the mirror with a fresh segment plan.
  A skipped mirror counts as failed; the loop goes on. For a mirror, `changed` ignores ETag / Last-Modified (R1/R2 replace them).
- State on a mirror: do NOT write `t.etag` / `t.last_modified` (the primary's validators stay, so a later run resumes against the primary);
  `Shared.etag` uses the mirror's own ETag for If-Range. `t.final_url`, `t.header_checksums`, `t.resumable` are set as today. Nothing new is persisted.
- File name on a mirror when `t.file_name` is empty: `suggested_name`, else `util::name_from_url(effective)` unless it is "download", else the mirror's name.
- Isolation per mirror (pure function, unit-tested): cookie + Basic only if `same_origin(t.url, mirror)`; `t.headers` and referer only if
  `same_origin(effective, mirror)`; `t.module_headers` only if `same_origin(resolved_url, mirror)`; User-Agent always. The existing post-probe redirect
  filtering (240-252) and the no-redirect worker client apply unchanged. `with_cookie_hint` applies to the primary only.
- All sources failed: return the PRIMARY's error unchanged (error class and exit code stay as in 0.7). Each mirror gets `ctx.retries` like the primary.
- Log (origin from `site::url_origin` only; never path, query or headers; no `eprintln!`): `HTTP source failed; trying mirror 1 of 2 (https://host)`,
  `HTTP mirror 1 skipped: size differs or no range support`, `HTTP download completed from mirror 1 (https://host)`. At most 2 lines per mirror.
- Verify retry (engine.rs:156-168) is unchanged: it starts again at the primary, then mirrors under R3.

## Design 2: hooks from dlx (B)
- `Registry::notify` stays (scheduler unchanged in behaviour). Add an awaitable variant and a shared event builder (see contract). `notify_wait` runs
  all subscribed hooks concurrently (`futures_util::future::join_all` over `external::deliver`) and returns when all ended; it ignores failures.
- dlx with `--modules-dir`: after each URL's result is reported, build the event with `HookEvent::for_result` (`id` is 0 in dlx; a prepare failure
  `module <id>: ...` is a `failed` event, as in the scheduler) and `tokio::spawn(registry.notify_wait(event))`; keep the JoinHandles.
  Before the final `finish()` await them all (bound: 65 s per hook, 4 at a time). In the interrupted branch (dlx.rs:380-391) and when Ctrl+C fires while
  waiting: `abort()` the handles and await them, so the children are dropped and killed, then exit 130. No event for a cancelled download.
- Output: nothing new on stdout or stderr; `summary` stays the last NDJSON line; exit codes ignore hook results.

## Design 3: --mirror (B)
- `dlx --mirror URL` (repeatable): each must be http(s) and pass `util::validate_source`, at most 8, and only with exactly one URL argument;
  otherwise usage error, exit 2. Set in the authoritative `Task` literal (dlx.rs:255-265): `mirrors`.
- `dlxctl add --mirror URL` (repeatable) -> body `"mirrors": [...]`; `remote.rs` `AddDownload` gains `#[serde(default)] mirrors: Vec<String>` mapped to
  `NewDownload.mirrors`. Keep dlxctl.rs < 800: move the `add` body builder into `core/src/bin/dlxctl/` if needed.

## Design 4: disk precheck off Windows (advice, no owner)
A correct `statvfs` needs either `libc` as a direct dependency (new crate edge: needs the user's approval; it is already in Cargo.lock transitively) or a
hand-declared `#[repr(C)] struct statvfs`, whose layout differs between Linux and macOS (`fsblkcnt_t` is 32-bit on macOS) and between 32/64-bit; a
wrong layout gives silent false "not enough disk space" failures and cannot be checked on the Windows dev box. Spawning `df` breaks the process rule.
Recommendation: do not hand-roll; keep the limit documented. If the user approves `libc` under `[target.'cfg(unix)'.dependencies]`, it is a 15-line
follow-up in util.rs. Free improvement for A now: in worker.rs:190 map `io::ErrorKind::StorageFull` to
`not enough disk space: write failed: {e}` so a full disk gets the `disk` class on every OS (other write errors keep `write failed: {e}`).

## Wave 0 contract (lead lands it first; it compiles with no behaviour change)
- task.rs: `#[serde(skip)] pub module_mirrors: Vec<String>` ("Mirrors a resolver module returned. Never persisted."), next to `module_headers`.
- modules/mod.rs signatures (B fills the bodies):
```rust
impl Registry { pub async fn notify_wait(&self, event: HookEvent); }   // awaits every subscribed hook; never fails
impl HookEvent { pub fn for_result(task: &Task, result: &Result<Outcome, String>) -> Option<HookEvent>; } // None when cancelled
```
- B changes `prepare`: clear `t.module_mirrors` at the top with `resolved_url` (mod.rs:291-296) and write resolver mirrors there instead of `t.mirrors`.
- A reads `t.mirrors`, `t.module_mirrors`, `t.resolved_url`, `t.module_headers`. `Ctx`, `engine::run`, `Outcome`, `NewDownload` are unchanged.
- Stable texts A must not reword: `file exists:`, `not enough disk space:`, `HTTP <code> ...`, `verification failed`.

## Implementer A (engine)
Owns: core/src/engine.rs, core/src/engine/** (+ new mirrors.rs), core/src/task.rs, core/tests/server/mod.rs, new core/tests/mirrors.rs,
core/tests/download.rs, core/tests/essentials.rs.
Fixture additions (additive fields with defaults in `start()`): `fail_status: AtomicUsize` (main file answers that status), `fail_after: AtomicU64`
(close the connection after N data bytes), `heads: Mutex<Vec<String>>` (raw request heads, to assert what a mirror received).
Tests (retries 0 or 1 to stay fast; `localhost` vs `127.0.0.1` gives a second origin, as download.rs:344 does):
1. primary 503 -> mirror completes; log names the mirror origin and not its `?token=`; `t.etag` is not the mirror's.
2. primary 404 -> mirror used (D1); primary 403 -> zero mirror requests, error `HTTP 403 ...`.
3. cross-origin mirror receives no Cookie, Authorization, custom header, module header or Referer, but the User-Agent; a same-origin mirror gets them.
4. primary dies mid-transfer -> same-size range mirror finishes the `.part` (mirror `bytes_sent` < total; file bytes equal).
5. different-size mirror skipped, next one used; all skipped -> the primary's error, `.part` kept. 6. no-range mirror skipped for a resumable partial.
7. `file exists:` (skip), checksum mismatch and cancel never contact a mirror. 8. `module_mirrors` come after user mirrors, only with `resolved_url`,
   and are absent from `serde_json::to_string(&task)`. 9. `ftp://` and duplicate mirrors ignored; at most 8 tried. 10. StorageFull mapping (unit).

## Implementer B (agent surfaces)
Owns: core/src/modules/**, core/src/bin/dlx.rs, core/src/bin/dlxctl.rs, core/src/bin/dlxctl/**, core/src/bin/shared/cli.rs, core/src/bin/module_example.rs,
core/src/remote.rs, core/src/manager/scheduler.rs (only to call `HookEvent::for_result`), core/tests/cli.rs, core/tests/modules.rs,
core/tests/management_services.rs, docs/cli.md, docs/modules.md.
Tests: modules.rs: `notify_wait` returns only after the hook wrote its log line; a `--sleep` hook with `timeout_ms` 1000 returns in under 10 s;
resolver mirrors land in `module_mirrors`, are cleared by the next `prepare`, and never serialize. cli.rs: dlx + approved hook module -> `events.log`
has exactly one `completed` line as soon as dlx exits, stdout NDJSON still ends with `summary`, human stdout is byte-identical in shape to 0.7;
a 404 URL gives one `failed` line and the exit code is unchanged; no `--modules-dir` -> no hook; `dlx --mirror` with two URLs or `ftp://` exits 2;
`dlxctl add --mirror` against `serve`, then `dlxctl export` shows the mirrors. If cli.rs nears 800 lines, add core/tests/cli_hooks.rs.
Docs: cli.md option rows for `--mirror` (both tools), replace the sentence at cli.md:78, describe fallback rules R1-R3, isolation and D1;
modules.md:60-61 and :78 (dlx waits for hooks before exit; resolver mirrors are tried and never saved).
Not owned by anyone (lead): CHANGELOG.md, docs/validation-0.8.0.md, versions, README, AGENTS.md, core/src/util.rs, manager.rs, manager/transfer.rs.

## Verification (PowerShell, repository root)
```powershell
$env:CARGO_TARGET_DIR = 'F:\dl-target-a'   # B: 'F:\dl-target-b'
cargo +1.96.1-x86_64-pc-windows-msvc clippy --release -p downloader-core --all-targets --locked -- -D warnings
cargo +1.96.1-x86_64-pc-windows-msvc test --release --locked -p downloader-core --lib --test mirrors --test download --test essentials --test external   # A
cargo +1.96.1-x86_64-pc-windows-msvc test --release --locked -p downloader-core --lib --test cli --test modules --test management_services --test external # B
rustfmt +1.96.1-x86_64-pc-windows-msvc --edition 2024 --check <each file you own>
```
Lead, after both land: full `cargo +1.96.1-x86_64-pc-windows-msvc test --release --locked --workspace`, workspace clippy, `cargo fmt --all --check`.
The other implementer's half-finished files can break your build: wait and retry, report it, never edit their file.

## Risks
- Mixed bytes from two sources with no checksum: R1/R2 limit it to same-size files; verification still catches it when a digest exists. Document it.
- D1 (404 falls back) widens the brief's "retryable" wording; the lead confirms or flips `mirror_worthy`.
- Changing `prepare` (F5) drops resolver mirrors from saved tasks; 0.7 `downloads.json` files may still hold old ones in `mirrors` (they stay user mirrors).
- A hung hook delays dlx exit by up to its `timeout_ms` + 5 s; Ctrl+C must still exit promptly (test it by hand if not automatable).
- Fixture changes touch every integration test: additive fields only, defaults keep old behaviour.
- A returning the last mirror's error would change exit codes: return the primary's.

## Hard rules for both implementers
- No git add/commit/stash/checkout/reset/restore. The lead integrates.
- Touch only files you own. Anything needed in another owner's file goes in your report.
- No new crates; Cargo.toml and Cargo.lock unchanged. Every source file < 800 lines.
- Processes argv only and only in core/src/modules/external.rs; nothing under engine/** or protocols/** spawns.
- Default human output of `dlx` and `dlxctl` is unchanged; new machine output is additive behind `--json`.
- Keep the stable error prefixes. No tokens, cookies, headers or URL queries in logs, state files, JSON output or docs.
- Module approval only through `dlxctl modules approve` or the app's Approve button.
