# Downloader improvement dispatch — 2026-10-22

Base: `f3f83664ec1ebc80337c72ff5c4dba1de17062b8` (0.5.1). The working tree was clean at intake.

The user authorized project review, focused optimization, UI/UX improvements, browser-extension installation in app Settings, and commit/push. GitHub Actions quota is exhausted: run checks locally and include `[skip ci]` in pushed commit messages. Do not dispatch release workflows.

The user subsequently authorized upgrading the locally installed app and installing the extension in each browser. Root owns this additional scope: bump to 0.5.2, build a local installer, preserve current settings/download records, upgrade the existing installation, and verify the installed runtime. Browser management is blocked by the browser-use URL policy; do not work around the refusal with CDP, command-line loading, profile edits or another browser-control surface. Prepare the extension and connector, then hand off the browser's Load unpacked step to the user.

| Task | Owner | Write scope | Acceptance |
| --- | --- | --- | --- |
| DL-UI | `ui_ux`, desktop engineer | Separate `E:\Downloader-work-ui` worktree: Slint UI, GUI interactions/tests, translation catalogs/generator, UI icons | Clear settings sections; Save/Cancel reachable at 760×480; browser installation guide/actions; focused usability improvements; retained lifecycle behavior |
| DL-BROWSER | `browser_setup`, Rust engineer | Separate `E:\Downloader-work-browser` worktree: `core/src/browser.rs`, new `gui/src/browser_setup.rs`, related installer checks | Discover bundled extension; accurate host status; explicit repair/register; selected Chrome/Brave/Edge page and folder actions; isolated regression tests |
| DL-REVIEW | `independent_review`, independent reviewer | Read-only source audit, then final candidate review | Concrete findings, file/line evidence, review of final integrated code |
| DL-INTEGRATE | `/root`, coordinator/integrator | Main worktree: integration wiring, focused audit fixes outside author scopes, docs, local verification and publication | Odin reads this project; meaningful checks pass locally; review findings addressed; remote HEAD verified after push |

Dependencies: freeze the UI/backend contract first; integrate author commits sequentially; wire backend after both arrive; run final checks and independent review on the same candidate. Authors do not push. Root owns final publication.

## Contract and decisions

- Settings tabs: General, Downloads, Browser. Browser selector order: Chrome, Brave, Edge.
- Slint properties: `s-tab`, `s-browser`, `browser-status`, `browser-folder`, `browser-message`, `browser-busy`. Callbacks: `browser-refresh`, `browser-register`, `browser-open-page`, `browser-open-folder`, `browser-copy-path`.
- Keep the pinned Rust/Slint versions and existing core/CLI separation. No dependency-wide upgrades.
- The unpacked extension requires the user's browser Developer mode and Load unpacked action. App Settings prepares and explains these steps; a native-host registration is not proof that an extension is loaded in a browser profile.
- Connected Odin only knows an unrelated project. Use the installed Odin 0.2.0 CLI with a dedicated read-only Downloader configuration, without changing the user's connected MCP configuration.
- Existing CI is left available for future use. `[skip ci]` avoids this push's `push`/`pull_request` runs; release remains manual.
- Local installed version discovered: 0.4.1, all-user path `C:\Program Files\Downloader`, no running Downloader process. The shell is not elevated; upgrading that location requires the installer's standard Windows elevation flow.
- Chrome and Edge were detected locally; Brave was absent from standard install locations and App Paths. Do not claim Brave installation or live browser-profile acceptance.

## Evidence

Local transient logs and Odin configuration live under ignored `target/review-evidence/`. The final validation report records commands, exit codes, review status, artifacts and remaining platform limits. Local passing checks do not imply CI, Linux/macOS or live browser-profile acceptance.
