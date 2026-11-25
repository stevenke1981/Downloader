# Command-line tools

`dlx` downloads from a terminal. `dlxctl` controls a running headless manager and edits local settings. Both are built from `core` and need no other programs installed.

Human output is the default and is kept stable. Add `--json` for machine-readable output.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Failed (HTTP error status, disk, other) |
| 2 | Usage or invalid input (bad flag, URL, checksum or header; server rejected the request) |
| 3 | Network failure, timeout or server unreachable |
| 4 | Authentication: missing or wrong token (HTTP 401/403) |
| 5 | Integrity verification failed |
| 6 | Conflict: file exists with `skip`, or a duplicate download |
| 7 | Module error (a module failed to load, resolve or approve) |
| 130 | Interrupted (Ctrl+C). `dlx` keeps the partial file; run the same command again to resume |

With several URLs (dlx) or several ids (`dlxctl wait`), the largest code wins.

## dlx

```text
dlx [OPTIONS] [URLS]...
dlx --tools [--json]
```

Downloads resume from the partial state kept in the output folder. Progress and verification notes go to stderr in every mode.

| Option | Meaning |
|---|---|
| `-o, --dir DIR` | Output folder (default `.`) |
| `-c, --connections N` | Connections per download, 1–32 (default 8) |
| `-l, --limit SPEED` | Speed limit, e.g. `500K`, `2M` (0 = unlimited) |
| `--verify auto|off|HASH` | Integrity check. A hex digest (MD5, SHA-1, SHA-256, SHA-512 by length; `b2:` prefix for BLAKE2b-512) |
| `--header "NAME: VALUE"` | Extra request header, repeatable. `Cookie`, `Authorization`, `Host`, `Range`, `If-Range` and `Content-Length` are refused; at most 16 headers and 4 KiB in total |
| `--user-agent TEXT` | User-Agent for every request |
| `--referer URL` | Referer for every request |
| `--on-conflict rename|overwrite|skip` | Existing file: save as `name (1).ext` (default), replace it when complete, or fail with exit 6. `skip` fails before transferring when the name is known up front (URL name, `--out`, resolver filename); a name only the server or a protocol reveals is checked right after the transfer. `overwrite` publishes beside the old file and replaces it only after verification |
| `--mirror URL` | Alternative `http(s)` source for the one URL, repeatable, at most 8. Tried in order when the main URL fails (see [Mirrors](#mirrors)). Allowed only with exactly one URL; anything else, or a non-http(s) URL, exits 2 |
| `--modules-dir DIR` | Load modules from DIR. Only modules approved with `dlxctl modules approve` run (approvals are read from the default settings file, see below) |
| `--video` | Extract webpage media or HLS |
| `--proxy URL`, `--retries N`, `-q` | As named |
| `--json` | NDJSON events on stdout (below) |

### dlx --json

Stdout carries one JSON object per line and nothing else. Every object has an `event` field:

| Event | Fields |
|---|---|
| `tools` | `tools`: list of `{name, available}` (only with `--tools`) |
| `start` | `url`, `resumed_bytes` |
| `progress` | `url`, `downloaded`, `total` (null if unknown), `bytes_per_sec`, `connections`. At most once per second; not with `-q` |
| `done` | `url`, `path`, `size`, `seconds`, `verification` (object, or null) |
| `error` | `url` (absent for usage errors), `code`, `message`. Codes: `invalid`, `http`, `network`, `auth`, `verify`, `conflict`, `duplicate`, `disk`, `module`, `interrupted`, `error` |
| `summary` | `completed`, `failed`, `exit_code`. Always the last line of a download run |

Example:

```json
{"event":"start","url":"https://example.com/a.iso","resumed_bytes":0}
{"event":"done","url":"https://example.com/a.iso","path":"/data/a.iso","size":1048576,"seconds":0.42,"verification":null}
{"event":"summary","completed":1,"failed":0,"exit_code":0}
```

### Mirrors

`--mirror` (dlx) and `dlxctl add --mirror` give the same file from other `http(s)` URLs. Modules can add mirrors too: a resolver's mirrors are tried after yours, only for the run that resolved them, and are never saved or exported. Mirrors are never used for FTP, torrent, Metalink or media downloads.

When the main URL fails, the mirrors are tried in order (at most 8, duplicates and invalid entries skipped silently), each with the usual `--retries`. The main URL's error is what you see when every source fails, so exit codes do not change.

- A mirror is tried after: network errors, timeouts, 408, 429, 5xx, and 404 or 410 on the main URL.
- A mirror is never tried after: 401, 403 or 407, a `file exists:` conflict, `not enough disk space:`, a local file error, a verification failure, or a cancel.
- R1: when the size is known, a mirror must report exactly that size; otherwise it is skipped, never restarted.
- R2: a resumable partial download continues on a mirror only if it reports the same size and supports range requests; otherwise it is skipped and the partial file is kept.
- R3: with nothing downloaded yet, or an unknown size, the mirror starts from byte 0.
- The main URL's ETag and Last-Modified are kept, so a later run resumes against the main URL.

Isolation: a mirror receives your cookies and Basic credentials only if it has the same origin as the main URL. Custom `--header` values and the referer go only to a mirror on the same origin as the URL being downloaded, and a resolver's headers only to its own origin. The User-Agent goes everywhere. Logs name a mirror by origin only, never its path or query.

With no checksum, bytes from two sources are mixed on a same-size resume. Use `--verify HASH` when the mirror is not fully trusted.

### Modules in dlx

To run modules from `dlx`, all three must hold:

1. `--modules-dir` is the state directory's `modules` folder (for example `%APPDATA%\Downloader\modules`, or `$DOWNLOADER_STATE_DIR/modules`). Any other folder is refused with exit 2.
2. Modules are enabled: `dlxctl modules enable`. Otherwise dlx prints a warning and runs no module.
3. Each module is approved: `dlxctl modules approve ID`, or `dlxctl modules approve ID --sha256 HEX` to approve only the `module.json` you checked.

dlx reads approvals from the default state directory's `settings.json`, so run `dlxctl` without a different `--data-dir` when you approve modules for `dlx`.

With `--modules-dir`, dlx runs approved resolver modules before each download. Approvals come from the default settings file (`settings.json` in the state folder, or `DOWNLOADER_STATE_DIR` when set). An approval is bound to the SHA-256 of the module's `module.json`, so an edited module stops running. After each URL finishes, dlx tells the approved hook modules subscribed to `completed` or `failed` (the download `id` is 0 in dlx; a resolver failure `module <id>: ...` is a `failed` event; a cancelled download sends nothing). dlx waits for the hooks before it exits, at most each hook's `timeout_ms` plus 5 seconds, so a hook's own output is complete when dlx returns. Hooks add nothing to stdout or stderr, `summary` stays the last NDJSON line, and exit codes ignore hook results. Ctrl+C stops waiting, kills the hook processes and exits 130.

## dlxctl

```text
dlxctl [--endpoint URL] [--token TOKEN] [--data-dir DIR] [--json] COMMAND
```

- `--endpoint` defaults to `http://127.0.0.1:8765`. Only HTTP(S) without embedded credentials is accepted.
- The token comes from `--token` or `DOWNLOADER_REMOTE_TOKEN`. Prefer the environment variable so the token stays out of shell history.
- `--data-dir` selects the state folder (default: the app's folder; `DOWNLOADER_STATE_DIR` overrides it).

### Output

Human mode prints what each command printed before (raw JSON for `list` and `status`, one line per link for `crawl`, and so on).

With `--json`, every result is one line:

```json
{"ok":true,"data":...}
{"ok":false,"error":{"code":"auth","message":"provide --token or DOWNLOADER_REMOTE_TOKEN"}}
```

Remote errors keep the server's text in `message` (for example `HTTP 401 Unauthorized: ...`). The `code` is the server's `code` when it sends one, otherwise it is derived from the HTTP status. Argument errors from the parser (unknown flag, missing value) go to stderr with exit 2, even with `--json`.

### Commands

| Command | Remote route | Notes |
|---|---|---|
| `list`, `status ID` | `GET /downloads`, `GET /downloads/{id}` | |
| `add URL` | `POST /downloads` | `-d DIR`, `--paused`, `--video`, `--unique`, `--priority N`, `--checksum DIGEST`, `--header "NAME: VALUE"`, `--on-conflict rename\|overwrite\|skip`, `--mirror URL` (repeatable, max 8, http(s) only; see [Mirrors](#mirrors)). `--unique` returns exit 6 when the URL is already queued or active |
| `start ID`, `pause ID`, `remove ID` | `POST …/start`, `POST …/pause`, `DELETE /downloads/{id}` | |
| `start-all`, `pause-all` | `POST /start-all`, `POST /pause-all` | |
| `wait ID...` [--timeout SECS] | polls `GET /downloads/{id}` | Waits until each download completes. Exit code is the most severe failure. Paused downloads wait until `--timeout` (if given) |
| `export` | `GET /export` | Unfinished downloads only. No cookies, credentials or logs |
| `import FILE` [--start]` | `POST /import?start=0\|1` | Imported downloads are added paused unless `--start` is given |
| `serve [--bind ADDR]` | (local) | Runs a headless manager until Ctrl+C. With `--json` it prints `{"ok":true,"data":{"listening":ADDR}}` once ready |
| `crawl URL`, `ftp URL` | (local) | Lists links or FTP entries |
| `site …`, `automation …` | (local) | Edit settings directly. Stop the app or `serve` first |
| `modules list` | (local) | Modules in `<data-dir>/modules`, with approval state and problems |
| `modules approve ID [--sha256 HEX]` | (local) | Stores the SHA-256 of `<data-dir>/modules/ID/module.json`. With `--sha256`, approval is refused unless the file has that hash (as `modules list` shows it). Any later edit to that file disables the module |
| `modules revoke ID`, `modules enable`, `modules disable` | (local) | Edit settings. A running app notices on its next settings save: a revocation or disable on disk wins, so it is not undone. The app applies it fully after a restart |
| `generate-token` | (local) | Prints a new 256-bit token |

Local commands that edit settings write `settings.json` and apply the next time the manager starts. Approvals are never accepted from the remote API or from an import file.

### Examples

```sh
export DOWNLOADER_REMOTE_TOKEN=...            # the token the app or serve was started with
dlxctl --json add https://example.com/a.iso --unique --checksum sha256:...
dlxctl --json wait 4 --timeout 600
dlxctl --json export > queue.json
dlxctl modules approve example-resolver
dlxctl modules approve example-resolver --sha256 <hex>   # only if module.json has this hash
```
