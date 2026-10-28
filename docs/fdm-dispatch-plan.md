# FDM alignment dispatch evidence

- Base SHA: `6e5cb2bbdd3933b7a613505115defebe0212b318`.
- Contract/scaffold SHA: `73dd445`.
- Native coordinator: `/root/luna_coordinator`, `gpt-6-luna/medium` (Host-reported canonical agent identity; actual native responses recorded in this session).
- Selected worker pair: `gpt-6.1-sol/xhigh` for all three coding tasks.
- Runtime live model/effort checks: `luna_decide` accepted each selection; no OpenRouter call.
- Workflow job: `757e52419ca54b53b2b224bb84be9981`.

| task | native agent | worktree | dispatch id |
|---|---|---|---|
| protocol-engine | /root/protocol_worker | target/worktrees/fdm-protocol | 090d4cded87742cb9fa61054fbdc237b |
| management-services | /root/management_worker | target/worktrees/fdm-management | 0a74bc63a9f440c69a128b7de590e836 |
| desktop-integration | /root/desktop_worker | target/worktrees/fdm-desktop | aac69db197324bd6a786fd21011582f8 |

All dispatches were claimed once and their returned agent IDs registered with `started`. Separate worktrees protect author ownership; they are not OS sandboxes. Root owns documentation, shared exports, final merge and user-authorized push. Configured final integrator is `gpt-6.1-sol/high`; independent verifier must execute a real command after integration.

Temporary acceptance tools live under ignored `target/fdm-tools`; no third-party binary is committed or installed systemwide. aria2 1.37.0 archive was downloaded from its official GitHub release (SHA256 `67d015301eef0b612191212d564c5bb0a14b5b9c4796b76454276a4d28d9b288`). yt-dlp 2026.10.26 executable SHA256 `f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d` matched the official GitHub asset digest. FFmpeg and curl were already installed.
