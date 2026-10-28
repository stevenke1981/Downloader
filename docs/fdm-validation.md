# FDM alignment acceptance

This document is updated from actual results. Worker-local checks below are complete; final integrated release and native GUI acceptance are pending until their commands run.

| Layer | Current evidence | Status |
|---|---|---|
| Native routing | Luna/medium selected Sol/xhigh for 3 authors; live capability checks and native IDs registered | PASS |
| Protocol regression | Existing core 71 tests; 3 focused external tests | PASS (worker source) |
| Real protocol tools | Native HTTP, anonymous FTP, Metalink mirror fallback, torrent webseed, magnet local peer, yt-dlp local MP4; `target/fdm-protocol-runtime/runtime.json` | PASS (debug artifact) |
| Real FFmpeg | Explicit finite RTSP/RTP capture and child cancellation/socket close tests | PASS (worker source) |
| Management | 26 unit and 7 integration tests; actual throttle/auth/crawler/automation/remote API fixtures | PASS (isolated worker source) |
| Desktop | Author validation in progress | NOT_RUN (integrated release) |
| Independent integration | Configured Sol/high role not yet started | NOT_RUN |
| Final release verifier | `scripts/verify-fdm.ps1` not yet invoked | NOT_RUN |
| Native GUI operation | Final release not yet launched | NOT_RUN |
| Public Internet / live MMS / other OS | No fixture or platform invocation | NOT_RUN |
| Windows install upgrade | Not requested; installer packaging can be built separately | NOT_RUN |

No actual shutdown is performed during acceptance. Optional protocol executables are temporary test dependencies, not installed or bundled. External launch-time/per-process limit and stream-resume limits are documented in README and the tools guide.
