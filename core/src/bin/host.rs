//! Native messaging host for the Downloader browser extension.
//!
//! Chrome/Edge start this with the extension origin as argument and talk over
//! stdin/stdout. Requests are forwarded to the running app over local IPC;
//! if the app isn't running it is started (it lives next to this binary).
#![cfg_attr(windows, windows_subsystem = "console")]
use dlcore::ipc::{self, AddRequest, Command, Reply};
use dlcore::native::{read_message, write_message};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn app_path() -> Option<std::path::PathBuf> {
    let me = std::env::current_exe().ok()?;
    let p = me.with_file_name(if cfg!(windows) { "downloader.exe" } else { "downloader" });
    p.exists().then_some(p)
}

fn forward(cmd: Command, launch: bool) -> Reply {
    let dir = ipc::default_state_dir();
    if let Ok(r) = ipc::send_to_app(&dir, cmd.clone()) {
        return r;
    }
    if !launch {
        return Reply::err("app not running");
    }
    let Some(app) = app_path() else { return Reply::err("Downloader app not found next to the host") };
    let _ = std::fs::remove_file(ipc::endpoint_path(&dir));
    let mut c = std::process::Command::new(app);
    c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Err(e) = c.spawn() {
        return Reply::err(format!("cannot start Downloader: {e}"));
    }
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(300));
        if let Ok(r) = ipc::send_to_app(&dir, cmd.clone()) {
            return r;
        }
    }
    Reply::err("Downloader did not start in time")
}

fn handle(msg: &Value) -> Reply {
    match msg.get("type").and_then(Value::as_str).unwrap_or("") {
        "ping" => forward(Command::Ping, false),
        "show" => forward(Command::Show, true),
        "add" => {
            let items: Vec<AddRequest> = match msg.get("downloads") {
                Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
                None => serde_json::from_value(msg.clone()).map(|a| vec![a]).unwrap_or_default(),
            };
            let items: Vec<AddRequest> =
                items.into_iter().filter(|a| a.url.starts_with("http://") || a.url.starts_with("https://")).collect();
            if items.is_empty() {
                return Reply::err("no http(s) URL");
            }
            let launch = msg.get("launch").and_then(Value::as_bool).unwrap_or(true);
            forward(Command::Add { downloads: items }, launch)
        }
        t => Reply::err(format!("unknown message type '{t}'")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("downloader-host {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    while let Ok(Some(msg)) = read_message(&mut stdin) {
        let r = handle(&msg);
        let v = serde_json::to_value(&r).unwrap_or(json!({"ok": false}));
        if write_message(&mut stdout, &v).is_err() {
            break;
        }
    }
}
