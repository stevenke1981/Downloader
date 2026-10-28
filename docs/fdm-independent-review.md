# FDM independent integration review

- Task: `workflow757e52419ca54b53b2b224bb84be9981`, independent integrator `/root/fdm_integrator` (configured `gpt-6.1-sol/high`).
- Base: `6e5cb2bbdd3933b7a613505115defebe0212b318`.
- Initial reviewed candidate: `b77a0bdab646a38c5fa809bf9a1809f229ed678b`.
- Focused fix recheck candidate: `1aa385ac5cf1f20e978005273b32b54398d9251f` (R1 `cb4f5cd`, R2/R3 `81a4498`, direct `url` lock dependency `1aa385a`).
- Platform / cwd: Windows PowerShell, `E:\Downloader`.
- Scope: one independent review of the integrated diff, with focused follow-up of reported fixes only. Reviewer did not author product changes, run a second reviewer, commit, push, or execute destructive scheduled actions.
- Inputs: `PLAN.md`, `SPEC.md`, feature matrix, current validation document, actual base/head diff, implementations, existing tests and packaging scripts.

## Findings

### FDM-R1 - P2: removing an external task leaves its output directory and staging data

**Status: RESOLVED (focused static recheck PASS).** At the initial candidate, `core/src/manager.rs:455-466` snapshotted a task before awaiting its worker, but only called `remove_file` on the HTTP partial path and final path. `core/src/external.rs:434-440` publishes multi-file downloads as a directory. Directory removal therefore failed silently. External staging was never considered by removal, including when `file_name` was still empty.

Reproduction: complete a two-file Torrent/Metalink, select Delete with the file deletion checkbox, and confirm. The list entry disappears while the output directory and both files remain. Alternatively pause an external download with staged files, then remove the task: its `.dlx-external-*` directory remains without any queue entry able to resume it. The GUI reaches this path through `gui/src/main.rs:784-786`.

Fix inspected: removal awaits its worker, then reads the final task path. It checks canonical targets remain beneath the download directory, removes external final directories and task staging when `delete_file` is true, and retains final/staged external data when false. Empty names do not delete the root. The two new tests cover multi-file nested output, paused/error staging, explicit retention, unrelated/root/outside files, native partial cleanup, and worker publication completing during cancellation. Test names: `remove_cleans_external_directories_and_staging_only_when_requested` and `remove_waits_for_worker_publication_before_resolving_output`. Root reports author focused tests and strict Clippy PASS; this reviewer inspected test assertions without rerunning them.

### FDM-R2 - P2: FTP browser results cannot be enqueued

**Status: RESOLVED (focused static recheck PASS).** At the initial candidate, `core/src/external.rs:515-519` returned FTP entry names, such as `payload.bin`, and `gui/src/tools.rs:404-405` wrote these names directly into the collected-links field. The Add collected links callback at `gui/src/tools.rs:428-435` invoked `batch::enqueue`; `gui/src/batch.rs:21` requires every line to be a supported source URL or local descriptor.

Reproduction: list `ftp://127.0.0.1:PORT/directory/` containing `payload.bin`, then press Add collected links. Line 1 fails source validation instead of creating `ftp://127.0.0.1:PORT/directory/payload.bin`. Listing success alone does not cover this boundary.

Fix inspected: `ftp_directory` normalizes an FTP directory URL before both listing and resolution, rejects embedded credentials, and appends a missing trailing slash. `ftp_sources` appends each raw NLST name as an encoded path segment, preserving directory and origin. The callback populates collected links from this adapter. `ftp_listing_names_become_encoded_downloadable_sources` asserts exact URLs for ordinary, punctuation and Unicode names, both trailing-slash variants, credentials rejection and successful batch enqueue. The author directly reported this focused test and strict GUI all-targets Clippy PASS (exit 0). This reviewer did not rerun them. The new direct `url` dependency matches its existing locked package and is listed for `downloader` in Cargo.lock.

### FDM-R3 - P2: ordinary Add dialog overrides every site connection default

**Status: RESOLVED (focused static recheck PASS).** At the initial candidate, `gui/src/main.rs:493` initialized Add connections from the global setting, and `gui/src/desktop.rs:99` unconditionally passed `Some(add_conns)`. `core/src/manager.rs:275-278` uses `SiteRule.connections` only when that optional override is absent. Consequently normal GUI additions and batches never used the configured per-origin connection default; the collected-links path, which passes no override, behaved differently.

Reproduction: set global connections to 8 and the matching site's connections to 2. Add one of that site's URLs using the default form values. The resulting snapshot has 8 connections. A batch containing two sites also forces the same global count for both.

Fix inspected: Add now initializes to 0; the control allows 0 and explains automatic/site-default behavior in English and Traditional Chinese. `connection_override(0)` preserves `None`, while a positive choice is retained as an explicit override. `add_form_automatic_uses_each_site_default_and_preserves_override` checks form reset, a batch using site 2/global 8 and an explicit 5 override. Updated `open_add` call sites match its simplified signature. The author directly reported this focused test and strict GUI all-targets Clippy PASS (exit 0). This reviewer did not rerun them.

## Other reviewed contracts

- Static inspection found no additional blocking defect in token authentication and queue-only remote routes, bounded HTTP request parsing, persisted one-shot consumption before action execution, argv-based process launch, skipped serialization of Cookie/Authorization, exact-origin Basic authentication selection, or the `dlxctl` binary entries in Cargo, CI, Windows installer, release archive and macOS bundle scripts.
- External outputs are discovered within per-task staging and checked after a successful child exit; explicit checksums prevent publishing mismatched or multi-file outputs. This review does not replace the independent runtime verifier.
- Existing worker tests and reported real protocol results were input evidence, not tests executed by this reviewer. The initial tests did not cover the three failing GUI/lifecycle boundaries; the fixes now include focused coverage described above.

## Evidence and handoff

- PASS: review commands (`git status`, `git rev-parse HEAD`, `git diff`, `rg`, PowerShell file reads) completed with exit code 0 and exposed the call paths above. Initial candidate working tree was clean.
- PASS: the one focused fix recheck inspected `git diff b77a0bdab646a38c5fa809bf9a1809f229ed678b..1aa385ac5cf1f20e978005273b32b54398d9251f` for the three reported defects, with exit code 0. All three are resolved at that candidate. No remaining evidence-backed blocking finding within this review scope.
- NOT_RUN: integrated fmt/Clippy/build/tests, real protocol fixtures, native GUI operation, installer execution, other operating systems, Internet tunnel and CI. These remain the root / independent verifier's responsibility; no runtime PASS is inferred from static review.
- N/A: no external side effects, paid requests, publication or push by reviewer.
- Next owner: root / independent release verifier executes full acceptance on the repaired candidate and records actual commands, exit codes and artifact hashes. This static review PASS does not assert that full release, GUI, install or CI acceptance has passed.
