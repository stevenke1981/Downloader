//! Queue control and local management utilities. Settings edits apply on next manager start.
#[path = "shared/cli.rs"]
mod cli;

#[path = "dlxctl/modules.rs"]
mod modules;
#[path = "dlxctl/settings.rs"]
mod settings;

use modules::{ModuleCommand, modules_command};
use settings::{APPLIES, APPLIES_SAVED, read_settings, save_settings, saved};

use clap::{Parser, Subcommand};
use cli::{CliError, ConflictArg, envelope_err, envelope_ok, parse_header, print_json};
use dlcore::{
    Checksum, Manager, Settings,
    automation::{Action, ScheduledAction},
    grabber::{self, GrabOptions},
    remote::{self, RemoteSettings},
    site::SiteRule,
    task::{ConflictPolicy, validate_headers},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "dlxctl",
    version,
    about = "Downloader remote control, crawl and local settings tools"
)]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8765")]
    endpoint: String,
    /// Bearer token. Prefer DOWNLOADER_REMOTE_TOKEN to avoid command-line history.
    #[arg(long)]
    token: Option<String>,
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Machine output: {"ok":true,"data":..} or {"ok":false,"error":{"code","message"}} on stdout.
    /// Exit codes: 0 ok, 1 failed, 2 usage, 3 network, 4 auth, 5 verification, 6 conflict or duplicate, 7 module.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    List,
    /// One download as JSON.
    Status {
        id: u64,
    },
    /// Wait until downloads complete. Exit status is the most severe failure.
    Wait {
        #[arg(required = true)]
        ids: Vec<u64>,
        /// Give up after this many seconds (exit 1).
        #[arg(long)]
        timeout: Option<u64>,
    },
    Add {
        url: String,
        #[arg(short, long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        paused: bool,
        #[arg(long)]
        video: bool,
        /// Download the audio of a supported site (YouTube) as MP3.
        #[arg(long, conflicts_with = "video")]
        audio: bool,
        /// Refuse (exit 6) when this URL is already queued or active.
        #[arg(long)]
        unique: bool,
        #[arg(long, allow_hyphen_values = true)]
        priority: Option<i32>,
        /// Expected digest: hex (MD5, SHA-1, SHA-256, SHA-512 by length) or b2:<hex>.
        #[arg(long)]
        checksum: Option<String>,
        /// Extra request header "Name: value" (repeatable).
        #[arg(long = "header", value_name = "NAME: VALUE")]
        headers: Vec<String>,
        /// When the file exists: rename, overwrite or skip.
        #[arg(long, value_enum)]
        on_conflict: Option<ConflictArg>,
        /// Alternative http(s) source tried when the URL fails (repeatable, max 8).
        #[arg(long = "mirror", value_name = "URL")]
        mirrors: Vec<String>,
    },
    Start {
        id: u64,
    },
    Pause {
        id: u64,
    },
    Remove {
        id: u64,
    },
    StartAll,
    PauseAll,
    /// Queue as JSON (no cookies, credentials or logs).
    Export,
    /// Add the downloads in an export file.
    Import {
        file: PathBuf,
        /// Start the imported downloads (default: add them paused).
        #[arg(long)]
        start: bool,
    },
    /// Run a headless queue manager with authenticated HTTP control until Ctrl+C.
    Serve {
        #[arg(long)]
        bind: Option<String>,
        /// Session-only credentials for this full origin (bare host means HTTPS).
        #[arg(long)]
        site_origin: Option<String>,
        #[arg(long, default_value = "")]
        username: String,
        /// Name of an environment variable holding the session password.
        #[arg(long)]
        password_env: Option<String>,
    },
    Crawl {
        url: String,
        #[arg(long, default_value_t = 1)]
        depth: usize,
        #[arg(long, default_value_t = 16)]
        pages: usize,
        #[arg(long, default_value = "")]
        proxy: String,
    },
    /// List an FTP directory. Credentials stay in memory for this invocation.
    Ftp {
        url: String,
        #[arg(long, default_value = "")]
        username: String,
        #[arg(long)]
        password_env: Option<String>,
    },
    /// Edit profiles while the GUI/headless manager is stopped.
    Site {
        #[command(subcommand)]
        command: SiteCommand,
    },
    /// Edit one-shot actions while the GUI/headless manager is stopped.
    Automation {
        #[command(subcommand)]
        command: AutomationCommand,
    },
    /// Installed modules and approvals. Edits settings while the manager is stopped.
    Modules {
        #[command(subcommand)]
        command: ModuleCommand,
    },
    GenerateToken,
}

