//! `dlxctl doctor`: local diagnostics. Each check is {id, status, message, hint?}.
//! The token is only ever reported as present or absent.
use crate::cli::{CliError, print_json};
use crate::settings::read_settings;
use dlcore::browser::{self, Browser};
use dlcore::modules::Registry;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Check {
    id: &'static str,
    status: &'static str,
    message: String,
    hint: Option<String>,
}

fn check(id: &'static str, status: &'static str, message: impl Into<String>) -> Check {
    Check {
        id,
        status,
        message: message.into(),
        hint: None,
    }
}

impl Check {
    fn hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_string());
        self
    }
    fn json(&self) -> Value {
        let mut value = json!({"id": self.id, "status": self.status, "message": self.message});
        if let Some(hint) = &self.hint {
            value["hint"] = json!(hint);
        }
        value
    }
}

/// Creates and removes a probe file; `Err` carries the reason.
fn writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(format!(".dlx-doctor-{}", std::process::id()));
    std::fs::write(&probe, b"").map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn state_dir(dir: &Path) -> Check {
    let shown = dir.display();
    if !dir.exists() {
        return check("state-dir", "warn", format!("{shown} does not exist yet")).hint(
            "It is created on first use; set --data-dir or DOWNLOADER_STATE_DIR to choose another.",
        );
    }
    match writable(dir) {
        Ok(()) => check("state-dir", "ok", shown.to_string()),
        Err(e) => check("state-dir", "fail", format!("{shown} is not writable: {e}"))
            .hint("Fix the permissions or point --data-dir / DOWNLOADER_STATE_DIR elsewhere."),
    }
}

fn download_checks(folder: &Path) -> Vec<Check> {
    let mut out = Vec::new();
    let shown = folder.display();
    if !folder.exists() {
        out.push(
            check(
                "download-dir",
                "warn",
                format!("{shown} does not exist yet"),
            )
            .hint("It is created when the first download starts."),
        );
    } else {
        out.push(match writable(folder) {
            Ok(()) => check("download-dir", "ok", shown.to_string()),
            Err(e) => check(
                "download-dir",
                "fail",
                format!("{shown} is not writable: {e}"),
            )
            .hint("Choose another folder with `dlxctl add --dir` or in the app settings."),
        });
    }
    out.push(match dlcore::util::free_space(folder) {
        Some(free) if free < 1 << 30 => check(
            "disk-space",
            "warn",
            format!("{} free", dlcore::util::human_size(free)),
        )
        .hint("Large downloads may be refused with `not enough disk space:`."),
        Some(free) => check(
            "disk-space",
            "ok",
            format!("{} free", dlcore::util::human_size(free)),
        ),
        None => check("disk-space", "ok", "free space unknown on this platform"),
    });
    out
}

fn modules_check(dir: &Path, settings: &dlcore::Settings) -> Check {
    let list = Registry::load(&settings.modules, &dir.join("modules")).list();
    let external: Vec<_> = list.iter().filter(|m| !m.builtin).collect();
    let approved = external.iter().filter(|m| m.approved).count();
    let problems = external.iter().filter(|m| m.error.is_some()).count();
    let unapproved = external.len() - approved;
    let message = format!(
        "{}; {approved} approved, {unapproved} unapproved, {problems} with problems",
        if settings.modules.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    if problems > 0 {
        check("modules", "warn", message).hint("Run `dlxctl modules list --json` for the errors.")
    } else {
        check("modules", "ok", message)
    }
}

fn native_host_check(host: &Path) -> Check {
    let mut registered = Vec::new();
    let mut issues = Vec::new();
    for browser in Browser::ALL {
        let status = browser::native_host_status(browser, host);
        if status.registered {
            registered.push(browser.name());
        } else if status.manifest_path.is_some()
            && let Some(issue) = status.issue
        {
            issues.push(format!("{}: {issue}", browser.name()));
        }
    }
    if !issues.is_empty() {
        return check("native-host", "warn", issues.join("; "))
            .hint("Register the connector again from the app's browser settings.");
    }
    if registered.is_empty() {
        return check("native-host", "warn", "no browser connector registered")
            .hint("Optional: register it from the app's browser settings.");
    }
    check(
        "native-host",
        "ok",
        format!("registered for {}", registered.join(", ")),
    )
}

fn binaries_check(exe_dir: Option<&Path>) -> Check {
    let Some(dir) = exe_dir else {
        return check("binaries", "warn", "cannot locate this executable");
    };
    let name = |base: &str| {
        if cfg!(windows) {
            format!("{base}.exe")
        } else {
            base.to_string()
        }
    };
    let found = |base: &str| dir.join(name(base)).is_file();
    let mut parts = vec![format!("dlxctl {}", env!("CARGO_PKG_VERSION"))];
    let mut missing = Vec::new();
    for base in ["dlx", "downloader-host"] {
        if found(base) {
            parts.push(base.to_string());
        } else {
            missing.push(base);
        }
    }
    parts.push(
        if found("downloader") {
            "downloader (GUI)"
        } else {
            "downloader (GUI, not installed)"
        }
        .to_string(),
    );
    let message = format!("{} in {}", parts.join(", "), dir.display());
    if missing.is_empty() {
        check("binaries", "ok", message)
    } else {
        check(
            "binaries",
            "warn",
            format!("{message}; missing {}", missing.join(", ")),
        )
        .hint("Keep dlx, dlxctl and downloader-host in the same folder.")
    }
}

async fn remote_check(endpoint: &str, token: Option<&str>) -> Check {
    let url = match crate::parse_endpoint(endpoint).and_then(|u| {
        u.join("/version")
            .map_err(|e| CliError::new("invalid", e.to_string()))
    }) {
        Ok(url) => url,
        Err(e) => return check("remote", "fail", e.message).hint("Fix --endpoint."),
    };
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(e) => return check("remote", "warn", e.without_url().to_string()),
    };
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    match request.send().await {
        Err(_) => check(
            "remote",
            "warn",
            format!("no manager listening at {endpoint}"),
        )
        .hint("Start one with `dlxctl serve` (or enable remote control in the app)."),
        Ok(response) => match response.status().as_u16() {
            200..=299 if token.is_some() => check(
                "remote",
                "ok",
                format!("{endpoint} reachable, token accepted"),
            ),
            200..=299 => check("remote", "ok", format!("{endpoint} reachable")),
            401 | 403 if token.is_none() => check(
                "remote",
                "warn",
                format!("{endpoint} reachable; no token to test"),
            )
            .hint("Set DOWNLOADER_REMOTE_TOKEN."),
            401 | 403 => check("remote", "fail", format!("{endpoint} rejected the token"))
                .hint("Use the token the manager was started with (DOWNLOADER_REMOTE_TOKEN)."),
            code => check("remote", "warn", format!("{endpoint} answered HTTP {code}")),
        },
    }
}

