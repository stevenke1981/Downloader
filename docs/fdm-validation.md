# FDM alignment acceptance

Final Windows x64 acceptance ran on 2026-10-28 from source `76b31c1` (product code unchanged by the subsequent evidence-only commit). This records observed results; source coverage and runtime evidence are separate.

| Layer | Current evidence | Status |
|---|---|---|
| Native routing | Luna/medium selected Sol/xhigh for 3 authors; live capability checks and native IDs registered | PASS |
| Release static/build | GUI fmt, strict all-target workspace release Clippy, locked release build | PASS |
| Workspace regression | `cargo test --release --workspace --locked`, including HTTP/Drive, protocol, management and GUI boundary tests | PASS |
| Browser extension regression | Node test suite plus `options.js` / `alt.js` syntax checks | PASS |
| Real protocol tools | Native HTTP, anonymous FTP, Metalink failed-mirror fallback and HTTP descriptor, torrent webseed and HTTP descriptor, magnet with a real local aria2 peer, yt-dlp local MP4 | PASS (8 release cases) |
| Authentication / remote manager | Real authenticated FTP listing and download through the headless manager; missing-token rejection and password exclusion from persistence | PASS (2 release cases) |
| Real FFmpeg | Two explicit finite RTSP/RTP capture and child cancellation/socket-close tests (`real_ffmpeg`, ignored tests selected explicitly) | PASS (release) |
| Management behavior | Actual limiter throughput, exact-origin auth/concurrency, bounded crawler, one-shot argv/restart, authenticated queue CRUD, deletion of external directories/staging | PASS (workspace tests) |
| Desktop boundaries | Atomic batch validation, FTP URL encoding/enqueue, site default 2 versus global 8 and explicit override 5, supported preview types and tool layout | PASS (workspace tests) |
| Independent integration | Native `/root/fdm_integrator`, configured Sol/high, one integration pass and focused recheck of three reported fixes | PASS; [report](fdm-independent-review.md) |
| Independent release verifier | `luna_workflow_verify` actually executed `scripts/verify-fdm.ps1`; exit 0 | PASS |
| Native GUI operation | Isolated fresh state; two HTTP batch downloads plus two FTP list-to-queue downloads, all completed and SHA256 matched; five tool tabs and real task log viewed | PASS; [GUI evidence](evidence/fdm/gui-runtime.json) |
| Installer packaging | `scripts/build-installer.ps1`, Inno Setup 7; installer includes `dlxctl.exe` | PASS (12,100,637 bytes); installation NOT_RUN |
| Physical drag/drop, file-picker import, OS preview launch | Code/boundary coverage exists, but these interactive actions were not exercised | NOT_RUN (native interaction) |
| Public Internet tunnel, public live RTSP/MMS, other OS | Local protocol fixtures do not establish public-network or other-platform behavior | NOT_RUN |
| Browser account integration / install upgrade / actual shutdown | Extension regression passed; no account/browser install, physical upgrade or shutdown performed | NOT_RUN |
| Hosted CI | Checked after push; local results do not imply CI success | See delivery report / GitHub Actions |

No actual shutdown is performed during acceptance. Optional protocol executables are temporary test dependencies, not installed or bundled. External launch-time/per-process limit and stream-resume limits are documented in README and the tools guide.

The independent workflow job is `757e52419ca54b53b2b224bb84be9981`, verification check `7208ed7ca60ac5ea51645b83ea377d68734f6554bf52055ede17e51dda5791a8`, exit `0`; captured verifier output SHA256 is `0509496058984283d3afe294381eb36c70db1e0fb5910d97ce16540c2c06bb7a`. Native role/model labels and canonical IDs are Host-reported evidence, not provider-level wire attestation. [Release evidence](evidence/fdm/release-acceptance.json) records all runtime cases and binary/installer SHA256 values. Full local fixtures/transcript remain under ignored `target/fdm-evidence/final-release`.

The runtime JSON's `rtsp-live: NOT_RUN` describes a public/live source; the separate real loopback RTSP/RTP tests above did execute and pass. The video fixture verifies the actual yt-dlp process and published bytes, without claiming every supported Internet extractor works.

Native release screenshots: [four completed downloads](evidence/fdm/gui-completed.png), [encoded FTP listing](evidence/fdm/gui-ftp-list.png), [actual aria2 task log](evidence/fdm/gui-task-log.png). The test GUI and local HTTP/FTP fixture server were closed after acceptance. No user's existing queue/settings were loaded.

To reproduce, use `scripts/verify-fdm.ps1` with explicit official aria2/yt-dlp paths and a Python environment containing pyftpdlib (`-PythonLib` can point at an isolated `pip --target` directory). It runs the same local release gate followed by real protocol fixtures; FFmpeg and curl must also be on PATH. Run native GUI, installer and Internet/other-OS checks separately as recorded above.
