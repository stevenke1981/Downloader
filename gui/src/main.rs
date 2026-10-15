// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use dlcore::util::{human_eta, human_size, looks_like_url};
use dlcore::{Checksum, Manager, NewDownload, Snapshot, Status};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

slint::include_modules!();

/// (gettext folder, native name). "en" is the source language.
const LANGS: [(&str, &str); 8] = [
    ("zh_TW", "繁體中文"),
    ("zh_CN", "简体中文"),
    ("en", "English"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("es", "Español"),
    ("de", "Deutsch"),
    ("fr", "Français"),
];

/// Map the OS locale to one of our languages. Any Chinese locale other than
/// Simplified (CN/SG/Hans) uses Traditional Chinese.
fn system_lang() -> &'static str {
    let loc = sys_locale::get_locale().unwrap_or_default().to_ascii_lowercase().replace('_', "-");
    if loc.starts_with("zh") {
        return if loc.contains("hans") || loc.ends_with("-cn") || loc.ends_with("-sg") { "zh_CN" } else { "zh_TW" };
    }
    let p = loc.split('-').next().unwrap_or("");
    LANGS.iter().map(|l| l.0).find(|c| *c == p).unwrap_or("en")
}

fn apply_lang(code: &str) {
    let c = if code.is_empty() { system_lang() } else { code };
    let _ = slint::select_bundled_translation(if c == "en" { "" } else { c });
}

fn state_dir() -> PathBuf {
    dlcore::ipc::default_state_dir()
}

/// `--register-browser` / `--unregister-browser`: native messaging host manifests.
fn browser_flags() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--unregister-browser") {
        dlcore::browser::unregister();
        println!("browser integration removed");
        return Some(0);
    }
    if args.iter().any(|a| a == "--register-browser") {
        let host = std::env::current_exe().ok().map(|p| p.with_file_name(dlcore::browser::host_exe_name()));
        let Some(host) = host.filter(|h| h.exists()) else {
            eprintln!("{} not found next to this program", dlcore::browser::host_exe_name());
            return Some(1);
        };
        return Some(match dlcore::browser::register(&host) {
            Ok(files) => {
                for f in files {
                    println!("registered {}", f.display());
                }
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        });
    }
    None
}

struct App {
    mgr: Option<Manager>,
    filter: i32,
    selected: Option<u64>,
    visible: Vec<u64>,
    detail_for: Option<u64>,
    last_clip: String,
    clipboard: Option<arboard::Clipboard>,
}

impl App {
    fn mgr(&self) -> &Manager {
        self.mgr.as_ref().unwrap()
    }
}

fn matches_filter(s: &Snapshot, f: i32) -> bool {
    match f {
        0 => true,
        1 => matches!(s.status, Status::Downloading | Status::Queued | Status::Verifying),
        2 => s.status == Status::Completed,
        c => s.category.index() == c - 3,
    }
}

fn kbs(bps: u64) -> String {
    (bps / 1024).to_string()
}
fn parse_kbs(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return Some(0);
    }
    s.parse::<f64>().ok().filter(|v| *v >= 0.0).map(|v| (v * 1024.0) as u64)
}

