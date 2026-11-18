# example-resolver

A ready-to-copy external module for Downloader. It is both a **resolver** (it rewrites
links before they download) and a **hook** (it is told when a download completes or fails).

What it does, as configured in `module.json`:

- For links on `https://example.com/...` and `https://*.example.org/...`, it answers with
  `https://downloads.example.net/files/<last part of the link>`.
- Every request it receives (resolve or event) is appended to `requests.log` in its folder.

Change `match.hosts` and `--base` to point it at something real, or use it as a template
for your own module in any language. The full protocol is in [docs/modules.md](../../../docs/modules.md).

## Install

1. Build the example program:

   ```
   cargo build --release -p downloader-core --bin dlx-module-example
   ```

2. Create the module folder. Its name must equal the `id` in `module.json`:

   - Windows: `%APPDATA%\Downloader\modules\example-resolver\`
   - Linux: `~/.config/Downloader/modules/example-resolver/`
   - macOS: `~/Library/Application Support/Downloader/modules/example-resolver/`
   - If `DOWNLOADER_STATE_DIR` is set: `$DOWNLOADER_STATE_DIR/modules/example-resolver/`

3. Copy into it:
   - this `module.json`
   - `target/release/dlx-module-example.exe` (Windows) or `target/release/dlx-module-example`

   `command[0]` is the bare name `dlx-module-example`; on Windows `.exe` is added when needed.

4. Turn modules on and approve this one, in the app (Tools > Modules: tick "Enable modules",
   select the module, Approve) or from the command line:

   ```
   dlxctl modules enable
   dlxctl modules approve example-resolver
   dlxctl modules list
   ```

Approval records the SHA-256 of `module.json`. If you edit the file afterwards the module
stops running until you approve it again.

## Options of dlx-module-example

| Option | Effect |
|---|---|
| `--base <url>` | Prefix for resolved links (required to resolve). |
| `--header K:V` | Add a request header to the answer (repeatable). |
| `--filename <name>` | Suggest a file name. |
| `--mirror <url>` | Add a mirror link (repeatable). |
| `--log <file>` | Append each request line to `<file>` in the module folder. |
| `--skip` | Leave every link unchanged. |
| `--fail <message>` | Refuse every link with `message`. |

`--sleep`, `--flood` and `--garbage` exist only for Downloader's own tests.
