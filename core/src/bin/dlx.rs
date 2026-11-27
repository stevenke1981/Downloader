//! dlx - headless multi-connection downloader.
#[path = "shared/cli.rs"]
mod cli;
#[path = "shared/schema.rs"]
mod schema;

use clap::{CommandFactory, Parser};
use cli::{CliError, ConflictArg, parse_header, print_json};
use dlcore::modules::{HookEvent, ModuleSettings, Registry};
use dlcore::task::DownloadKind;
use dlcore::{Checksum, Ctx, Outcome, RateLimiter, Settings, Status, Task, engine, util};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "dlx",
    version,
    about = "Resumable HTTP, FTP, torrent, Metalink and media downloader"
)]
struct Args {
    /// URL(s), local .torrent/.metalink/.meta4 descriptor(s)
    #[arg(required_unless_present_any = ["tools", "schema"])]
    urls: Vec<String>,
    /// Extract direct webpage media or HLS using the built-in Rust engine
    #[arg(long)]
    video: bool,
    /// Download the audio of a supported site (YouTube) as MP3
    #[arg(long, conflicts_with = "video")]
    audio: bool,
    /// Show built-in Rust protocol engines
    #[arg(long)]
    tools: bool,
    /// Print the machine-readable description of this command (flags, exit codes, events) as JSON
    #[arg(long)]
    schema: bool,
    /// NDJSON events on stdout (start, progress, done, error, summary); human text stays on stderr
    #[arg(long)]
    json: bool,
    /// Extra request header "Name: value" (repeatable). Cookie, Authorization and Host are refused
    #[arg(long = "header", value_name = "NAME: VALUE")]
    headers: Vec<String>,
    /// User-Agent sent with every request
    #[arg(long, value_name = "TEXT")]
    user_agent: Option<String>,
    /// Referer sent with every request
    #[arg(long, value_name = "URL")]
    referer: Option<String>,
    /// When the file exists: rename (default), overwrite, or skip (exit 6)
    #[arg(long, value_enum, default_value = "rename")]
    on_conflict: ConflictArg,
    /// Use external modules. DIR must be the modules folder of the state directory: modules run
    /// only when enabled and approved there ("dlxctl modules enable" / "dlxctl modules approve")
    #[arg(long, value_name = "DIR")]
    modules_dir: Option<PathBuf>,
    /// Alternative http(s) source tried when the main URL fails (repeatable, max 8, one URL only)
    #[arg(long = "mirror", value_name = "URL")]
    mirrors: Vec<String>,
    /// Output directory
    #[arg(short = 'o', long, default_value = ".")]
    dir: PathBuf,
    /// Connections per download (1-32)
    #[arg(short = 'c', long, default_value_t = 8)]
    connections: u32,
    /// Speed limit, e.g. 500K, 2M (0 = unlimited)
    #[arg(short = 'l', long, default_value = "0")]
    limit: String,
    /// Integrity check: "auto" (find checksum files / headers next to the download),
    /// "off", or an expected hex digest (MD5, SHA-1, SHA-256, SHA-512 detected by length;
    /// prefix "b2:" for BLAKE2b-512)
    #[arg(long, default_value = "auto")]
    verify: String,
    /// Same as --verify <hash>
    #[arg(long, hide = true)]
    checksum: Option<String>,
    /// Proxy URL (http://, https://, socks5://)
    #[arg(long, default_value = "")]
    proxy: String,
    /// Retries per connection
    #[arg(long, default_value_t = 5)]
    retries: u32,
    /// No progress output
    #[arg(short, long)]
    quiet: bool,
}

fn state_path(dir: &Path, url: &str) -> PathBuf {
    // FNV-1a of the URL keeps one resume file per URL.
    let h = url.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    dir.join(format!(".dlx-{h:016x}.json"))
}

/// Usage errors exit 2 before any transfer starts.
fn usage(json: bool, message: String) -> ! {
    if json {
        print_json(&json!({"event":"error","code":"invalid","message":message}));
    } else {
        eprintln!("{message}");
    }
    finish(json, 0, 0, cli::USAGE)
}