#[derive(Subcommand)]
enum SiteCommand {
    List,
    Set {
        origin: String,
        #[arg(long, default_value = "")]
        username: String,
        #[arg(long, default_value_t = 2)]
        concurrent: u32,
        #[arg(long, default_value_t = 4)]
        connections: u32,
    },
    Remove {
        origin: String,
    },
}

#[derive(Subcommand)]
enum AutomationCommand {
    List,
    /// UTC epoch seconds; consumed actions never repeat after a restart.
    StartAll {
        at: i64,
    },
    PauseAll {
        at: i64,
    },
    Run {
        at: i64,
        program: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Explicitly schedule operating-system shutdown. No confirmation is implied.
    Shutdown {
        at: i64,
    },
    Remove {
        id: u64,
    },
}

fn main() {
    let args = Args::parse();
    let json = args.json;
    if let Err(error) = run(args) {
        if json {
            print_json(&envelope_err(&error));
        } else {
            eprintln!("{}", error.message);
        }
        std::process::exit(error.exit_code());
    }
}

fn print_envelope(data: Value) {
    print_json(&envelope_ok(data));
}

/// Human text, or the JSON envelope around the data.
fn output(json: bool, human: &str, data: Value) {
    if json {
        print_envelope(data);
    } else {
        println!("{human}");
    }
}

fn require_token(token: Option<String>) -> Result<String, CliError> {
    token.ok_or_else(|| CliError::new("auth", "provide --token or DOWNLOADER_REMOTE_TOKEN"))
}

fn parse_endpoint(text: &str) -> Result<reqwest::Url, CliError> {
    let endpoint =
        reqwest::Url::parse(text).map_err(|e| CliError::new("invalid", e.to_string()))?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
    {
        return Err(CliError::new(
            "invalid",
            "endpoint must be HTTP(S) without embedded credentials",
        ));
    }
    Ok(endpoint)
}

/// Server errors keep their text ("HTTP <status>: <body>"). The class comes from the body's
/// "code" when the server sends one, otherwise from the status.
fn http_error(status: reqwest::StatusCode, body: &str) -> CliError {
    let code = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("code")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            match status.as_u16() {
                401 | 403 => "auth",
                400 | 422 => "invalid",
                408 => "network",
                409 => "conflict",
                _ => "error",
            }
            .to_string()
        });
    CliError {
        code,
        message: format!("HTTP {status}: {body}"),
    }
}

async fn call(
    endpoint: &reqwest::Url,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<String>,
) -> Result<String, CliError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client
        .request(method, endpoint.join(path).map_err(|e| e.to_string())?)
        .bearer_auth(token);
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let response = request
        .send()
        .await
        .map_err(|e| CliError::new("network", e.without_url().to_string()))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| CliError::new("network", e.without_url().to_string()))?;
    if !status.is_success() {
        return Err(http_error(status, &text));
    }
    Ok(text)
}

