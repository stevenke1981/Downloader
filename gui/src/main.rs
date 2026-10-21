// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use dlcore::util::{human_eta, human_size, looks_like_url};
use dlcore::{Checksum, Manager, NewDownload, Snapshot, Status};
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
mod window_placement;

slint::include_modules!();
mod browser_controller;
mod browser_setup;
mod reveal;
mod tray;

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
        ui.set_notice(ui.global::<Tr>().invoke_tray_unavailable());
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

fn reveal_download(ui: &AppWindow, app: &App, id: u64) {
    if ui.get_reveal_busy() {
        return;
    }
    if let Some(s) = app.mgr().snapshots().into_iter().find(|s| s.id == id) {
        ui.set_reveal_busy(true);
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let result = reveal::reveal(s.status, &s.path);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let tr = ui.global::<Tr>();
                let notice = match result {
                    Ok(()) => SharedString::default(),
                    Err(reveal::RevealError::NotCompleted) => tr.invoke_unfinished(),
                    Err(reveal::RevealError::Missing | reveal::RevealError::NotFile) => {
                        tr.invoke_missing_file()
                    }
                    Err(reveal::RevealError::Io(e)) => {
                        format!("{} {e}", tr.invoke_reveal_failed()).into()
                    }
                };
                ui.set_notice(notice);
                ui.set_reveal_busy(false);
            });
        });
    }
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

/// List models that live as long as the windows. They are updated in place so
/// ListView rows are only re-created when the list actually changes.
struct Models {
    rows: Rc<VecModel<Row>>,
    compact_rows: Rc<VecModel<Row>>,
    counts: Rc<VecModel<i32>>,
}

impl Models {
    fn new() -> Self {
        Self {
            rows: Rc::new(VecModel::from(Vec::new())),
            compact_rows: Rc::new(VecModel::from(Vec::new())),
            counts: Rc::new(VecModel::from(vec![0; 10])),
        }
    }
}

fn bind_models(ui: &AppWindow, compact: &CompactWindow, models: &Models) {
    ui.set_rows(ModelRc::from(models.rows.clone()));
    ui.set_counts(ModelRc::from(models.counts.clone()));
    compact.set_rows(ModelRc::from(models.compact_rows.clone()));
}

/// Makes `model` equal to `items`. Changed rows are replaced in place, surplus rows
/// are removed from the tail and new rows are appended, so unchanged rows are not touched.
fn sync_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, items: Vec<T>) {
    let shared = model.row_count().min(items.len());
    for (i, item) in items.iter().enumerate().take(shared) {
        if model.row_data(i).as_ref() != Some(item) {
            model.set_row_data(i, item.clone());
        }
    }
    while model.row_count() > items.len() {
        model.remove(model.row_count() - 1);
    }
    for item in items.into_iter().skip(shared) {
        model.push(item);
    }
}

/// Copies the compact window's summary and rows from the current UI state and one snapshot.
fn sync_compact(ui: &AppWindow, compact: &CompactWindow, app: &App, snaps: &[Snapshot]) {
    compact.set_total_speed(ui.get_total_speed());
    compact.set_status_line(ui.get_status_line());
    sync_model(
        &app.models.compact_rows,
        rows_for(snaps.iter(), app.selected),
    );
}

/// Shows the compact window with rows taken now, not left stale until the next timer tick.
fn open_compact(ui: &AppWindow, compact: &CompactWindow, app: &RefCell<App>) {
    show_compact(ui, compact);
    let mut a = app.borrow_mut();
    let snaps = a.mgr().snapshots();
    refresh(ui, &mut a, &snaps);
    sync_compact(ui, compact, &a, &snaps);
}

