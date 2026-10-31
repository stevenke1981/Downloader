// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use dlcore::util::{human_eta, human_size, looks_like_url};
use dlcore::{Manager, NewDownload, Snapshot, Status};
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
mod window_placement;

slint::include_modules!();
mod actions;
mod batch;
mod browser_controller;
mod browser_setup;
mod desktop;
mod reveal;
mod tools;
mod tray;
mod view;

use actions::{reveal_download, show_notice};
#[cfg(test)]
use view::sync_model;
use view::{Models, kbs, open_compact, parse_kbs, refresh, refresh_now, sync_compact};

fn schedule_initial_placement(_ui: &AppWindow) {
    #[cfg(windows)]
    {
        // Slint creates the native window after entering the event loop. Waiting
        // for that window avoids silently skipping placement before HWND exists.
        let weak = _ui.as_weak();
        if let Err(error) = slint::spawn_local(async move {
            if let Some(ui) = weak.upgrade() {
                match ui.window().winit_window().await {
                    Ok(native) => {
                        drop(native);
                        window_placement::fit_initial_window(ui.window());
                    }
                    Err(error) => eprintln!("Cannot position initial window: {error}"),
                }
            }
        }) {
            eprintln!("Cannot schedule initial window placement: {error}");
        }
    }
}

fn restore_main(ui: &AppWindow, compact: &CompactWindow) {
    if ui.show().is_ok() {
        ui.window().set_minimized(false);
        ui.window().with_winit_window(|w| w.focus_window());
        let _ = compact.hide();
    }
}

fn show_compact(ui: &AppWindow, compact: &CompactWindow) {
    if ui.get_exit_open() {
        restore_main(ui, compact);
        return;
    }
    if compact.show().is_ok() {
        compact.window().set_minimized(false);
        compact.window().with_winit_window(|w| w.focus_window());
        let _ = ui.hide();
    }
}

fn active(status: Status) -> bool {
    matches!(
        status,
        Status::Queued | Status::Downloading | Status::Verifying
    )
}

fn minimize_to_tray(ui: &AppWindow, compact: &CompactWindow, available: bool) {
    if ui.get_exit_open() {
        restore_main(ui, compact);
        return;
    }
    if available {
        let _ = compact.hide();
        let _ = ui.hide();
    } else {
        restore_main(ui, compact);
        show_notice(ui, ui.global::<Tr>().invoke_tray_unavailable(), true);
    }
}