async fn site_check(id: &'static str, url: &str) -> Check {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build();
    let result = match client {
        Ok(client) => client.get(url).send().await,
        Err(e) => return check(id, "warn", e.without_url().to_string()),
    };
    match result {
        Ok(response) => check(
            id,
            "ok",
            format!("{url} reachable (HTTP {})", response.status().as_u16()),
        ),
        Err(e) => check(
            id,
            "warn",
            format!("{url} unreachable: {}", e.without_url()),
        )
        .hint("Check connectivity, DNS or the proxy settings."),
    }
}

pub fn doctor(
    dir: &Path,
    json: bool,
    token: Option<&str>,
    endpoint: &str,
    online: bool,
) -> Result<(), CliError> {
    let mut checks = vec![state_dir(dir)];
    let settings = match read_settings(dir) {
        Ok(settings) => {
            checks.push(check(
                "settings",
                "ok",
                "settings.json readable (defaults when missing)",
            ));
            settings
        }
        Err(e) => {
            checks.push(
                check("settings", "fail", format!("settings.json: {e}"))
                    .hint("Repair or delete settings.json in the state directory."),
            );
            dlcore::Settings::default()
        }
    };
    checks.extend(download_checks(&settings.download_dir));
    checks.push(if token.is_some() {
        check("token", "ok", "DOWNLOADER_REMOTE_TOKEN or --token present")
    } else {
        check("token", "warn", "no token set")
            .hint("Set DOWNLOADER_REMOTE_TOKEN (`dlxctl generate-token` makes one).")
    });
    let exe_dir: Option<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    checks.push(runtime.block_on(remote_check(endpoint, token)));
    checks.push(modules_check(dir, &settings));
    let host = exe_dir
        .as_deref()
        .map(|d| d.join(browser::host_exe_name()))
        .unwrap_or_default();
    checks.push(native_host_check(&host));
    checks.push(binaries_check(exe_dir.as_deref()));
    if online {
        checks.push(runtime.block_on(site_check("net-youtube", "https://www.youtube.com")));
        checks.push(runtime.block_on(site_check(
            "net-twitter-syndication",
            "https://cdn.syndication.twimg.com",
        )));
    }
    let count = |status: &str| checks.iter().filter(|c| c.status == status).count();
    let (ok, warn, fail) = (count("ok"), count("warn"), count("fail"));
    if json {
        let data = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "checks": checks.iter().map(Check::json).collect::<Vec<_>>(),
            "summary": {"ok": ok, "warn": warn, "fail": fail},
        });
        if fail == 0 {
            print_json(&crate::cli::envelope_ok(data));
        } else {
            // ok:false with the checks attached; exit 1 (class "doctor").
            let mut value = crate::cli::envelope_err(&CliError::new(
                "doctor",
                format!("{fail} check(s) failed"),
            ));
            value["data"] = data;
            print_json(&value);
        }
    } else {
        for c in &checks {
            println!("{:<7}{:<18}{}", format!("[{}]", c.status), c.id, c.message);
            if let Some(hint) = &c.hint {
                println!("{:<25}hint: {hint}", "");
            }
        }
        println!("{ok} ok, {warn} warn, {fail} fail");
    }
    if fail > 0 {
        std::process::exit(crate::cli::FAILED);
    }
    Ok(())
}
