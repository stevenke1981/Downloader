# Downloader 0.7.0 team plan (adviser: Fable 5.2)

Goal: human first, agent friendly; essential downloader features; an add-on module system.
Implementers work in the SAME tree concurrently: file ownership is disjoint. Each uses its own CARGO_TARGET_DIR.

## Findings
- engine::Ctx must not change (struct literals in dlx.rs, scheduler.rs and tests). Module resolution runs before engine::run.
- core/tests/external.rs asserts no Command::new under engine/** and protocols/**: the external module host lives in core/src/modules/.
- gui/ui/app.slint (827 lines) and gui/src/main.rs (708) are at the size limit: new UI goes into the Tools window.
- No new crates (tokio has process; serde_json, sha2 present). Manifests are JSON. Cargo.lock unchanged; --locked works.

## Gap analysis
| Feature | Today | Owner |
|---|---|---|
| File-exists policy | always renames (util::unique_name) | B |
| Custom headers / User-Agent | only referer, cookie, basic auth; UA fixed | B |
| Duplicate-URL detection | GUI batch/clipboard only | B |
| Disk-space precheck | none (set_len failure ignored) | B |
| Queue export/import | none | B |
| Completion hooks | only one-shot Action::Run | A (hook modules), B calls |
| Remote add fields | AddDownload lacks priority/checksum/referer | B |
| HTTP mirror fallback | metalink only | B, stretch |

## Wave 0 contract (compiling stubs, done first)
- core/src/lib.rs: pub mod modules;
- core/src/modules/mod.rs (A's file):
```rust
#[derive(Debug,Clone,Default,Serialize,Deserialize,PartialEq,Eq)] #[serde(default)]
pub struct ModuleSettings { pub enabled: bool, pub approved: BTreeMap<String,String> } // id -> sha256(module.json) hex
impl ModuleSettings { pub fn validate(&self) -> Result<(), String> }
#[derive(Debug,Clone,Serialize)]
pub struct ModuleInfo { pub id: String, pub name: String, pub version: String, pub builtin: bool,
    pub kinds: Vec<String>, pub approved: bool, pub error: Option<String>, pub manifest_sha256: String }
#[derive(Debug,Clone,Serialize)]
pub struct HookEvent { pub event: &'static str /* completed|failed */, pub id: u64, pub url: String,
    pub path: PathBuf, pub size: Option<u64>, pub error: Option<String> }