/// The NDJSON summary (when --json) and exit.
fn finish(json: bool, completed: u32, failed: u32, code: i32) -> ! {
    if json {
        print_json(
            &json!({"event":"summary","completed":completed,"failed":failed,"exit_code":code}),
        );
    }
    std::process::exit(code)
}

fn report_url_error(json: bool, url: &str, error: &CliError) {
    if json {
        print_json(&json!({"event":"error","url":url,"code":error.code,"message":error.message}));
    } else {
        eprintln!("error: {url}: {}", error.message);
    }
}

/// Approvals from the default state directory's settings.json (written by "dlxctl modules approve").
fn approved_modules(json: bool) -> ModuleSettings {
    let path = dlcore::ipc::default_state_dir().join("settings.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Settings>(&bytes)
            .map(|settings| settings.modules)
            .unwrap_or_else(|e| usage(json, format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ModuleSettings::default(),
        Err(e) => usage(json, format!("{}: {e}", path.display())),
    }
}

/// Approvals bind a module id and manifest hash to the state directory's modules folder, so
/// another folder is refused, and the global enable switch is honoured.
fn load_modules(json: bool, quiet: bool, dir: &Path) -> Arc<Registry> {
    let home = dlcore::modules::modules_dir();
    let same = match (std::fs::canonicalize(dir), std::fs::canonicalize(&home)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if !same {
        usage(
            json,
            format!(
                "--modules-dir must be the modules folder {}: approvals are only valid there",
                home.display()
            ),
        );
    }
    let modules = approved_modules(json);
    if !modules.enabled && !quiet {
        eprintln!("modules are disabled; run \"dlxctl modules enable\" to use them");
    }
    Registry::load(&modules, &home)
}

#[tokio::main]
async fn main() {
    let a = Args::parse();
    let json = a.json;
    if a.schema {
        print_json(&schema::dlx_schema(Args::command()));
        return;
    }
    if a.tools {
        let tools = dlcore::protocols::capabilities();
        if json {
            let list: Vec<Value> = tools
                .iter()
                .map(|tool| json!({"name": tool.name, "available": tool.available}))
                .collect();
            print_json(&json!({"event":"tools","tools":list}));
        } else {
            for tool in &tools {
                println!(
                    "{}: {}",
                    tool.name,
                    if tool.available {
                        "built-in Rust"
                    } else {
                        "unavailable"
                    }
                );
            }
        }
        if a.urls.is_empty() {
            return;
        }
    }
    for (index, url) in a.urls.iter().enumerate() {
        if let Err(error) = util::validate_source(url) {
            usage(json, format!("source {}: {error}", index + 1));
        }
    }
    if !a.mirrors.is_empty() && a.urls.len() != 1 {
        usage(json, "--mirror needs exactly one URL".into());
    }
    let mirrors = cli::check_mirrors(&a.mirrors).unwrap_or_else(|e| usage(json, e));
    let kind = if a.audio {
        DownloadKind::Audio
    } else if a.video {
        DownloadKind::Video
    } else {
        DownloadKind::Auto
    };
    let limit = util::parse_size(&a.limit).unwrap_or_else(|| usage(json, "invalid --limit".into()));
    let v = a.checksum.clone().unwrap_or_else(|| a.verify.clone());
    let auto_verify = v != "off";
    let checksum = if v == "auto" || v == "off" {
        None
    } else {
        Some(Checksum::guess(&v).unwrap_or_else(|| {
            usage(
                json,
                "--verify must be auto, off, or a 32/40/64/128-character hex digest".into(),
            )
        }))
    };
    let headers: Vec<(String, String)> = a
        .headers
        .iter()
        .map(|raw| parse_header(raw).unwrap_or_else(|e| usage(json, e)))
        .collect();
    if let Err(error) = dlcore::task::validate_headers(&headers) {
        usage(json, error);
    }
    let settings = Settings {
        proxy: a.proxy.clone(),
        ..Default::default()
    };
    let client = settings
        .build_client()
        .unwrap_or_else(|e| usage(json, e.to_string()));
    let registry = a
        .modules_dir
        .as_deref()
        .map(|dir| load_modules(json, a.quiet, dir));
    let cancel = CancellationToken::new();
    let protocol_client = settings
        .build_protocol_client()
        .unwrap_or_else(|e| usage(json, e.to_string()));
    {
        let c = cancel.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            c.cancel();
        });
    }
    let global = Arc::new(RateLimiter::new(0));
    let mut code = 0;
    let (mut completed, mut failed) = (0u32, 0u32);
    // Hook deliveries run beside the next download and are awaited before exit.
    let mut hooks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for url in &a.urls {
        let sp = state_path(&a.dir, url);
        let task = std::fs::read(&sp)
            .ok()
            .and_then(|b| serde_json::from_slice::<Task>(&b).ok())
            .filter(|t| {
                t.url == *url
                    && t.kind == kind
                    && (t.part_path().exists() || dlcore::protocols::staging_path(t).exists())
            })
            .unwrap_or_else(|| Task {
                url: url.trim().into(),
                kind,
                dir: a.dir.clone(),
                ..Default::default()
            });
        let resumed = task.downloaded();
        // Flags are authoritative on every run; nothing from an earlier state file leaks in.
        let task = Arc::new(Mutex::new(Task {
            connections: a.connections.clamp(1, 32),
            checksum: checksum.clone(),
            status: Status::Downloading,
            verify_retried: false,
            headers: headers.clone(),
            mirrors: mirrors.clone(),
            user_agent: a.user_agent.clone(),
            referer: a.referer.clone(),
            conflict: a.on_conflict.into(),
            ..task
        }));
        let prepared = match &registry {
            Some(registry) => registry.prepare(&task, &cancel).await,
            None => Ok(()),
        };
        if let Err(error) = prepared {
            let error = CliError::from(error);
            failed += 1;
            code = code.max(error.exit_code());
            report_url_error(json, url, &error);
            if let Some(event) =
                HookEvent::for_result(&task.lock().unwrap(), &Err(error.message.clone()))
            {
                spawn_hook(&registry, &mut hooks, event);
            }
            continue;
        }
        if resumed > 0 && !a.quiet {
            eprintln!("resuming at {}", util::human_size(resumed));
        }
        let ctx = Ctx {
            protocol_client: protocol_client.clone(),
            client: client.clone(),
            global: global.clone(),
            local: Arc::new(RateLimiter::new(limit)),
            retries: a.retries,
            cancel: cancel.clone(),
            verify: auto_verify,
        };
        if json {
            print_json(&json!({"event":"start","url":url,"resumed_bytes":resumed}));
        }
        let start = Instant::now();
        let job = tokio::spawn(engine::run(task.clone(), ctx));
        let mut last_save = Instant::now();
        let mut last_json = Instant::now();
        while !job.is_finished() {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let t = task.lock().unwrap().clone();
            if last_save.elapsed() > Duration::from_secs(2) && !t.file_name.is_empty() {
                last_save = Instant::now();
                let _ = std::fs::write(&sp, serde_json::to_vec(&t).unwrap());
            }
            if !a.quiet {
                let d = t.downloaded();
                let el = start.elapsed().as_secs_f64().max(0.01);
                let rate = (d.saturating_sub(resumed)) as f64 / el;
                let pct = t
                    .total
                    .map(|n| format!("{:5.1}%", d as f64 * 100.0 / n.max(1) as f64))
                    .unwrap_or_default();
                let conns = t.segments.iter().filter(|s| s.busy).count();
                eprint!(
                    "\r{} {pct} {} / {}  {}/s  [{conns} conn]   ",
                    t.display_name(),
                    util::human_size(d),
                    t.total.map(util::human_size).unwrap_or("?".into()),
                    util::human_size(rate as u64)
                );
                let _ = std::io::stderr().flush();
                if json && last_json.elapsed() >= Duration::from_secs(1) {
                    last_json = Instant::now();
                    print_json(&json!({
                        "event": "progress",
                        "url": url,
                        "downloaded": d,
                        "total": t.total,
                        "bytes_per_sec": rate as u64,
                        "connections": conns,
                    }));
                }
            }
        }
        let r = match job.await {
            Ok(r) => r,
            Err(e) => {
                if !a.quiet {
                    eprintln!();
                }
                let error = CliError::new("error", format!("download task failed: {e}"));
                failed += 1;
                code = code.max(error.exit_code());
                report_url_error(json, url, &error);
                continue;
            }
        };
        let t = task.lock().unwrap().clone();
        if !a.quiet {
            eprintln!();
        }
        let event = HookEvent::for_result(&t, &r);
        match r {
            Ok(Outcome::Completed) => {
                let _ = std::fs::remove_file(&sp);
                if !a.quiet {
                    report_verification(&t);
                }
                let el = start.elapsed().as_secs_f64();
                let total = t.total.unwrap_or_else(|| t.downloaded());
                completed += 1;
                if json {
                    print_json(&json!({
                        "event": "done",
                        "url": url,
                        "path": t.final_path().display().to_string(),
                        "size": total,
                        "seconds": el,
                        "verification": t.verification,
                    }));
                } else {
                    println!(
                        "{}  {} in {:.2}s ({}/s)",
                        t.final_path().display(),
                        util::human_size(total),
                        el,
                        util::human_size(
                            (total.saturating_sub(resumed) as f64 / el.max(0.001)) as u64
                        )
                    );
                }
            }
            Ok(Outcome::Cancelled) => {
                let _ = std::fs::write(&sp, serde_json::to_vec(&t).unwrap());
                let message = "interrupted; run the same command again to resume";
                if json {
                    print_json(
                        &json!({"event":"error","url":url,"code":"interrupted","message":message}),
                    );
                } else {
                    eprintln!("{message}");
                }
                stop_hooks(hooks).await;
                finish(json, completed, failed, cli::INTERRUPTED);
            }
            Err(e) => {
                if !a.quiet {
                    report_verification(&t);
                }
                if t.resumable {
                    let _ = std::fs::write(&sp, serde_json::to_vec(&t).unwrap());
                }
                let error = CliError::from(e);
                failed += 1;
                code = code.max(error.exit_code());
                report_url_error(json, url, &error);
            }
        }
        if let Some(event) = event {
            spawn_hook(&registry, &mut hooks, event);
        }
    }
    if wait_hooks(&mut hooks, &cancel).await {
        stop_hooks(hooks).await;
        finish(json, completed, failed, cli::INTERRUPTED);
    }
    finish(json, completed, failed, code);
}

/// Deliver a hook event in the background; nothing is spawned without modules.
fn spawn_hook(
    registry: &Option<Arc<Registry>>,
    hooks: &mut Vec<tokio::task::JoinHandle<()>>,
    event: HookEvent,
) {
    if let Some(registry) = registry.clone() {
        hooks.push(tokio::spawn(async move {
            registry.notify_wait(event).await;
        }));
    }
}

/// Wait for every hook; true when Ctrl+C arrived first. Finished hooks win over a cancel.
async fn wait_hooks(hooks: &mut [tokio::task::JoinHandle<()>], cancel: &CancellationToken) -> bool {
    if hooks.is_empty() {
        return false;
    }
    let all = async {
        for hook in hooks.iter_mut() {
            let _ = hook.await;
        }
    };
    tokio::select! {
        biased;
        _ = all => false,
        _ = cancel.cancelled() => true,
    }
}

/// Abort running hook deliveries so their processes are dropped and killed.
async fn stop_hooks(hooks: Vec<tokio::task::JoinHandle<()>>) {
    for hook in &hooks {
        hook.abort();
    }
    for hook in hooks {
        // A handle already awaited by `wait_hooks` is finished and must not be polled again.
        if !hook.is_finished() {
            let _ = hook.await;
        }
    }
}

fn report_verification(t: &Task) {
    use dlcore::VerifyState::*;
    let Some(v) = &t.verification else { return };
    let algo = v.algo.map(|a| a.name()).unwrap_or("");
    match v.state {
        Verified => eprintln!("verified: {algo} {} (from {})", v.actual, v.source),
        Failed => eprintln!(
            "VERIFICATION FAILED: {algo} expected {} got {} (from {})",
            v.expected, v.actual, v.source
        ),
        NoChecksum => eprintln!("no checksum found; integrity not verified (size check passed)"),
        Unverified => eprintln!("not verified: {}", v.note),
        Verifying => {}
    }
    if let Some(s) = &v.signature {
        eprintln!("signature available: {s} (not verified)");
    }
}