struct App {
    mgr: Option<Manager>,
    filter: i32,
    selected: Option<u64>,
    visible: Vec<u64>,
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

fn matches_filter(s: &Snapshot, f: i32) -> bool {
    match f {
        0 => true,
        1 => matches!(
            s.status,
            Status::Downloading | Status::Queued | Status::Verifying
        ),
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
    s.parse::<f64>()
        .ok()
        .filter(|v| *v >= 0.0)
        .map(|v| (v * 1024.0) as u64)
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

fn rows_for<'a>(snaps: impl IntoIterator<Item = &'a Snapshot>, selected: Option<u64>) -> Vec<Row> {
    snaps
        .into_iter()
        .map(|s| {
            let progress = match s.total {
                Some(n) if n > 0 => s.downloaded as f32 / n as f32,
                _ if s.status == Status::Completed => 1.0,
                _ => 0.0,
            };
            Row {
                id: s.id as i32,
                name: s.name.clone().into(),
                size: s
                    .total
                    .map(human_size)
                    .unwrap_or_else(|| {
                        if s.downloaded > 0 {
                            human_size(s.downloaded)
                        } else {
                            "—".into()
                        }
                    })
                    .into(),
                progress,
                progress_text: if s.total.is_some() {
                    format!("{:.0}%", progress * 100.0)
                } else {
                    String::new()
                }
                .into(),
                speed: if s.status == Status::Downloading && s.speed > 0.0 {
                    format!("{}/s", human_size(s.speed as u64))
                } else {
                    String::new()
                }
                .into(),
                eta: s.eta.map(human_eta).unwrap_or_default().into(),
                status: s.status.index(),
                category: s.category.index(),
                selected: Some(s.id) == selected,
                verify: verify_index(s),
                verify_progress: s.verify_progress,
            }
        })
        .collect()
}

/// Refreshes every view from one snapshot taken by the caller.
fn refresh(ui: &AppWindow, app: &mut App, snaps: &[Snapshot]) {
    let tr = ui.global::<Tr>();
    let mut counts = vec![snaps.len() as i32, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    for s in snaps {
        if matches_filter(s, 1) {
            counts[1] += 1;
        }
        if s.status == Status::Completed {
            counts[2] += 1;
        }
        counts[3 + s.category.index() as usize] += 1;
    }
    sync_model(&app.models.counts, counts.clone());

    let visible: Vec<&Snapshot> = snaps
        .iter()
        .filter(|s| matches_filter(s, app.filter))
        .collect();
    if app
        .selected
        .is_some_and(|id| !snaps.iter().any(|s| s.id == id))
    {
        app.selected = None;
    }
    app.visible = visible.iter().map(|s| s.id).collect();
    sync_model(
        &app.models.rows,
        rows_for(visible.iter().copied(), app.selected),
    );

    let total_speed: f64 = snaps.iter().map(|s| s.speed).sum();
    ui.set_total_speed(format!("{}/s", human_size(total_speed as u64)).into());
    ui.set_status_line(tr.invoke_status_line(snaps.len() as i32, counts[1]));

    let sel = app
        .selected
        .and_then(|id| snaps.iter().find(|s| s.id == id));
    ui.set_has_selection(sel.is_some());
    ui.set_sel_can_start(sel.is_some_and(|s| matches!(s.status, Status::Paused | Status::Error)));
    ui.set_sel_can_pause(
        sel.is_some_and(|s| matches!(s.status, Status::Downloading | Status::Queued)),
    );
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
                    done: if e > a {
                        (p - a) as f32 / (e - a) as f32
                    } else {
                        1.0
                    },
                })
                .collect()
        } else {
            vec![SegBar {
                x: 0.0,
                w: 1.0,
                done: if s.status == Status::Completed {
                    1.0
                } else {
                    0.0
                },
            }]
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
            ui.set_d_priority(s.priority.clamp(-1, 1) + 1);
        }
    } else {
        app.detail_for = None;
    }
}