fn open_in_file_manager(path: &Path) {
    let dir = if path.is_dir() { path.to_path_buf() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
    #[cfg(windows)]
    let r = if path.is_file() {
        std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()
    } else {
        std::process::Command::new("explorer").arg(&dir).spawn()
    };
    #[cfg(target_os = "macos")]
    let r = if path.is_file() {
        std::process::Command::new("open").arg("-R").arg(path).spawn()
    } else {
        std::process::Command::new("open").arg(&dir).spawn()
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&dir).spawn();
    let _ = r;
}

fn verify_index(s: &Snapshot) -> i32 {
    use dlcore::VerifyState::*;
    match s.verification.as_ref().map(|v| v.state) {
        Some(Verifying) => 0,
        Some(Verified) => 1,
        Some(Failed) => 2,
        Some(NoChecksum) => 3,
        Some(Unverified) => 4,
        None => -1,
    }
}

fn refresh(ui: &AppWindow, app: &mut App) {
    let snaps = app.mgr().snapshots();
    let tr = ui.global::<Tr>();
    let mut counts = vec![snaps.len() as i32, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    for s in &snaps {
        if matches_filter(s, 1) {
            counts[1] += 1;
        }
        if s.status == Status::Completed {
            counts[2] += 1;
        }
        counts[3 + s.category.index() as usize] += 1;
    }
    ui.set_counts(ModelRc::new(VecModel::from(counts.clone())));

    let visible: Vec<&Snapshot> = snaps.iter().filter(|s| matches_filter(s, app.filter)).collect();
    if app.selected.is_some_and(|id| !snaps.iter().any(|s| s.id == id)) {
        app.selected = None;
    }
    app.visible = visible.iter().map(|s| s.id).collect();
    let rows: Vec<Row> = visible
        .iter()
        .map(|s| {
            let progress = match s.total {
                Some(n) if n > 0 => s.downloaded as f32 / n as f32,
                _ if s.status == Status::Completed => 1.0,
                _ => 0.0,
            };
            Row {
                id: s.id as i32,
                name: s.name.clone().into(),
                size: s.total.map(human_size).unwrap_or_else(|| if s.downloaded > 0 { human_size(s.downloaded) } else { "—".into() }).into(),
                progress,
                progress_text: if s.total.is_some() { format!("{:.0}%", progress * 100.0) } else { String::new() }.into(),
                speed: if s.status == Status::Downloading && s.speed > 0.0 { format!("{}/s", human_size(s.speed as u64)) } else { String::new() }.into(),
                eta: s.eta.map(human_eta).unwrap_or_default().into(),
                status: s.status.index(),
                category: s.category.index(),
                selected: Some(s.id) == app.selected,
                verify: verify_index(s),
                verify_progress: s.verify_progress,
            }
        })
        .collect();
    ui.set_rows(ModelRc::new(VecModel::from(rows)));

    let total_speed: f64 = snaps.iter().map(|s| s.speed).sum();
    ui.set_total_speed(format!("{}/s", human_size(total_speed as u64)).into());
    ui.set_status_line(tr.invoke_status_line(snaps.len() as i32, counts[1]));

    let sel = app.selected.and_then(|id| snaps.iter().find(|s| s.id == id));
    ui.set_has_selection(sel.is_some());
    ui.set_sel_can_start(sel.is_some_and(|s| matches!(s.status, Status::Paused | Status::Error)));
    ui.set_sel_can_pause(sel.is_some_and(|s| matches!(s.status, Status::Downloading | Status::Queued)));
    if let Some(s) = sel {
        ui.set_d_name(s.name.clone().into());
        ui.set_d_url(s.url.clone().into());
        ui.set_d_path(s.path.display().to_string().into());
        ui.set_d_size(
            match s.total {
                Some(n) => format!("{} / {}", human_size(s.downloaded), human_size(n)),
                None => format!("{} / {}", human_size(s.downloaded), tr.invoke_unknown()),
            }
            .into(),
        );
        ui.set_d_resume(tr.invoke_yes_no(s.resumable));
        ui.set_d_conns(format!("{} / {}", s.active_connections, s.connections).into());
        ui.set_d_error(s.error.clone().unwrap_or_default().into());
        let total = s.total.unwrap_or(0).max(1) as f32;
        let segs: Vec<SegBar> = if s.total.is_some() {
            s.segments
                .iter()
                .map(|&(a, p, e)| SegBar {
                    x: a as f32 / total,
                    w: (e.saturating_sub(a)) as f32 / total,
                    done: if e > a { (p - a) as f32 / (e - a) as f32 } else { 1.0 },
                })
                .collect()
        } else {
            vec![SegBar { x: 0.0, w: 1.0, done: if s.status == Status::Completed { 1.0 } else { 0.0 } }]
        };
        ui.set_d_segs(ModelRc::new(VecModel::from(segs)));
        let v = s.verification.clone().unwrap_or_default();
        ui.set_d_verify(verify_index(s));
        ui.set_d_verify_progress(s.verify_progress);
        ui.set_d_algo(v.algo.map(|a| a.name()).unwrap_or("").into());
        ui.set_d_expected(v.expected.into());
        ui.set_d_actual(v.actual.into());
        ui.set_d_source(v.source.into());
        ui.set_d_signature(v.signature.unwrap_or_default().into());
        ui.set_d_verify_note(v.note.into());
        if app.detail_for != Some(s.id) {
            app.detail_for = Some(s.id);
            ui.set_d_limit(kbs(s.speed_limit).into());
            ui.set_d_priority((s.priority.clamp(-1, 1) + 1) as i32);
        }
    } else {
        app.detail_for = None;
    }
}

fn set_languages_model(ui: &AppWindow) {
    let mut v: Vec<SharedString> = vec![ui.global::<Tr>().invoke_system_default()];
    v.extend(LANGS.iter().map(|l| SharedString::from(l.1)));
    ui.set_languages(ModelRc::new(VecModel::from(v)));
}

fn open_add(ui: &AppWindow, app: &App, url: &str) {
    ui.set_add_url(url.into());
    ui.set_add_folder("".into());
    ui.set_add_conns(app.mgr().settings().max_connections as i32);
    ui.set_add_checksum("".into());
    ui.set_add_start(true);
    ui.set_add_error("".into());
    ui.set_add_open(true);
}

/// On Windows prefer the Skia renderer: the software renderer's partial presentation
/// can leave regions of the window unpainted (desktop shows through).
fn select_renderer() {
    #[cfg(windows)]
    if std::env::var_os("SLINT_BACKEND").is_none() {
        let skia = slint::BackendSelector::new().backend_name("winit".into()).renderer_name("skia".into()).select();
        if skia.is_err() {
            let _ = slint::BackendSelector::new().backend_name("winit".into()).renderer_name("software".into()).select();
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    if let Some(code) = browser_flags() {
        std::process::exit(code);
    }
    // Single instance: if the app already runs, hand our URLs to it and quit.
    let cli_urls: Vec<String> = std::env::args().skip(1).filter(|a| looks_like_url(a)).collect();
    {
        use dlcore::ipc::{AddRequest, Command};
        let cmd = if cli_urls.is_empty() {
            Command::Show
        } else {
            Command::Add { downloads: cli_urls.iter().map(|u| AddRequest { url: u.clone(), ..Default::default() }).collect() }
        };
        if dlcore::ipc::send_to_app(&state_dir(), cmd).is_ok_and(|r| r.ok) {
            return Ok(());
        }
    }
    select_renderer();
    let mgr = Manager::new(&state_dir());
    let settings = mgr.settings();
    // URLs on the command line are queued right away (e.g. from a browser or script).
    for a in cli_urls {
        mgr.add(NewDownload { url: a, start: true, ..Default::default() });
    }
    // Local IPC for the browser extension's native host.
    let (ipc_tx, ipc_rx) = std::sync::mpsc::channel::<dlcore::ipc::Command>();
    {
        let tx = std::sync::Mutex::new(ipc_tx);
        let r = dlcore::ipc::serve(&mgr.runtime(), &state_dir(), move |c| {
            if !matches!(c, dlcore::ipc::Command::Ping) {
                let _ = tx.lock().unwrap().send(c);
            }
            dlcore::ipc::Reply::ok()
        });
        if let Err(e) = r {
            eprintln!("IPC server failed: {e}");
        }
    }

    let ui = AppWindow::new()?;
    apply_lang(&settings.language);
    set_languages_model(&ui);

    let mut clipboard = arboard::Clipboard::new().ok();
    let last_clip = clipboard.as_mut().and_then(|c| c.get_text().ok()).unwrap_or_default();
    let app = Rc::new(RefCell::new(App { mgr: Some(mgr), filter: 0, selected: None, visible: vec![], detail_for: None, last_clip, clipboard }));

    refresh(&ui, &mut app.borrow_mut());

    let timer = slint::Timer::default();
    {
        let (ui_w, app) = (ui.as_weak(), app.clone());
        let mut ticks = 0u32;
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(500), move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let mut a = app.borrow_mut();
            while let Ok(cmd) = ipc_rx.try_recv() {
                if let dlcore::ipc::Command::Add { downloads } = cmd {
                    for d in downloads {
                        let id = a.mgr().add(NewDownload {
                            url: d.url,
                            referer: d.referrer,
                            file_name: d.filename,
                            checksum_urls: d.checksum_urls,
                            checksum: d.checksum.as_deref().and_then(dlcore::Checksum::guess),
                            start: true,
                            ..Default::default()
                        });
                        a.selected = Some(id);
                    }
                }
                let _ = ui.show();
                ui.window().set_minimized(false);
            }
            refresh(&ui, &mut a);
            ticks += 1;
            if ticks % 2 == 0 && a.mgr().settings().watch_clipboard && !ui.get_add_open() && !ui.get_set_open() {
                let text = a.clipboard.as_mut().and_then(|c| c.get_text().ok()).unwrap_or_default();
                if text != a.last_clip {
                    a.last_clip = text.clone();
                    let t = text.trim();
                    if looks_like_url(t) && !a.mgr().snapshots().iter().any(|s| s.url == t) {
                        open_add(&ui, &a, t);
                    }
                }
            }
        });
    }

    macro_rules! on {
        ($ui:ident, $app:ident, $setter:ident, |$($arg:ident),*| $body:block) => {{
            let ui_w = $ui.as_weak();
            let app_c = $app.clone();
            $ui.$setter(move |$($arg),*| {
                #[allow(unused_variables)]
                let $ui = ui_w.upgrade().unwrap();
                #[allow(unused_mut)]
                let mut $app = app_c.borrow_mut();
                $body
            });
        }};
    }

    on!(ui, app, on_add_clicked, | | {
        open_add(&ui, &app, "");
    });
    on!(ui, app, on_add_ok, | | {
        let url = ui.get_add_url().trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) || !looks_like_url(&url) {
            ui.set_add_error(ui.global::<Tr>().invoke_invalid_url());
            return;
        }
        let sum = ui.get_add_checksum().trim().to_string();
        let checksum = if sum.is_empty() { None } else {
            match Checksum::guess(&sum) {
                Some(c) => Some(c),
                None => {
                    ui.set_add_error(ui.global::<Tr>().invoke_invalid_checksum());
                    return;
                }
            }
        };
        let folder = ui.get_add_folder().trim().to_string();
        let id = app.mgr().add(NewDownload {
            url,
            dir: (!folder.is_empty()).then(|| PathBuf::from(folder)),
            connections: Some(ui.get_add_conns().clamp(1, 32) as u32),
            checksum,
            start: ui.get_add_start(),
            ..Default::default()
        });
        app.selected = Some(id);
        ui.set_add_open(false);
        refresh(&ui, &mut app);
    });
    on!(ui, app, on_start_clicked, | | {
        if let Some(id) = app.selected { app.mgr().start(id); }
        refresh(&ui, &mut app);
    });
    on!(ui, app, on_pause_clicked, | | {
        if let Some(id) = app.selected { app.mgr().pause(id); }
        refresh(&ui, &mut app);
    });
    on!(ui, app, on_start_all, | | {
        app.mgr().start_all();
        refresh(&ui, &mut app);
    });
    on!(ui, app, on_pause_all, | | {
        app.mgr().pause_all();
        refresh(&ui, &mut app);
    });
    on!(ui, app, on_delete_confirmed, | | {
        if let Some(id) = app.selected.take() { app.mgr().remove(id, ui.get_del_file()); }
        refresh(&ui, &mut app);
    });
    on!(ui, app, on_filter_clicked, |f| {
        app.filter = f;
        ui.set_filter(f);
        refresh(&ui, &mut app);
    });
    on!(ui, app, on_row_clicked, |i| {
        app.selected = app.visible.get(i as usize).copied();
        refresh(&ui, &mut app);
    });
    on!(ui, app, on_row_double_clicked, |i| {
        if let Some(id) = app.visible.get(i as usize).copied() {
            if let Some(s) = app.mgr().snapshots().into_iter().find(|s| s.id == id) {
                open_in_file_manager(&s.path);
            }
        }
    });
    on!(ui, app, on_open_folder, | | {
        if let Some(id) = app.selected {
            if let Some(s) = app.mgr().snapshots().into_iter().find(|s| s.id == id) {
                open_in_file_manager(&s.path);
            }
        }
    });
    on!(ui, app, on_detail_apply, | | {
        if let Some(id) = app.selected {
            if let Some(v) = parse_kbs(&ui.get_d_limit()) { app.mgr().set_speed_limit(id, v); }
            app.mgr().set_priority(id, ui.get_d_priority() - 1);
            app.mgr().save();
        }
    });
    on!(ui, app, on_browse, |which| {
        let current = if which == 0 { ui.get_add_folder() } else { ui.get_s_folder() };
        let start = if current.is_empty() { app.mgr().settings().download_dir } else { PathBuf::from(current.as_str()) };
        match rfd::FileDialog::new().set_directory(&start).pick_folder() {
            Some(p) => p.display().to_string().into(),
            None => current,
        }
    });
    on!(ui, app, on_settings_clicked, | | {
        let s = app.mgr().settings();
        ui.set_s_language(LANGS.iter().position(|l| l.0 == s.language).map(|i| i as i32 + 1).unwrap_or(0));
        ui.set_s_folder(s.download_dir.display().to_string().into());
        ui.set_s_catfolders(s.category_folders);
        ui.set_s_concurrent(s.max_concurrent as i32);
        ui.set_s_conns(s.max_connections as i32);
        ui.set_s_global(kbs(s.global_limit).into());
        ui.set_s_perdl(kbs(s.per_download_limit).into());
        ui.set_s_retries(s.retries as i32);
        ui.set_s_proxy(s.proxy.clone().into());
        ui.set_s_sched(s.schedule.enabled);
        ui.set_s_start(s.schedule.start.clone().into());
        ui.set_s_stop(s.schedule.stop.clone().into());
        ui.set_s_clip(s.watch_clipboard);
        ui.set_s_verify(s.auto_verify);
        ui.set_s_error("".into());
        ui.set_set_open(true);
    });
    on!(ui, app, on_settings_ok, | | {
        let mut s = app.mgr().settings();
        let li = ui.get_s_language();
        s.language = if li <= 0 { String::new() } else { LANGS[(li - 1) as usize].0.to_string() };
        s.download_dir = PathBuf::from(ui.get_s_folder().trim());
        s.category_folders = ui.get_s_catfolders();
        s.max_concurrent = ui.get_s_concurrent().clamp(1, 20) as u32;
        s.max_connections = ui.get_s_conns().clamp(1, 32) as u32;
        s.retries = ui.get_s_retries().clamp(0, 50) as u32;
        s.proxy = ui.get_s_proxy().trim().to_string();
        s.schedule.enabled = ui.get_s_sched();
        s.schedule.start = ui.get_s_start().trim().to_string();
        s.schedule.stop = ui.get_s_stop().trim().to_string();
        s.watch_clipboard = ui.get_s_clip();
        s.auto_verify = ui.get_s_verify();
        let (g, p) = (parse_kbs(&ui.get_s_global()), parse_kbs(&ui.get_s_perdl()));
        let tr = ui.global::<Tr>();
        let (Some(g), Some(p)) = (g, p) else { ui.set_s_error(tr.invoke_invalid_number()); return; };
        if !s.schedule.valid() { ui.set_s_error(tr.invoke_invalid_number()); return; }
        s.global_limit = g;
        s.per_download_limit = p;
        let lang = s.language.clone();
        if let Err(e) = app.mgr().set_settings(s) { ui.set_s_error(e.into()); return; }
        apply_lang(&lang);
        set_languages_model(&ui);
        ui.set_set_open(false);
        refresh(&ui, &mut app);
    });

    ui.run()?;
    drop(timer);
    let _ = std::fs::remove_file(dlcore::ipc::endpoint_path(&state_dir()));
    if let Some(m) = app.borrow_mut().mgr.take() {
        m.shutdown();
    }
    Ok(())
}
