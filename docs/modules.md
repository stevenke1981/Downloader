# Modules

Modules add abilities to Downloader without changing the app. There are two sorts:

- **Built-in protocol modules** ship inside the app: FTP, BitTorrent / magnet, Metalink and
  HLS / RTSP / MMS / web media. They are always on and need no setup.
- **External modules** are small programs you install yourself. A *resolver* turns a link into
  the real download link (for example a "download page" into the file behind it). A *hook*
  is told when a download completes or fails (for example to scan, unpack or index the file).

External modules are **off by default** and nothing runs until you both enable modules and
approve each module.

## Using modules

### Install

Each external module lives in its own folder under the modules folder:

| System | Modules folder |
|---|---|
| Windows | `%APPDATA%\Downloader\modules` |
| Linux | `~/.config/Downloader/modules` |
| macOS | `~/Library/Application Support/Downloader/modules` |
| any (override) | `$DOWNLOADER_STATE_DIR/modules` |

Copy the module's folder there. The folder name must equal the module's `id`, and the folder
must contain a `module.json` (and usually the module's program). A ready-to-copy example is
in [`modules/examples/example-resolver`](../modules/examples/example-resolver/README.md).

### Enable and approve

In the app: **Tools > Modules**, tick **Enable modules**, select a module and press **Approve**.
**Revoke** takes approval back; **Open modules folder** shows where modules live.

From the command line:

```
dlxctl modules list                # every module, approved or not, with problems
dlxctl modules enable              # or: disable
dlxctl modules approve <id>         # prints the program, arguments and sha256 it approves
dlxctl modules approve <id> --sha256 <hex>   # approve only that exact module.json
dlxctl modules revoke <id>
```

The list shows a reason next to any module that cannot run (for example an invalid
`module.json`, or "module.json changed since it was approved; approve it again").

### What happens during a download

1. If a built-in protocol takes the link (FTP, magnet, `.torrent`, `.metalink`, `.m3u8`,
   RTSP/MMS, website-video mode), no external module is asked.
2. Otherwise the first approved resolver (by folder name) whose `match` covers the link's
   scheme and host is asked to resolve it. It may return a new link, extra request headers,
   a file name and mirrors, or leave the link unchanged.
3. The download runs against the resolved link. The original link stays in the list; the
   resolved link and the module's extra headers are not saved, and are dropped at the start of
   every run: after a restart, or once the module is disabled, revoked or answers `skip`,
   the module is asked again or the original link is used.
4. When the download completes or fails, every approved hook subscribed to that event is told
   in the background. Hooks cannot change or delay the download.

Errors from a module fail the download with a message starting `module <id>:`.

### Security model

- **Off by default, per-module approval.** A module runs only when modules are enabled *and*
  the SHA-256 of its exact `module.json` bytes matches the stored approval. Editing the file
  (even whitespace) revokes it; this is re-checked right before every start. Approval is only
  given through the app's Approve button or `dlxctl modules approve`, never by remote
  control, settings import or queue import.
- **Approval covers `module.json`, not the program file.** Only install modules you trust,
  keep their folders writable only by you, and prefer an absolute `command[0]` to a program
  in a folder you control.
- **Contained start.** Programs are started directly from an argument list (no shell), with
  the module folder as working directory, an empty environment except `PATH`, `SystemRoot`,
  `TEMP` and `TMP`, no console window, and are killed on timeout (≤ 60 s) or cancel.
  At most 4 module processes run at once; at most 32 hook deliveries wait, later ones are dropped.
- **Narrow input and output.** Resolvers receive only the link and referer, never cookies or
  passwords. Answers are capped at 64 KiB. A resolved link must be plain `http(s)` without
  credentials, cannot be another built-in protocol link, headers must pass the same rules as
  user headers (no `Cookie`, `Authorization`, `Host`, `Range`, ...), and file names are
  sanitized. Extra headers are kept apart from your own headers, are never saved, and are sent
  only to the resolved link's origin.
- **One modules folder.** `dlx --modules-dir` accepts only the state directory's modules
  folder (the table above) and honours `dlxctl modules enable` / `disable`; approvals do not
  carry over to a copy of a module in another folder.

## Writing a module

A module is a folder with a `module.json` and any program, in any language, that reads one
JSON line from stdin and writes at most one JSON line to stdout.

### module.json

```json
{
  "schema": 1,
  "id": "example-resolver",
  "name": "Example resolver",
  "version": "1.0.0",
  "kinds": ["resolver", "hook"],
  "match": { "schemes": ["https"], "hosts": ["example.com", "*.example.org"] },
  "command": ["example-resolver", "--stdio"],
  "timeout_ms": 10000,
  "events": ["completed", "failed"]
}
```

| Field | Rules |
|---|---|
| `schema` | Must be `1`. |
| `id` | 1–40 characters `a-z`, `0-9`, `-`; equals the folder name. |
| `name`, `version` | Non-empty, at most 80 / 40 printable characters. |
| `kinds` | Non-empty subset of `"resolver"`, `"hook"`. |
| `match.schemes` | Subset of `"http"`, `"https"`. Required for resolvers. |
| `match.hosts` | Lowercase host names; `*.example.org` matches subdomains but not `example.org`. At most 64. Required for resolvers. |
| `command` | Argument list, at most 32 entries of < 1 KiB. `command[0]` is a file name inside the module folder (Windows adds `.exe` if needed) or an absolute path; no `..`, no `/` or `\` in a relative name; the canonical path must stay inside the folder. |
| `timeout_ms` | Optional, 1–60000, default 10000. Applies to each process. |
| `events` | Subset of `"completed"`, `"failed"`. Required for hooks. |

Unknown fields are rejected. The file may be at most 64 KiB.

### Process contract

- One process per request. Downloader writes one UTF-8 JSON line plus `\n` to stdin and
  closes it.
- A resolver writes **one** JSON line to stdout (first line only is read, at most 64 KiB) and
  exits. Stderr is discarded. The process is killed after the answer, on timeout, or when the
  download is cancelled.
- A hook may write anything (it is read and discarded) and runs until it exits or its
  `timeout_ms` passes, when it is killed.
- Working directory is the module folder. Environment: only `PATH`, `SystemRoot`, `TEMP`, `TMP`.

### Resolve

Request:

```json
{"v":1,"op":"resolve","url":"https://example.com/get/file.zip","referer":null}
```

`referer` is a string or `null`. Answers:

```json
{"ok":true,"url":"https://cdn.example.net/file.zip","headers":{"X-Token":"abc"},"filename":"file.zip","mirrors":["https://mirror.example.net/file.zip"]}
{"ok":true,"skip":true}
{"ok":false,"error":"this page has no download"}
```

- `url` is required when `ok` is true and `skip` is absent: `http`/`https`, no user/password,
  not a `.torrent`/`.metalink`/`.meta4`/`.m3u8` link.
- `headers` (optional): at most 16, 4 KiB total; replaces user headers of the same name.
  `Cookie`, `Authorization`, `Host`, `Range`, `If-Range`, `Content-Length` are refused.
- `filename` (optional): used only when the user gave no name; sanitized.
- `mirrors` (optional): at most 16, same rules as `url`.
- `skip`: download the original link unchanged.
- `ok:false`: the download fails with `module <id>: <error>` (error shortened to 300 characters).
- Anything else (no line, not JSON, too large, timeout) fails the download with `module <id>: ...`.
  Unknown answer fields are ignored.

### Event (hooks)

```json
{"v":1,"op":"event","event":"completed","id":12,"url":"https://example.com/a.zip","path":"C:\\Downloads\\a.zip","size":1048576,"error":null}
{"v":1,"op":"event","event":"failed","id":13,"url":"https://example.com/b.zip","path":"C:\\Downloads\\b.zip","size":null,"error":"http 404"}
```

`path` is the downloaded file; it is empty (`""`) when the download did not create a file
(for example a failure because the file already existed). Output is ignored; exit when done
(within `timeout_ms`). Delivery is best effort and never retried.

### Checking a module by hand

```
echo {"v":1,"op":"resolve","url":"https://example.com/a.zip","referer":null} | dlx-module-example --base https://downloads.example.net/files/
```

Then `dlxctl modules list` shows whether Downloader accepts the manifest.