fn request_exit(ui: &AppWindow, compact: &CompactWindow, app: &Rc<RefCell<App>>) {
    restore_main(ui, compact);
    let busy = app
        .borrow()
        .mgr()
        .snapshots()
        .iter()
        .any(|s| active(s.status));
    if busy {
        ui.set_exit_open(true);
        return;
    }
    let _ = slint::quit_event_loop();
}

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
    let loc = sys_locale::get_locale()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace('_', "-");
    if loc.starts_with("zh") {
        return if loc.contains("hans") || loc.ends_with("-cn") || loc.ends_with("-sg") {
            "zh_CN"
        } else {
            "zh_TW"
        };
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
        return Some(match dlcore::browser::unregister() {
            Ok(()) => {
                println!("browser integration removed");
                0
            }
            Err(error) => {
                eprintln!("{error}");
                1
            }
        });
    }
    if args.iter().any(|a| a == "--register-browser") {
        let host = std::env::current_exe()
            .ok()
            .map(|p| p.with_file_name(dlcore::browser::host_exe_name()));
        let Some(host) = host.filter(|h| h.exists()) else {
            eprintln!(
                "{} not found next to this program",
                dlcore::browser::host_exe_name()
            );
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
    /// Lowercase, trimmed list search; empty shows every download.
    search: String,
    detail_for: Option<u64>,
    last_clip: String,
    clipboard: Option<arboard::Clipboard>,
    models: Models,
}

impl App {
    fn mgr(&self) -> &Manager {
        self.mgr.as_ref().unwrap()
    }
}

fn set_languages_model(ui: &AppWindow) {
    let mut v: Vec<SharedString> = vec![ui.global::<Tr>().invoke_system_default()];
    v.extend(LANGS.iter().map(|l| SharedString::from(l.1)));
    ui.set_languages(ModelRc::new(VecModel::from(v)));
}

fn open_add(ui: &AppWindow, url: &str) {
    ui.set_add_url(url.into());
    ui.set_add_folder("".into());
    ui.set_add_conns(0);
    ui.set_add_checksum("".into());
    ui.set_add_start(true);
    ui.set_add_kind(0);
    ui.set_add_error("".into());
    ui.set_add_open(true);
}

/// On Windows prefer the Skia renderer: the software renderer's partial presentation
/// can leave regions of the window unpainted (desktop shows through).
fn select_renderer() {
    #[cfg(windows)]
    if std::env::var_os("SLINT_BACKEND").is_none() {
        let skia = slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("skia".into())
            .select();
        if skia.is_err() {
            let _ = slint::BackendSelector::new()
                .backend_name("winit".into())
                .renderer_name("software".into())
                .select();
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    if let Some(code) = browser_flags() {
        std::process::exit(code);
    }
    // Single instance: if the app already runs, hand our URLs to it and quit.
    let cli_urls: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| looks_like_url(a))
        .collect();
    {
        use dlcore::ipc::{AddRequest, Command};
        let cmd = if cli_urls.is_empty() {
            Command::Show
        } else {
            Command::Add {
                downloads: cli_urls
                    .iter()
                    .map(|u| AddRequest {
                        url: u.clone(),
                        ..Default::default()
                    })
                    .collect(),
            }
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
        mgr.add(NewDownload {
            url: a,
            start: true,
            ..Default::default()
        });
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
    let compact = CompactWindow::new()?;
    let tray = Rc::new(tray::Tray::new().ok());
    ui.set_tray_available(tray.as_ref().as_ref().is_some_and(|t| t.available()));
    ui.on_exit_confirmed(|| {
        let _ = slint::quit_event_loop();
    });

    let mut clipboard = arboard::Clipboard::new().ok();
    let last_clip = clipboard
        .as_mut()
        .and_then(|c| c.get_text().ok())
        .unwrap_or_default();
    let app = Rc::new(RefCell::new(App {
        mgr: Some(mgr),
        filter: 0,
        selected: None,
        visible: vec![],
        search: String::new(),
        detail_for: None,
        last_clip,
        clipboard,
        models: Models::new(),
    }));
    app.borrow().models.bind(&ui, &compact);
    browser_controller::bind(&ui, &app);
    let desktop = desktop::bind(&ui, &compact, &app)?;
    let tools = tools::bind(&ui, &app)?;
    actions::bind(&ui, &app);

    refresh_now(&ui, &mut app.borrow_mut());

    {
        let (uw, cw, a) = (ui.as_weak(), compact.as_weak(), app.clone());
        ui.on_compact_clicked(move || {
            if let (Some(u), Some(c)) = (uw.upgrade(), cw.upgrade()) {
                open_compact(&u, &c, &a);
            }
        });
        let (uw, cw) = (ui.as_weak(), compact.as_weak());
        compact.on_restore(move || {
            if let (Some(u), Some(c)) = (uw.upgrade(), cw.upgrade()) {
                restore_main(&u, &c);
            }
        });
        let (uw, cw) = (ui.as_weak(), compact.as_weak());
        compact.window().on_close_requested(move || {
            if let (Some(u), Some(c)) = (uw.upgrade(), cw.upgrade()) {
                restore_main(&u, &c);
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
        let (uw, cw, t) = (ui.as_weak(), compact.as_weak(), tray.clone());
        ui.on_tray_clicked(move || {
            if let (Some(u), Some(c)) = (uw.upgrade(), cw.upgrade()) {
                minimize_to_tray(&u, &c, t.as_ref().as_ref().is_some_and(|t| t.available()));
            }
        });
        let (uw, cw, a) = (ui.as_weak(), compact.as_weak(), app.clone());
        ui.on_exit_clicked(move || {
            if let (Some(u), Some(c)) = (uw.upgrade(), cw.upgrade()) {
                request_exit(&u, &c, &a);
            }
        });
        let (uw, cw, a) = (ui.as_weak(), compact.as_weak(), app.clone());
        ui.window().on_close_requested(move || {
            if let (Some(u), Some(c)) = (uw.upgrade(), cw.upgrade()) {
                request_exit(&u, &c, &a);
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }

    let timer = slint::Timer::default();
    {
        let (ui_w, app) = (ui.as_weak(), app.clone());
        let (compact_w, tray) = (compact.as_weak(), tray.clone());
        let mut ticks = 0u32;
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(500),
            move || {
                let Some(ui) = ui_w.upgrade() else { return };
                let Some(compact) = compact_w.upgrade() else {
                    return;
                };
                let available = tray.as_ref().as_ref().is_some_and(|t| t.available());
                ui.set_tray_available(available);
                if !available && !ui.window().is_visible() && !compact.window().is_visible() {
                    minimize_to_tray(&ui, &compact, false);
                }
                if let Some(t) = tray.as_ref() {
                    for action in t.actions() {
                        match action {
                            tray::Action::Restore => restore_main(&ui, &compact),
                            tray::Action::Compact => open_compact(&ui, &compact, &app),
                            tray::Action::Exit => request_exit(&ui, &compact, &app),
                        }
                    }
                }
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
                                cookie: d.cookie,
                                start: true,
                                ..Default::default()
                            });
                            a.selected = Some(id);
                        }
                    }
                    restore_main(&ui, &compact);
                }
                // One snapshot per tick, shared by every view and the clipboard check.
                let snaps = a.mgr().snapshots();
                refresh(&ui, &mut a, &snaps);
                if compact.window().is_visible() {
                    sync_compact(&ui, &compact, &a, &snaps);
                }
                if let Some(t) = tray.as_ref() {
                    t.update(&format!(
                        "{} · {}",
                        ui.get_status_line(),
                        ui.get_total_speed()
                    ));
                }
                ticks += 1;
                if ticks.is_multiple_of(2)
                    && a.mgr().settings().watch_clipboard
                    && !ui.get_add_open()
                    && !ui.get_set_open()
                {
                    let text = a
                        .clipboard
                        .as_mut()
                        .and_then(|c| c.get_text().ok())
                        .unwrap_or_default();
                    if text != a.last_clip {
                        a.last_clip = text.clone();
                        let fresh = text.lines().map(str::trim).any(|line| {
                            [
                                "https://", "http://", "ftp://", "magnet:", "rtsp://", "mms://",
                                "mmsh://", "mmst://",
                            ]
                            .iter()
                            .any(|scheme| line.starts_with(scheme))
                                && !snaps.iter().any(|s| s.url == line)
                        });
                        if fresh {
                            open_add(&ui, text.trim());
                        }
                    }
                }
            },
        );
    }

    macro_rules! on {
        ($ui:ident, $app:ident, $setter:ident, || $body:block) => {{
            let ui_w = $ui.as_weak();
            let app_c = $app.clone();
            $ui.$setter(move || {
                #[allow(unused_variables)]
                let $ui = ui_w.upgrade().unwrap();
                #[allow(unused_mut)]
                let mut $app = app_c.borrow_mut();
                $body
            });
        }};
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

    on!(ui, app, on_start_clicked, || {
        if let Some(id) = app.selected {
            app.mgr().start(id);
        }
        refresh_now(&ui, &mut app);
    });
    on!(ui, app, on_pause_clicked, || {
        if let Some(id) = app.selected {
            app.mgr().pause(id);
        }
        refresh_now(&ui, &mut app);
    });
    on!(ui, app, on_start_all, || {
        app.mgr().start_all();
        refresh_now(&ui, &mut app);
    });
    on!(ui, app, on_pause_all, || {
        app.mgr().pause_all();
        refresh_now(&ui, &mut app);
    });
    on!(ui, app, on_delete_confirmed, || {
        if let Some(id) = app.selected.take() {
            app.mgr().remove(id, ui.get_del_file());
        }
        refresh_now(&ui, &mut app);
    });
    on!(ui, app, on_filter_clicked, |f| {
        app.filter = f;
        ui.set_filter(f);
        refresh_now(&ui, &mut app);
    });
    on!(ui, app, on_row_clicked, |i| {
        app.selected = app.visible.get(i as usize).copied();
        refresh_now(&ui, &mut app);
    });
    on!(ui, app, on_row_double_clicked, |i| {
        let Some(id) = app.visible.get(i as usize).copied() else {
            return;
        };
        app.selected = Some(id);
        refresh_now(&ui, &mut app);
        if app
            .mgr()
            .snapshots()
            .iter()
            .any(|s| s.id == id && s.status == Status::Completed)
        {
            reveal_download(&ui, &app, id);
        }
    });
    on!(ui, app, on_open_folder, || {
        if let Some(id) = app.selected {
            reveal_download(&ui, &app, id);
        }
    });
    on!(ui, app, on_detail_apply, || {
        if let Some(id) = app.selected {
            let Some(v) = parse_kbs(&ui.get_d_limit()) else {
                show_notice(&ui, ui.global::<Tr>().invoke_invalid_number(), true);
                return;
            };
            app.mgr().set_speed_limit(id, v);
            app.mgr().set_priority(id, ui.get_d_priority() - 1);
            app.mgr().save();
        }
    });
    on!(ui, app, on_browse, |which| {
        let current = if which == 0 {
            ui.get_add_folder()
        } else {
            ui.get_s_folder()
        };
        let start = if current.is_empty() {
            app.mgr().settings().download_dir
        } else {
            PathBuf::from(current.as_str())
        };
        match rfd::FileDialog::new().set_directory(&start).pick_folder() {
            Some(p) => p.display().to_string().into(),
            None => current,
        }
    });
    on!(ui, app, on_settings_clicked, || {
        let s = app.mgr().settings();
        ui.set_s_language(
            LANGS
                .iter()
                .position(|l| l.0 == s.language)
                .map(|i| i as i32 + 1)
                .unwrap_or(0),
        );
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
        ui.invoke_browser_refresh();
    });
    on!(ui, app, on_settings_ok, || {
        let mut s = app.mgr().settings();
        let li = ui.get_s_language();
        s.language = if li <= 0 {
            String::new()
        } else {
            LANGS[(li - 1) as usize].0.to_string()
        };
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
        let folder = ui.get_s_folder().trim().to_string();
        let (g, p) = (parse_kbs(&ui.get_s_global()), parse_kbs(&ui.get_s_perdl()));
        let tr = ui.global::<Tr>();
        if folder.is_empty() {
            ui.set_s_error(tr.invoke_invalid_folder());
            return;
        }
        s.download_dir = PathBuf::from(folder);
        let (Some(g), Some(p)) = (g, p) else {
            ui.set_s_error(tr.invoke_invalid_number());
            return;
        };
        if !s.schedule.valid() {
            ui.set_s_error(tr.invoke_invalid_number());
            return;
        }
        s.global_limit = g;
        s.per_download_limit = p;
        let lang = s.language.clone();
        if let Err(e) = app.mgr().set_settings(s) {
            ui.set_s_error(e.into());
            return;
        }
        apply_lang(&lang);
        set_languages_model(&ui);
        ui.set_set_open(false);
        refresh_now(&ui, &mut app);
    });

    ui.show()?;
    schedule_initial_placement(&ui);
    let result = slint::run_event_loop_until_quit();
    drop(timer);
    drop(desktop);
    drop(tools);
    let _ = std::fs::remove_file(dlcore::ipc::endpoint_path(&state_dir()));
    if let Some(m) = app.borrow_mut().mgr.take() {
        m.shutdown();
    }
    result
}

#[cfg(test)]
#[path = "../../core/tests/server/mod.rs"]
#[rustfmt::skip]
mod fixture_server;

#[cfg(test)]
#[path = "tests/model_sync.rs"]
mod model_sync_tests;

#[cfg(test)]
#[path = "tests/lifecycle.rs"]
mod lifecycle_tests;
