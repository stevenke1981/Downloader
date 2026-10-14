//! dlx - headless multi-connection downloader.
use clap::Parser;
use dlcore::{Checksum, Ctx, Outcome, RateLimiter, Settings, Status, Task, engine, util};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "dlx", version, about = "Multi-connection HTTP(S) downloader (resumable)")]
struct Args {
    /// URL(s) to download
    #[arg(required = true)]
    urls: Vec<String>,
    /// Output directory
    #[arg(short = 'o', long, default_value = ".")]
    dir: PathBuf,
    /// Connections per download (1-32)
    #[arg(short = 'c', long, default_value_t = 8)]
    connections: u32,
    /// Speed limit, e.g. 500K, 2M (0 = unlimited)
    #[arg(short = 'l', long, default_value = "0")]
    limit: String,
    /// Expected MD5 or SHA-256 hex digest (algorithm detected by length)
    #[arg(long)]
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
    let h = url.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    dir.join(format!(".dlx-{h:016x}.json"))
}

#[tokio::main]
async fn main() {
    let a = Args::parse();
    let limit = util::parse_size(&a.limit).unwrap_or_else(|| {
        eprintln!("invalid --limit");
        std::process::exit(2)
    });
    let checksum = a.checksum.as_deref().map(|c| {
        Checksum::guess(c).unwrap_or_else(|| {
            eprintln!("--checksum must be a 32 (MD5) or 64 (SHA-256) hex digest");
            std::process::exit(2)
        })
    });
    let settings = Settings { proxy: a.proxy.clone(), ..Default::default() };
    let client = settings.build_client().unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    let cancel = CancellationToken::new();
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
            .filter(|t| t.url == *url && t.part_path().exists())
            .unwrap_or_else(|| Task { url: url.clone(), dir: a.dir.clone(), ..Default::default() });
        let resumed = task.downloaded();
        let task = Arc::new(Mutex::new(Task {
            connections: a.connections.clamp(1, 32),
            checksum: checksum.clone(),
            status: Status::Downloading,
            ..task
        }));
        if resumed > 0 && !a.quiet {
            eprintln!("resuming at {}", util::human_size(resumed));
        }
        let ctx = Ctx {
            client: client.clone(),
            global: global.clone(),
            local: Arc::new(RateLimiter::new(limit)),
            retries: a.retries,
            cancel: cancel.clone(),
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
                let pct = t.total.map(|n| format!("{:5.1}%", d as f64 * 100.0 / n.max(1) as f64)).unwrap_or_default();
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
        let r = job.await.unwrap();
        let t = task.lock().unwrap().clone();
        if !a.quiet {
            eprintln!();
        }
        match r {
            Ok(Outcome::Completed) => {
                let _ = std::fs::remove_file(&sp);
                let el = start.elapsed().as_secs_f64();
                println!(
                    "{}  {} in {:.2}s ({}/s)",
                    t.final_path().display(),
                    util::human_size(t.total.unwrap_or(0)),
                    el,
                    util::human_size(((t.total.unwrap_or(0) - resumed) as f64 / el.max(0.001)) as u64)
                );
            }
            Ok(Outcome::Cancelled) => {
                let _ = std::fs::write(&sp, serde_json::to_vec(&t).unwrap());
                eprintln!("interrupted; run the same command again to resume");
                std::process::exit(130);
            }
            Err(e) => {
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
