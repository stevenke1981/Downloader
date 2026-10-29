//! dlx - headless multi-connection downloader.
use clap::Parser;
use dlcore::task::DownloadKind;
use dlcore::{Checksum, Ctx, Outcome, RateLimiter, Settings, Status, Task, engine, util};
use std::io::Write;
use std::path::PathBuf;
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
    #[arg(required_unless_present = "tools")]
    urls: Vec<String>,
    /// Extract direct webpage media or HLS using the built-in Rust engine
    #[arg(long)]
    video: bool,
    /// Show built-in Rust protocol engines
    #[arg(long)]
    tools: bool,
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

fn state_path(dir: &std::path::Path, url: &str) -> PathBuf {
    // FNV-1a of the URL keeps one resume file per URL.
    let h = url.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    dir.join(format!(".dlx-{h:016x}.json"))
}

#[tokio::main]
async fn main() {
    let a = Args::parse();
    if a.tools {
        for tool in dlcore::protocols::capabilities() {
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
        if a.urls.is_empty() {
            return;
        }
    }
    for (index, url) in a.urls.iter().enumerate() {
        if let Err(error) = util::validate_source(url) {
            eprintln!("source {}: {error}", index + 1);
            std::process::exit(2);
        }
    }
    let kind = if a.video {
        DownloadKind::Video
    } else {
        DownloadKind::Auto
    };
    let limit = util::parse_size(&a.limit).unwrap_or_else(|| {
        eprintln!("invalid --limit");
        std::process::exit(2)
    });
    let v = a.checksum.clone().unwrap_or_else(|| a.verify.clone());
    let auto_verify = v != "off";
    let checksum = if v == "auto" || v == "off" {
        None
    } else {
        Some(Checksum::guess(&v).unwrap_or_else(|| {
            eprintln!("--verify must be auto, off, or a 32/40/64/128-character hex digest");
            std::process::exit(2)
        }))
    };
    let settings = Settings {
        proxy: a.proxy.clone(),
        ..Default::default()
    };
    let client = settings.build_client().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    let cancel = CancellationToken::new();
    let protocol_client = settings.build_protocol_client().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    {
        let c = cancel.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            c.cancel();
        });
    }
    let global = Arc::new(RateLimiter::new(0));
    let mut code = 0;
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
        let task = Arc::new(Mutex::new(Task {
            connections: a.connections.clamp(1, 32),
            checksum: checksum.clone(),
            status: Status::Downloading,
            verify_retried: false,
            ..task
        }));
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
        let start = Instant::now();
        let job = tokio::spawn(engine::run(task.clone(), ctx));
        let mut last_save = Instant::now();
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
                let sp = (d.saturating_sub(resumed)) as f64 / el;
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
                    util::human_size(sp as u64)
                );
                let _ = std::io::stderr().flush();
            }
        }
        let r = match job.await {
            Ok(r) => r,
            Err(e) => {
                if !a.quiet {
                    eprintln!();
                }
                eprintln!("error: {url}: download task failed: {e}");
                code = 1;
                continue;
            }
        };
        let t = task.lock().unwrap().clone();
        if !a.quiet {
            eprintln!();
        }
        match r {
            Ok(Outcome::Completed) => {
                let _ = std::fs::remove_file(&sp);
                if !a.quiet {
                    report_verification(&t);
                }
                let el = start.elapsed().as_secs_f64();
                let total = t.total.unwrap_or_else(|| t.downloaded());
                println!(
                    "{}  {} in {:.2}s ({}/s)",
                    t.final_path().display(),
                    util::human_size(total),
                    el,
                    util::human_size((total.saturating_sub(resumed) as f64 / el.max(0.001)) as u64)
                );
            }
            Ok(Outcome::Cancelled) => {
                let _ = std::fs::write(&sp, serde_json::to_vec(&t).unwrap());
                eprintln!("interrupted; run the same command again to resume");
                std::process::exit(130);
            }
            Err(e) => {
                if !a.quiet {
                    report_verification(&t);
                }
                if t.resumable {
                    let _ = std::fs::write(&sp, serde_json::to_vec(&t).unwrap());
                }
                eprintln!("error: {url}: {e}");
                code = 1;
            }
        }
    }
    std::process::exit(code);
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
