//! Queue control and local management utilities. Settings edits apply on next manager start.
use clap::{Parser, Subcommand};
use dlcore::{
    Manager, Settings,
    automation::{Action, ScheduledAction},
    grabber::{self, GrabOptions},
    remote::{self, RemoteSettings},
    site::SiteRule,
};
use std::path::{Path, PathBuf};

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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    List,
    Add {
        url: String,
        #[arg(short, long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        paused: bool,
        #[arg(long)]
        video: bool,
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
    if let Err(error) = run(Args::parse()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<(), String> {
    let dir = args.data_dir.unwrap_or_else(dlcore::ipc::default_state_dir);
    let token = args
        .token
        .or_else(|| std::env::var("DOWNLOADER_REMOTE_TOKEN").ok());
    match args.command {
        Command::GenerateToken => println!("{}", remote::generate_token()?),
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
            for link in runtime.block_on(grabber::crawl(
                &client,
                &url,
                GrabOptions {
                    max_depth: depth,
                    max_pages: pages,
                },
            ))? {
                println!("{link}");
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
            for entry in runtime.block_on(dlcore::external::list_ftp(&url, &username, &password))? {
                println!("{entry}");
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
                SiteCommand::List => println!(
                    "{}",
                    serde_json::to_string_pretty(&settings.site_rules).map_err(|e| e.to_string())?
                ),
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
                        host: origin,
                        username,
                        max_concurrent: concurrent,
                        connections,
                    });
                    save_settings(&dir, &settings)?;
                }
                SiteCommand::Remove { origin } => {
                    let origin = dlcore::site::origin(&origin)?;
                    settings.site_rules.retain(|rule| {
                        dlcore::site::origin(&rule.host).ok().as_ref() != Some(&origin)
                    });
                    save_settings(&dir, &settings)?;
                }
            }
        }
        Command::Automation { command } => {
            let mut settings = read_settings(&dir)?;
            if matches!(command, AutomationCommand::List) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&settings.scheduled_actions)
                        .map_err(|e| e.to_string())?
                );
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
            }
        }
        command => {
            let token = token.ok_or("provide --token or DOWNLOADER_REMOTE_TOKEN")?;
            let (method, path, body) = match command {
                Command::List => (reqwest::Method::GET, "/downloads".into(), None),
                Command::Add {
                    url,
                    dir,
                    paused,
                    video,
                } => (
                    reqwest::Method::POST,
                    "/downloads".into(),
                    Some(
                        serde_json::json!({"url":url,"dir":dir,"start":!paused,"kind":if video {"Video"} else {"Auto"}}),
                    ),
                ),
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
                _ => unreachable!(),
            };
            let endpoint = reqwest::Url::parse(&args.endpoint).map_err(|e| e.to_string())?;
            if !matches!(endpoint.scheme(), "http" | "https")
                || !endpoint.username().is_empty()
                || endpoint.password().is_some()
            {
                return Err("endpoint must be HTTP(S) without embedded credentials".into());
            }
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .map_err(|e| e.to_string())?;
                let mut request = client
                    .request(method, endpoint.join(&path).map_err(|e| e.to_string())?)
                    .bearer_auth(token);
                if let Some(body) = body {
                    request = request
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(body.to_string());
                }
                let response = request
                    .send()
                    .await
                    .map_err(|e| e.without_url().to_string())?;
                let status = response.status();
                let body = response
                    .text()
                    .await
                    .map_err(|e| e.without_url().to_string())?;
                if !status.is_success() {
                    return Err(format!("HTTP {status}: {body}"));
                }
                println!("{body}");
                Ok(())
            })?;
        }
    }
    Ok(())
}

fn read_settings(dir: &Path) -> Result<Settings, String> {
    match std::fs::read(dir.join("settings.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error.to_string()),
    }
}

fn save_settings(dir: &Path, settings: &Settings) -> Result<(), String> {
    use std::io::Write;
    settings.validate()?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    let temporary = dir.join("settings.json.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(&json)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&temporary, dir.join("settings.json")).map_err(|e| e.to_string())?;
    println!("Saved. Settings changes apply when the manager next starts.");
    Ok(())
}