pub struct Registry { .. }
impl Registry {
    pub fn load(settings: &ModuleSettings, dir: &Path) -> Arc<Registry>;  // never fails; errors in ModuleInfo.error
    pub fn empty() -> Arc<Registry>;
    pub fn list(&self) -> Vec<ModuleInfo>;
    pub async fn prepare(&self, task: &Arc<Mutex<Task>>, cancel: &CancellationToken) -> Result<(), String>;
    pub fn notify(self: &Arc<Self>, rt: &tokio::runtime::Handle, event: HookEvent); // detached, bounded
}
pub fn modules_dir() -> PathBuf; // <state dir>/modules
```
- core/src/task.rs (B's file):
  ConflictPolicy { #[default] Rename, Overwrite, Skip } (Serialize/Deserialize lowercase, Copy, Default)
  Task gains (all #[serde(default)]): headers: Vec<(String,String)>, user_agent: Option<String>, mirrors: Vec<String>,
  conflict: ConflictPolicy, #[serde(skip)] resolved_url: Option<String>, module: Option<String>
  pub fn validate_headers(h: &[(String,String)]) -> Result<(), String> — rejects Cookie, Authorization, Host, Range, If-Range,
  Content-Length, CR/LF, >16 entries, >4 KiB total.
- core/src/settings.rs (B): pub conflict: ConflictPolicy, pub modules: ModuleSettings; validate() calls self.modules.validate().
- core/src/util.rs (B): pub fn resolve_conflict(dir:&Path, name:&str, p:ConflictPolicy) -> Result<String,String>;
  pub fn error_code(msg:&str) -> &'static str  // http|network|verify|conflict|duplicate|disk|module|auth|invalid|error
- core/src/manager/models.rs (B): NewDownload gains headers, user_agent, mirrors, conflict: Option<ConflictPolicy>, unique: bool.
- Stable error prefixes: "file exists:", "duplicate: already queued as #ID", "not enough disk space:", "module <id>:", "verification failed".

## Implementer A (Opus 5.5): module system
Owns core/src/modules/** (mod.rs, manifest.rs, external.rs, tests.rs), core/src/protocols/mod.rs + new core/src/protocols/registry.rs,
core/src/lib.rs, core/Cargo.toml (only a new [[bin]] dlx-module-example), core/src/bin/module_example.rs, core/tests/external.rs,
new core/tests/modules.rs, modules/examples/**, docs/modules.md.
- Built-in trait in registry.rs: ProtocolModule { id, name, matches(source, kind), download(task, ctx, stage) -> boxed future Result<Outcome,String>,
  publish_name(actual, suggested, file) }; builtin() -> &'static [&'static dyn ProtocolModule] reproducing exact current protocol() precedence
  (Video kind -> media, then scheme, then extension). capabilities(), handles(), staging_path(), run(), list_ftp() signatures unchanged.
  Publication uses util::resolve_conflict(dir, name, task.conflict).
- External module manifest at <state_dir>/modules/<id>/module.json:
  {"schema":1,"id":"example-resolver","name":"..","version":"1.0.0","kinds":["resolver","hook"],
   "match":{"schemes":["https"],"hosts":["example.com","*.example.org"]},"command":["example-resolver","--stdio"],
   "timeout_ms":10000,"events":["completed","failed"]}
  Runs only if settings.modules.enabled && approved[id] == sha256(manifest bytes). Edited manifest => disabled.
  id = [a-z0-9-]{1,40} == directory name. command[0] = bare name inside module dir or absolute path; no "..", canonical path checked.
- Host (external.rs): argv only, no shell; current_dir = module dir; env_clear + PATH/SystemRoot/TEMP/TMP; kill_on_drop; CREATE_NO_WINDOW;
  timeout <= 60 s; stdout <= 64 KiB; stderr discarded; <= 4 concurrent (semaphore).
- Protocol: one process per request, one JSON line each way.
  Request {"v":1,"op":"resolve","url":..,"referer":..} (never cookies/authorization).
  Response {"ok":true,"url":..,"headers":{..},"filename":..,"mirrors":[..]} | {"ok":true,"skip":true} | {"ok":false,"error":..}
  Hook {"v":1,"op":"event", ...HookEvent}; output ignored.
- prepare(): no-op when a built-in protocol handles the task or no module matches. Output URL must be http(s), pass util::validate_source,
  and not satisfy protocols::handles. Headers via task::validate_headers; filename via util::sanitize. Sets resolved_url, headers, mirrors,
  module, suggested_name if None. Errors "module <id>: ...".
- Tests: precedence identical; unapproved/hash-changed/disabled never spawn (marker file); timeout kills; 1 MiB stdout, non-JSON, ftp:// output,
  Cookie header, path-escaping command[0] rejected; end-to-end with CARGO_BIN_EXE_dlx-module-example resolving to fixture server; hook fires once;
  external.rs: Command only in modules/external.rs.

## Implementer B (Sonnet 5.5): essentials
Owns core/src/task.rs, settings.rs, util.rs, engine.rs, engine/**, manager.rs, manager/** (+ new manager/transfer.rs), remote.rs, verify.rs,
core/tests/download.rs, core/tests/management_services.rs, new core/tests/essentials.rs.
1. Conflict: try_add sets task.conflict = d.conflict.unwrap_or(s.conflict). Skip => Err("file exists: <path>") before transfer; Overwrite replaces
   only at final rename; a .part is never overwritten.
2. Headers/UA: probe and Shared take them. Effective URL = t.resolved_url.unwrap_or(t.url). Custom headers only to effective origin (same_origin);
   UA always. Cookie/authorization dropped when effective origin differs from t.url.
3. Duplicates: Manager::find_duplicate(&self, url) -> Option<u64> (trimmed URL + kind, non-Completed). try_add errors when d.unique.
4. Disk: util::free_space(dir) -> Option<u64> (Windows GetDiskFreeSpaceExW FFI; None elsewhere). Fail before set_len if remaining > free.
5. Module wiring: Inner.modules: Mutex<Arc<Registry>> reloaded in set_settings (keep existing approved map; approvals only via
   Manager::approve_module(id) / revoke_module(id), never via remote/import/settings form). Manager::modules() -> Vec<ModuleInfo>.
   Scheduler calls registry.prepare(&task,&cancel).await before engine::run; on Completed/Error calls registry.notify(..).
6. Export/import (transfer.rs): export_queue(&self) -> String {"schema":1,"downloads":[..]} (no cookies/auth/log);
   import_queue(&self, json, start) -> Result<ImportReport,String>; ImportReport { added: Vec<u64>, skipped: Vec<(String,String)> }.
7. Remote: AddDownload gains priority, checksum, referer, headers, user_agent, unique, conflict. Routes GET /downloads/{id}, GET /export,
   POST /import, GET /modules, GET /version. Errors {"error":msg,"code":util::error_code(msg)}; duplicates 409.
8. Stretch: mirror fallback after probe retries, same size only.
Tests: 0.6.2 downloads.json/settings.json load unchanged; rename/overwrite/skip on real download; headers+UA reach fixture server, not after
cross-origin redirect; unique rejected with stable prefix; export/import round-trip; remote 409 with code.

## Implementer C (Haiku 5.5): agent surfaces
Owns core/src/bin/dlx.rs, core/src/bin/dlxctl.rs, new core/src/bin/shared/cli.rs (#[path] in both bins), new core/tests/cli.rs,
AGENTS.md, docs/cli.md, docs/dependencies-and-modules.md.
Exit codes: 0 ok, 1 failed, 2 usage/invalid, 3 network/unreachable, 4 auth/token, 5 verification, 6 conflict/duplicate, 7 module, 130 interrupted.
Mapped via util::error_code; with several URLs highest-severity wins.
dlx: --json NDJSON on stdout (start, progress <=1/s, done{path,size,seconds,verification}, error{code,message}, summary); human text on stderr;
default output unchanged. Flags --header K:V (repeatable), --user-agent, --referer, --on-conflict rename|overwrite|skip, --modules-dir <dir>
(opt-in: Registry::load + prepare), --tools --json.
dlxctl: global --json => {"ok":true,"data":..} | {"ok":false,"error":{"code","message"}}. New: status <id>, wait <id>.. [--timeout s], export,
import <file>, modules list|approve <id>|revoke <id>|enable|disable (edits settings; approve stores hash list shows); add gains --unique
--priority --checksum --header --on-conflict.
AGENTS.md: layout, build/test/clippy/fmt commands, 800-line rule, i18n flow, how to add a protocol module / external module / CLI flag,
no-required-executables rule.
Tests (cli.rs): every --json stdout line parses; codes 2, 5, 6 reproduced; dlxctl --json list against serve; missing token => 4.

## GUI (wave 2, after core lands)
Tools window: sixth tab "Modules" (enable checkbox, list from Manager::modules(), Approve/Revoke, Open modules folder) and a
"When a file already exists" ComboBox in "Sites / folders"; wiring in new gui/src/tools/modules.rs + minimal hook in gui/src/tools.rs.
app.slint and main.rs untouched. Then gen_po.py entries + .po translations, README. Lead: CHANGELOG, version 0.7.0, installer.

## Verification
cargo test --release --locked -p downloader-core
cargo clippy --release -p downloader-core --all-targets --locked -- -D warnings
cargo fmt --all --check
cargo test --release --locked -p downloader   (wave 2)

## Risks
Shared-tree build breaks (land signature changes early); module approval only via approve/revoke; resolved URLs not persisted;
Overwrite never default; hook storm capped (semaphore, drop beyond 32); exclude dlx-module-example from packaging; no disk precheck on Unix (documented).

## Hard rules for all implementers
- No git add/commit/stash/checkout/reset/restore. The lead integrates.
- Touch only files you own. If you need something in another owner's file, write it in your report instead.
- No new crates. Keep files < 800 lines. Launch processes argv only.