/// For callbacks that change state and do not already hold a snapshot.
fn refresh_now(ui: &AppWindow, app: &mut App) {
    let snaps = app.mgr().snapshots();
    refresh(ui, app, &snaps);
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
        detail_for: None,
        last_clip,
        clipboard,
        models: Models::new(),
    }));
    bind_models(&ui, &compact, &app.borrow().models);
    browser_controller::bind(&ui, &app);

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
                        let t = text.trim();
                        if looks_like_url(t) && !snaps.iter().any(|s| s.url == t) {
                            open_add(&ui, &a, t);
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

    on!(ui, app, on_add_clicked, || {
        open_add(&ui, &app, "");
    });
    on!(ui, app, on_add_ok, || {
        let url = ui.get_add_url().trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) || !looks_like_url(&url) {
            ui.set_add_error(ui.global::<Tr>().invoke_invalid_url());
            return;
        }
        let sum = ui.get_add_checksum().trim().to_string();
        let checksum = if sum.is_empty() {
            None
        } else {
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
        refresh_now(&ui, &mut app);
    });
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
                ui.set_notice(ui.global::<Tr>().invoke_invalid_number());
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
mod model_sync_tests {
    use super::*;

    #[test]
    fn sync_model_matches_items_for_growth_change_and_shrink() {
        let model = VecModel::from(vec![1, 2, 3]);
        sync_model(&model, vec![1, 9, 3, 4]);
        assert_eq!(model.iter().collect::<Vec<_>>(), vec![1, 9, 3, 4]);
        sync_model(&model, vec![7]);
        assert_eq!(model.iter().collect::<Vec<_>>(), vec![7]);
        sync_model(&model, Vec::new());
        assert_eq!(model.row_count(), 0);
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    // Runs only when explicitly selected for native screenshots.
    // It renders the real AppWindow with synthetic data, without a Manager,
    // clipboard, private history, download, or simulated native mouse input.
    #[test]
    #[ignore = "native rendering fixture; set DOWNLOADER_UI_SNAPSHOT for a local PPM"]
    fn render_layout_fixture() {
        let snapshot_path = std::env::var_os("DOWNLOADER_UI_SNAPSHOT");
        assert!(
            snapshot_path.is_some() || std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true"),
            "set DOWNLOADER_UI_SNAPSHOT for an explicit local native screenshot"
        );
        let scene = std::env::var("DOWNLOADER_UI_FIXTURE").unwrap();
        assert!(matches!(
            scene.as_str(),
            "details"
                | "settings"
                | "details-scrolled"
                | "settings-scrolled"
                | "settings-browser"
                | "settings-downloads"
                | "settings-general"
                | "empty"
        ));
        select_renderer();
        let ui = AppWindow::new().unwrap();
        apply_lang("zh_TW");
        set_languages_model(&ui);
        ui.set_tray_available(true);
        ui.set_has_selection(scene != "empty");
        ui.set_d_name("自製測試資料 fixture with spaces.txt".into());
        ui.set_d_path("C:\\fixture\\自製測試資料 fixture with spaces.txt".into());
        ui.set_d_url("http://127.0.0.1/fixture.txt".into());
        ui.set_d_size("256 KiB".into());
        ui.set_d_conns("4".into());
        ui.set_d_resume("Yes".into());
        ui.set_d_verify(1);
        ui.set_d_algo("SHA-256".into());
        ui.set_d_expected("0123456789abcdef".repeat(4).into());
        ui.set_d_actual("0123456789abcdef".repeat(4).into());
        ui.set_d_limit("0".into());
        ui.set_total_speed("0 B/s".into());
        ui.set_status_line(if scene == "empty" {
            "0 個下載，0 個進行中（自製測試資料）".into()
        } else {
            "1 個下載，0 個進行中（自製測試資料）".into()
        });
        let rows = if scene == "empty" {
            vec![]
        } else {
            vec![Row {
                name: "自製測試資料 fixture with spaces.txt".into(),
                size: "256 KiB".into(),
                progress: 1.0,
                progress_text: "100%".into(),
                status: 4,
                selected: true,
                verify: 1,
                ..Default::default()
            }]
        };
        ui.set_counts(ModelRc::new(VecModel::from(vec![rows.len() as i32; 10])));
        ui.set_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_set_open(scene.starts_with("settings"));
        ui.set_s_tab(match scene.as_str() {
            "settings-browser" => 2,
            "settings-downloads" | "settings-scrolled" => 1,
            _ => 0,
        });
        ui.set_s_folder("C:\\fixture\\下載資料夾".into());
        ui.set_browser_status(ui.global::<Tr>().invoke_browser_host_ready());
        ui.set_browser_folder("C:\\fixture\\Downloader\\extension".into());
        ui.set_browser_message(ui.global::<Tr>().invoke_browser_path_copied());
        ui.window().set_size(slint::LogicalSize::new(760.0, 480.0));
        ui.show().unwrap();
        schedule_initial_placement(&ui);
        if scene.ends_with("-scrolled") {
            let weak = ui.as_weak();
            let position = if scene.starts_with("settings") {
                slint::LogicalPosition::new(673.0, 240.0)
            } else {
                slint::LogicalPosition::new(753.0, 358.0)
            };
            slint::Timer::single_shot(Duration::from_millis(500), move || {
                if let Some(ui) = weak.upgrade() {
                    // Internal widget event, not OS/native mouse automation.
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                            position,
                            delta_x: 0.0,
                            delta_y: -2000.0,
                        });
                }
            });
        }
        if let Some(path) = snapshot_path {
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(800), move || {
                use std::io::Write;
                let ui = weak.upgrade().expect("fixture window is alive");
                let snapshot = ui.window().take_snapshot().expect("native window snapshot");
                let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
                write!(
                    file,
                    "P6\n{} {}\n255\n",
                    snapshot.width(),
                    snapshot.height()
                )
                .unwrap();
                for pixel in snapshot.as_slice() {
                    file.write_all(&[pixel.r, pixel.g, pixel.b]).unwrap();
                }
                file.flush().unwrap();
                let _ = slint::quit_event_loop();
            });
        }
        // Bound the fixture lifetime even if the screenshot helper fails.
        slint::Timer::single_shot(Duration::from_secs(30), || {
            let _ = slint::quit_event_loop();
        });
        slint::run_event_loop_until_quit().unwrap();
    }

    #[test]
    fn important_controls_remain_inside_small_client_sizes() {
        use i_slint_backend_testing::ElementHandle;
        use std::cell::Cell;
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        apply_lang("en");
        ui.set_tray_available(true);
        ui.set_has_selection(true);
        ui.set_d_name("A long fixture filename with spaces and Unicode 測試.txt".into());
        let compact_calls = Rc::new(Cell::new(0));
        let calls = compact_calls.clone();
        ui.on_compact_clicked(move || calls.set(calls.get() + 1));
        ui.show().unwrap();
        for (width, height) in [(760.0, 480.0), (960.0, 640.0), (1280.0, 800.0)] {
            ui.window().set_size(slint::LogicalSize::new(width, height));
            for label in ["Compact window", "Minimize to tray", "Exit", "Open folder"] {
                let control = ElementHandle::find_by_accessible_label(&ui, label)
                    .next()
                    .unwrap_or_else(|| panic!("missing control {label}"));
                let p = control.absolute_position();
                let s = control.size();
                assert!(s.width > 0.0 && s.height > 0.0, "empty control {label}");
                assert!(
                    p.x >= 0.0
                        && p.y >= 0.0
                        && p.x + s.width <= width + 0.5
                        && p.y + s.height <= height + 0.5,
                    "{label} at {p:?} size {s:?} outside {width}x{height}"
                );
                if label == "Open folder" {
                    // The details pane takes roughly the lower 40% (at most 300px).
                    let pane = height - p.y;
                    assert!(
                        pane > 120.0 && pane < 330.0,
                        "details pane height ~{pane} at {width}x{height}"
                    );
                }
            }
            ElementHandle::find_by_accessible_label(&ui, "Compact window")
                .next()
                .unwrap()
                .mock_single_click(slint::platform::PointerEventButton::Left);
        }
        assert_eq!(compact_calls.get(), 3);
    }

    #[test]
    fn small_window_scrolls_details_and_pins_settings_actions() {
        use i_slint_backend_testing::ElementHandle;
        use std::cell::Cell;
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        apply_lang("en");
        ui.set_has_selection(true);
        ui.set_d_verify(1);
        ui.set_d_expected("a".repeat(64).into());
        ui.set_d_actual("a".repeat(64).into());
        let applied = Rc::new(Cell::new(false));
        let observed = applied.clone();
        ui.on_detail_apply(move || observed.set(true));
        ui.show().unwrap();
        ui.window().set_size(slint::LogicalSize::new(760.0, 480.0));
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(753.0, 358.0),
                delta_x: 0.0,
                delta_y: -2000.0,
            });
        let apply = ElementHandle::find_by_accessible_label(&ui, "Apply")
            .next()
            .unwrap();
        let p = apply.absolute_position();
        let s = apply.size();
        assert!(
            p.x >= 197.0 && p.y >= 262.0 && p.x + s.width <= 760.5 && p.y + s.height <= 454.5,
            "Apply must be fully visible in the details viewport: {p:?}, {s:?}"
        );
        apply.mock_single_click(slint::platform::PointerEventButton::Left);
        assert!(
            applied.get(),
            "detail Apply must be reachable after scrolling"
        );

        ui.set_set_open(true);
        for tab in 0..3 {
            ui.set_s_tab(tab);
            let mut footer_y = None;
            for scroll in [0.0, -2000.0] {
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(673.0, 240.0),
                        delta_x: 0.0,
                        delta_y: scroll,
                    });
                for label in ["Cancel", "Save"] {
                    let control = ElementHandle::find_by_accessible_label(&ui, label)
                        .next()
                        .unwrap();
                    let p = control.absolute_position();
                    let s = control.size();
                    assert!(
                        p.x >= 28.0
                            && p.y >= 16.0
                            && p.x + s.width <= 732.5
                            && p.y + s.height <= 464.5,
                        "{label} must stay visible in tab {tab}: {p:?}, {s:?}"
                    );
                    if label == "Save" {
                        assert_eq!(
                            *footer_y.get_or_insert(p.y),
                            p.y,
                            "footer moved while scrolling"
                        );
                    }
                }
            }
        }
        let cancel = ElementHandle::find_by_accessible_label(&ui, "Cancel")
            .next()
            .unwrap();
        cancel.mock_single_click(slint::platform::PointerEventButton::Left);
        assert!(
            !ui.get_set_open(),
            "settings Cancel must remain reachable on every tab"
        );
    }

    fn press_key(ui: &AppWindow, key: slint::platform::Key) {
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
    }

    #[test]
    fn cancelling_exit_restores_keyboard_navigation_to_open_settings() {
        use i_slint_backend_testing::ElementHandle;
        use std::cell::Cell;
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        apply_lang("en");
        let background_calls = Rc::new(Cell::new(0));
        let calls = background_calls.clone();
        ui.on_compact_clicked(move || calls.set(calls.get() + 1));
        ui.show().unwrap();
        ui.set_set_open(true);
        ElementHandle::find_by_accessible_label(&ui, "General")
            .next()
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        ui.set_exit_open(true);
        for key in [
            slint::platform::Key::Escape,
            slint::platform::Key::Tab,
            slint::platform::Key::Return,
        ] {
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key.into() });
        }
        assert!(!ui.get_exit_open());
        assert!(ui.get_set_open());
        assert_eq!(
            background_calls.get(),
            0,
            "Settings must keep keyboard focus"
        );
        assert_eq!(
            ui.get_s_tab(),
            1,
            "Tab/Enter must activate Downloads in Settings"
        );
    }

    #[test]
    fn settings_tabs_support_keyboard_and_report_real_browser_actions() {
        use i_slint_backend_testing::ElementHandle;
        use std::cell::Cell;
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        apply_lang("en");
        let registered = Rc::new(Cell::new(0));
        let calls = registered.clone();
        ui.on_browser_register(move || calls.set(calls.get() + 1));
        let refreshed = Rc::new(Cell::new(0));
        let calls = refreshed.clone();
        ui.on_browser_refresh(move || calls.set(calls.get() + 1));
        let saved = Rc::new(Cell::new(0));
        let calls = saved.clone();
        ui.on_settings_ok(move || calls.set(calls.get() + 1));
        ui.show().unwrap();
        ui.window().set_size(slint::LogicalSize::new(760.0, 480.0));
        ui.set_set_open(true);
        assert_eq!(ui.get_s_tab(), 0);
        // Shift+Tab from the first tab wraps to the pinned Save action, not the main window.
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Shift.into(),
            });
        press_key(&ui, slint::platform::Key::Tab);
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::Shift.into(),
            });
        press_key(&ui, slint::platform::Key::Return);
        assert_eq!(
            saved.get(),
            1,
            "keyboard must reach pinned Save without leaving Settings"
        );
        press_key(&ui, slint::platform::Key::Tab);
        press_key(&ui, slint::platform::Key::RightArrow);
        assert_eq!(ui.get_s_tab(), 1);
        press_key(&ui, slint::platform::Key::RightArrow);
        assert_eq!(ui.get_s_tab(), 2);
        assert!(
            refreshed.get() > 0,
            "entering Browser must request fresh status"
        );
        let browser_tab = ElementHandle::find_by_accessible_label(&ui, "Browser extension")
            .next()
            .unwrap();
        assert_eq!(browser_tab.accessible_item_selected(), Some(true));
        let register = ElementHandle::find_by_accessible_label(&ui, "Register / repair")
            .next()
            .unwrap();
        // Register is near the top and reachable without scrolling at minimum size.
        register.mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(registered.get(), 1);
        ui.set_browser_busy(true);
        assert_eq!(register.accessible_enabled(), Some(false));
        register.mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(
            registered.get(),
            1,
            "busy state must reject duplicate actions"
        );
        // An unverified profile must never be described as having the extension installed.
        assert!(ui.get_browser_status().is_empty());
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(673.0, 240.0),
                delta_x: 0.0,
                delta_y: -2000.0,
            });
        let copied = ElementHandle::find_by_accessible_label(&ui, "Copy path")
            .next()
            .unwrap();
        assert_eq!(copied.accessible_enabled(), Some(false));
        ui.set_browser_busy(false);
        ui.set_browser_folder("C:\\fixture\\extension".into());
        assert_eq!(copied.accessible_enabled(), Some(true));
        press_key(&ui, slint::platform::Key::Escape);
        assert!(!ui.get_set_open());
    }

    #[test]
    fn empty_list_offers_browser_setup_and_filtered_list_offers_reset() {
        use i_slint_backend_testing::ElementHandle;
        use std::cell::Cell;
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        apply_lang("en");
        let weak = ui.as_weak();
        ui.on_settings_clicked(move || weak.upgrade().unwrap().set_set_open(true));
        ui.show().unwrap();
        ui.window().set_size(slint::LogicalSize::new(760.0, 480.0));
        ElementHandle::find_by_accessible_label(&ui, "Connect browser")
            .next()
            .unwrap()
            .mock_single_click(slint::platform::PointerEventButton::Left);
        assert!(ui.get_set_open());
        assert_eq!(ui.get_s_tab(), 2);
        ui.set_set_open(false);
        ui.set_counts(ModelRc::new(VecModel::from(vec![3; 10])));
        ui.set_filter(2);
        let filtered = Rc::new(Cell::new(-1));
        let calls = filtered.clone();
        ui.on_filter_clicked(move |f| calls.set(f));
        // The sidebar and empty-state action share the label. The latter is in the list column.
        let reset = ElementHandle::find_by_accessible_label(&ui, "All downloads")
            .find(|e| e.absolute_position().x > 196.0)
            .unwrap();
        reset.mock_single_click(slint::platform::PointerEventButton::Left);
        assert_eq!(filtered.get(), 0);
        assert!(
            ElementHandle::find_by_accessible_label(&ui, "No downloads in this view")
                .next()
                .is_some()
        );
    }

    #[test]
    fn exit_protects_queued_downloading_and_verifying() {
        for s in [Status::Queued, Status::Downloading, Status::Verifying] {
            assert!(active(s));
        }
        for s in [Status::Paused, Status::Completed, Status::Error] {
            assert!(!active(s));
        }
    }

    #[test]
    fn compact_rows_are_current_as_soon_as_it_opens() {
        i_slint_backend_testing::init_no_event_loop();
        let state = tempfile::tempdir().unwrap();
        let app = Rc::new(RefCell::new(App {
            mgr: Some(Manager::new(state.path())),
            filter: 0,
            selected: None,
            visible: vec![],
            detail_for: None,
            last_clip: String::new(),
            clipboard: None,
            models: Models::new(),
        }));
        let ui = AppWindow::new().unwrap();
        let compact = CompactWindow::new().unwrap();
        bind_models(&ui, &compact, &app.borrow().models);
        refresh_now(&ui, &mut app.borrow_mut());
        // Not started, so no network is touched.
        let id = app.borrow().mgr().add(NewDownload {
            url: "https://example.invalid/fixture.bin".into(),
            start: false,
            ..Default::default()
        });
        open_compact(&ui, &compact, &app);
        assert!(compact.window().is_visible());
        assert_eq!(compact.get_rows().row_count(), 1);
        restore_main(&ui, &compact);
        // The 500 ms timer has not run yet, so the compact model still holds the removed row.
        app.borrow().mgr().remove(id, false);
        open_compact(&ui, &compact, &app);
        assert_eq!(compact.get_rows().row_count(), 0);
        restore_main(&ui, &compact);
    }

    #[test]
    fn repeated_window_switches_keep_download_and_exit_confirmation_state() {
        use std::time::Instant;
        i_slint_backend_testing::init_no_event_loop();
        let state = tempfile::tempdir().unwrap();
        let downloads = tempfile::tempdir().unwrap();
        let mgr = Manager::new(state.path());
        let mut settings = mgr.settings();
        settings.download_dir = downloads.path().into();
        settings.auto_verify = false;
        settings.max_connections = 1;
        mgr.set_settings(settings).unwrap();
        let bytes = fixture_server::test_data(256 * 1024);
        let server = mgr
            .runtime()
            .block_on(fixture_server::start(bytes.clone(), true, 128 * 1024));
        let app = Rc::new(RefCell::new(App {
            mgr: Some(mgr),
            filter: 0,
            selected: None,
            visible: vec![],
            detail_for: None,
            last_clip: String::new(),
            clipboard: None,
            models: Models::new(),
        }));
        // Gate HTTP responses so confirmation cannot race a fast download.
        // Declared after App so a failed assertion releases the gate before Runtime drops.
        let gate = server.data.lock().unwrap();
        let id = app.borrow().mgr().add(NewDownload {
            url: server.url("fixture.bin"),
            start: true,
            ..Default::default()
        });
        app.borrow_mut().selected = Some(id);
        let ui = AppWindow::new().unwrap();
        let compact = CompactWindow::new().unwrap();
        ui.show().unwrap();
        assert!(!compact.get_pinned());
        for _ in 0..30 {
            show_compact(&ui, &compact);
            assert!(compact.window().is_visible());
            assert!(!ui.window().is_visible());
            restore_main(&ui, &compact);
            assert!(ui.window().is_visible());
            assert!(!compact.window().is_visible());
        }
        request_exit(&ui, &compact, &app);
        assert!(ui.get_exit_open());
        request_exit(&ui, &compact, &app);
        assert!(ui.get_exit_open());
        minimize_to_tray(&ui, &compact, true);
        assert!(
            ui.window().is_visible(),
            "exit confirmation must stay accessible"
        );
        show_compact(&ui, &compact);
        assert!(ui.window().is_visible());
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Tab.into(),
            });
        assert!(ui.get_exit_choice());
        assert!(ui.get_exit_open());
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::Tab.into(),
            });
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                text: slint::platform::Key::Escape.into(),
            });
        assert!(!ui.get_exit_open());
        ui.window()
            .dispatch_event(slint::platform::WindowEvent::KeyReleased {
                text: slint::platform::Key::Escape.into(),
            });
        ui.hide().unwrap(); // Same operation as minimize-to-tray, with no native tray in this backend.
        drop(gate);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let snaps = app.borrow().mgr().snapshots();
            assert_eq!(snaps.len(), 1, "switching must not duplicate downloads");
            assert_eq!(snaps[0].id, id);
            assert_ne!(snaps[0].status, Status::Error, "{:?}", snaps[0].error);
            if snaps[0].status == Status::Completed {
                break;
            }
            assert!(Instant::now() < deadline, "fixture download timed out");
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(
            std::fs::read(downloads.path().join("fixture.bin")).unwrap(),
            bytes
        );
        restore_main(&ui, &compact);
        assert!(ui.window().is_visible());
        assert!(!ui.get_exit_open());
        ui.hide().unwrap();
        minimize_to_tray(&ui, &compact, false);
        assert!(
            ui.window().is_visible(),
            "a missing tray must never strand the user"
        );
        assert!(!ui.get_notice().is_empty());
        app.borrow_mut().mgr.take().unwrap().shutdown();
    }
}