/// Response text as JSON when it parses; otherwise the raw text.
fn parse_body(text: String) -> Value {
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

/// Polls each download until it completes or fails. Returns the most severe failure.
fn wait_for(
    endpoint: &reqwest::Url,
    token: &str,
    ids: &[u64],
    timeout: Option<u64>,
) -> Result<Vec<(u64, String)>, CliError> {
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let started = Instant::now();
    let mut pending: Vec<u64> = ids.to_vec();
    let mut done: BTreeMap<u64, Result<String, CliError>> = BTreeMap::new();
    runtime.block_on(async {
        while !pending.is_empty() {
            let mut still = Vec::new();
            for id in pending {
                let path = format!("/downloads/{id}");
                match call(endpoint, token, reqwest::Method::GET, &path, None).await {
                    Err(error) => {
                        done.insert(id, Err(error));
                    }
                    Ok(text) => {
                        let snapshot = parse_body(text);
                        let status = snapshot["status"].as_str().unwrap_or("").to_string();
                        match status.as_str() {
                            "Completed" => {
                                done.insert(id, Ok(status));
                            }
                            "Error" => {
                                let message = snapshot["error"]
                                    .as_str()
                                    .unwrap_or("download failed")
                                    .to_string();
                                done.insert(id, Err(CliError::from(message)));
                            }
                            _ => still.push(id),
                        }
                    }
                }
            }
            if still.is_empty() {
                break;
            }
            if timeout.is_some_and(|secs| started.elapsed() >= Duration::from_secs(secs)) {
                for id in still {
                    done.insert(
                        id,
                        Err(CliError::new(
                            "timeout",
                            format!("timed out waiting for download {id}"),
                        )),
                    );
                }
                break;
            }
            pending = still;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    let mut results = Vec::new();
    let mut failures: Vec<(u64, CliError)> = Vec::new();
    for (id, result) in done {
        match result {
            Ok(status) => results.push((id, status)),
            Err(error) => failures.push((id, error)),
        }
    }
    if failures.is_empty() {
        return Ok(results);
    }
    let worst = failures
        .iter()
        .max_by_key(|(_, error)| error.exit_code())
        .map(|(_, error)| error.code.clone())
        .unwrap_or_default();
    let message = failures
        .iter()
        .map(|(id, error)| format!("#{id}: {}", error.message))
        .collect::<Vec<_>>()
        .join("; ");
    Err(CliError::new(&worst, message))
}

fn run(args: Args) -> Result<(), CliError> {
    let json = args.json;
    let dir = args.data_dir.unwrap_or_else(dlcore::ipc::default_state_dir);
    let token = args
        .token
        .or_else(|| std::env::var("DOWNLOADER_REMOTE_TOKEN").ok());
    let endpoint_text = args.endpoint;
    match args.command {
        Command::GenerateToken => {
            let token = remote::generate_token()?;
            output(json, &token, json!({"token": token}));
        }
        Command::Crawl {
            url,
            depth,
            pages,
            proxy,
        } => {
            let client = Settings {
                proxy,
                ..Default::default()
            }
            .build_crawl_client()?;
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            let links = runtime.block_on(grabber::crawl(
                &client,
                &url,
                GrabOptions {
                    max_depth: depth,
                    max_pages: pages,
                },
            ))?;
            if json {
                print_envelope(json!(links));
            } else {
                for link in &links {
                    println!("{link}");
                }
            }
        }
        Command::Ftp {
            url,
            username,
            password_env,
        } => {
            let password = password_env
                .map(|key| {
                    std::env::var(key)
                        .map_err(|_| "password environment variable is missing".to_string())
                })
                .transpose()?
                .unwrap_or_default();
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            let entries =
                runtime.block_on(dlcore::protocols::list_ftp(&url, &username, &password))?;
            if json {
                print_envelope(json!(entries));
            } else {
                for entry in &entries {
                    println!("{entry}");
                }
            }
        }
        Command::Serve {
            bind,
            site_origin,
            username,
            password_env,
        } => {
            let manager = Manager::new(&dir);
            let mut settings = manager.settings();
            settings.remote = RemoteSettings {
                enabled: true,
                bind: bind.unwrap_or(settings.remote.bind),
                token: token.unwrap_or(settings.remote.token),
            };
            manager.set_settings(settings.clone())?;
            if let Some(origin) = site_origin {
                let password = password_env
                    .map(|key| {
                        std::env::var(key)
                            .map_err(|_| "password environment variable is missing".to_string())
                    })
                    .transpose()?
                    .unwrap_or_default();
                manager.set_site_credentials(&origin, &username, &password)?;
            }
            let mut server = remote::serve(manager.clone(), settings.remote)?;
            eprintln!("Remote control listening on {}", server.address());
            if json {
                print_envelope(json!({"listening": server.address().to_string()}));
            }
            manager
                .runtime()
                .block_on(tokio::signal::ctrl_c())
                .map_err(|e| e.to_string())?;
            server.stop();
            drop(server);
            manager.shutdown();
        }
        Command::Site { command } => {
            let mut settings = read_settings(&dir)?;
            match command {
                SiteCommand::List => {
                    if json {
                        print_envelope(
                            serde_json::to_value(&settings.site_rules)
                                .map_err(|e| e.to_string())?,
                        );
                    } else {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&settings.site_rules)
                                .map_err(|e| e.to_string())?
                        );
                    }
                }
                SiteCommand::Set {
                    origin,
                    username,
                    concurrent,
                    connections,
                } => {
                    let origin = dlcore::site::origin(&origin)?;
                    settings.site_rules.retain(|rule| {
                        dlcore::site::origin(&rule.host).ok().as_ref() != Some(&origin)
                    });
                    settings.site_rules.push(SiteRule {
                        host: origin.clone(),
                        username,
                        max_concurrent: concurrent,
                        connections,
                    });
                    save_settings(&dir, &settings)?;
                    saved(
                        json,
                        APPLIES_SAVED,
                        json!({"saved": true, "origin": origin, "applies": APPLIES}),
                    );
                }
                SiteCommand::Remove { origin } => {
                    let origin = dlcore::site::origin(&origin)?;
                    settings.site_rules.retain(|rule| {
                        dlcore::site::origin(&rule.host).ok().as_ref() != Some(&origin)
                    });
                    save_settings(&dir, &settings)?;
                    saved(
                        json,
                        APPLIES_SAVED,
                        json!({"saved": true, "applies": APPLIES}),
                    );
                }
            }
        }
        Command::Automation { command } => {
            let mut settings = read_settings(&dir)?;
            if matches!(command, AutomationCommand::List) {
                if json {
                    print_envelope(
                        serde_json::to_value(&settings.scheduled_actions)
                            .map_err(|e| e.to_string())?,
                    );
                } else {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&settings.scheduled_actions)
                            .map_err(|e| e.to_string())?
                    );
                }
            } else {
                let id = settings
                    .scheduled_actions
                    .iter()
                    .map(|action| action.id)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("action ID exhausted")?;
                let action = match command {
                    AutomationCommand::StartAll { at } => Some((at, Action::StartAll)),
                    AutomationCommand::PauseAll { at } => Some((at, Action::PauseAll)),
                    AutomationCommand::Run { at, program, args } => {
                        Some((at, Action::Run { program, args }))
                    }
                    AutomationCommand::Shutdown { at } => Some((at, Action::Shutdown)),
                    AutomationCommand::Remove { id } => {
                        settings.scheduled_actions.retain(|action| action.id != id);
                        None
                    }
                    AutomationCommand::List => unreachable!(),
                };
                if let Some((at, action)) = action {
                    settings.scheduled_actions.push(ScheduledAction {
                        id,
                        at,
                        action,
                        consumed: false,
                    });
                }
                save_settings(&dir, &settings)?;
                saved(
                    json,
                    APPLIES_SAVED,
                    json!({"saved": true, "applies": APPLIES}),
                );
            }
        }
        Command::Modules { command } => modules_command(&dir, json, command)?,
        Command::Wait { ids, timeout } => {
            let token = require_token(token)?;
            let endpoint = parse_endpoint(&endpoint_text)?;
            let results = wait_for(&endpoint, &token, &ids, timeout)?;
            if json {
                let rows: Vec<Value> = results
                    .iter()
                    .map(|(id, status)| json!({"id": id, "status": status}))
                    .collect();
                print_envelope(json!(rows));
            } else {
                for (id, status) in &results {
                    println!("{id} {status}");
                }
            }
        }
        command => {
            let token = require_token(token)?;
            let (method, path, body) = match command {
                Command::List => (reqwest::Method::GET, "/downloads".into(), None),
                Command::Status { id } => (reqwest::Method::GET, format!("/downloads/{id}"), None),
                Command::Add {
                    url,
                    dir,
                    paused,
                    video,
                    audio,
                    unique,
                    priority,
                    checksum,
                    headers,
                    on_conflict,
                    mirrors,
                } => {
                    let mut body = json!({
                        "url": url,
                        "dir": dir,
                        "start": !paused,
                        "kind": if audio {
                            "Audio"
                        } else if video {
                            "Video"
                        } else {
                            "Auto"
                        },
                    });
                    if unique {
                        body["unique"] = json!(true);
                    }
                    if let Some(priority) = priority {
                        body["priority"] = json!(priority);
                    }
                    if let Some(checksum) = checksum {
                        if Checksum::guess(&checksum).is_none() {
                            return Err(CliError::new(
                                "invalid",
                                "--checksum must be a 32/40/64/128-character hex digest, optionally prefixed b2:",
                            ));
                        }
                        body["checksum"] = json!(checksum);
                    }
                    if !headers.is_empty() {
                        let pairs = headers
                            .iter()
                            .map(|raw| parse_header(raw))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|e| CliError::new("invalid", e))?;
                        validate_headers(&pairs).map_err(|e| CliError::new("invalid", e))?;
                        body["headers"] = json!(pairs);
                    }
                    if !mirrors.is_empty() {
                        body["mirrors"] = json!(
                            cli::check_mirrors(&mirrors)
                                .map_err(|e| CliError::new("invalid", e))?
                        );
                    }
                    if let Some(policy) = on_conflict {
                        body["conflict"] = json!(ConflictPolicy::from(policy));
                    }
                    (
                        reqwest::Method::POST,
                        "/downloads".into(),
                        Some(body.to_string()),
                    )
                }
                Command::Start { id } => (
                    reqwest::Method::POST,
                    format!("/downloads/{id}/start"),
                    None,
                ),
                Command::Pause { id } => (
                    reqwest::Method::POST,
                    format!("/downloads/{id}/pause"),
                    None,
                ),
                Command::Remove { id } => {
                    (reqwest::Method::DELETE, format!("/downloads/{id}"), None)
                }
                Command::StartAll => (reqwest::Method::POST, "/start-all".into(), None),
                Command::PauseAll => (reqwest::Method::POST, "/pause-all".into(), None),
                Command::Export => (reqwest::Method::GET, "/export".into(), None),
                Command::Import { file, start } => {
                    let text = std::fs::read_to_string(&file).map_err(|e| {
                        CliError::new("invalid", format!("{}: {e}", file.display()))
                    })?;
                    serde_json::from_str::<Value>(&text).map_err(|e| {
                        CliError::new("invalid", format!("{}: {e}", file.display()))
                    })?;
                    (
                        reqwest::Method::POST,
                        format!("/import?start={start}"),
                        Some(text),
                    )
                }
                _ => unreachable!(),
            };
            let endpoint = parse_endpoint(&endpoint_text)?;
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            let body = runtime.block_on(call(&endpoint, &token, method, &path, body))?;
            if json {
                print_envelope(parse_body(body));
            } else {
                println!("{body}");
            }
        }
    }
    Ok(())
}
