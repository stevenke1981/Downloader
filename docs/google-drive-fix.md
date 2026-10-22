# Google Drive download fix — 2026-10-23

The reported `identifier` file was Google sign-in HTML. The original probe followed redirects and accepted any successful HTTP response, inferred the name from `/v3/signin/identifier`, and published the page as a completed download. Browser interception also accepted that final URL.

## Changes

- Reject Google account pages before naming or writing a file, including when a valid probe is followed by an expired download redirect.
- Resolve known Drive file-view/open links to the download endpoint, retain resource keys, and follow bounded GET confirmation forms. Confirmation actions are limited to Drive download endpoints; no browser script runs and no new dependencies are installed.
- Retain actual HTML attachments and ordinary HTML downloads. Replace generic Drive suggestions such as `identifier` with the response filename.
- Keep Google login/error pages and browser-generated blob ZIPs in the browser. Resolved Drive media use the final URL and cookies scoped to that URL; unresolved links retain their original resource key. Cookies are not copied across origins or saved to disk.
- Folder URLs receive an explicit instruction to open the folder in the browser, select files and choose Download. Recursive folder downloading is outside this fix.

## Verification

- Baseline `77f4d7821ea6540bf6ab618005d12086cff674e7`: controlled HTTP fixture reproduced `Ok(Completed)`, `file_name="identifier"`, 137 bytes of login HTML. Eight integration tests: three passed, five failed (exit 1).
- Candidate: all eight Drive integration tests passed (exit 0); core library tests 26 passed; extension tests 16 passed. The extension's same test fixture on baseline had five failures (exit 1).
- Public live smoke: `dlx` downloaded the public gdown sample `0B9P1L--7Wd2vU3VUVlFnbTgtS2c` from its file-view share link as `spam.txt`, five bytes, contents `spam` followed by newline (exit 0). No browser cookies were used.
- User-provided folder: the connected Drive account listed three ZIP files. A direct unauthenticated `dlx` attempt for one member followed Google sign-in and exited 1 with an explicit browser-login message; no output file was created. This is the expected refusal of a login page, not proof that an authenticated browser download succeeded.
- A live 2,468-byte public large-file confirmation page was inspected and contained the supported GET `download-form` targeting `https://drive.usercontent.google.com/download`; the 498,881,336-byte file was not downloaded.
- Odin live analysis and Rust LSP symbols succeeded against `E:\Downloader`. The connected project is read-only; edits and execution used the authorized local workspace tools without changing Odin grants.
- One independent scoped code check reported no evidenced blocking issue. Source/tests and local gates are recorded in `google-drive-handoff.json`.
- `scripts/verify-local.ps1` exited 0: GUI format, strict release Clippy with warnings denied, release build, 87 Rust tests passed, 16 extension tests passed, JavaScript checks and CLI version smoke. One pre-existing native-render fixture remained ignored.
- The rebuilt release CLI passed three live cases recorded in `target/google-drive-release-smoke.json`: correct public file bytes, private sign-in rejection without output, and explicit folder guidance without output.
- Inno Setup 7 compiled `dist/downloader-setup-google-drive-fix-windows-x64.exe` (exit 0). `dist/downloader-browser-extension-google-drive-fix.zip` passed ZIP CRC verification and its background script matches the tested source. Installer installation/uninstallation was not executed.

## Use and limits

The local fix retains version 0.5.2; no release or account settings were changed. Use the rebuilt app/core and updated `extension` files together, then reload the extension in the browser. An already-installed app or already-loaded extension does not update merely because this repository changed.

Real signed-in Chrome download acceptance, installed-app upgrade, remote CI, Linux/macOS runtime, and recursive folder download were not performed. The private folder still requires an authorized Google browser session.

Implementation references: [Google Drive download documentation](https://developers.google.com/workspace/drive/api/guides/manage-downloads), [gdown confirmation-page handling](https://github.com/wkentaro/gdown/blob/main/gdown/download.py), and [public sample link](https://drive.google.com/file/d/0B9P1L--7Wd2vU3VUVlFnbTgtS2c/view).
