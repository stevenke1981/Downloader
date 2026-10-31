//! List and file actions: live search, keyboard selection, clear-completed,
//! open or locate a finished file, clipboard prefill for the add form, and the
//! notice banner's severity.
use crate::{App, AppWindow, Tr, open_add, refresh_now, reveal};
use dlcore::util::looks_like_url;
use dlcore::{Snapshot, Status};
use reveal::RevealError;
use slint::{ComponentHandle, SharedString};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// Most links the add form takes from the clipboard; matches the batch limit.
const PREFILL_LIMIT: usize = 1000;

/// A file operation that runs off the UI thread on one selected download.
type FileAction = fn(Status, &Path) -> Result<(), RevealError>;

pub(super) fn bind(ui: &AppWindow, app: &Rc<RefCell<App>>) {
    {
        let (weak, app) = (ui.as_weak(), app.clone());
        ui.on_search_changed(move |text| {
            let Some(ui) = weak.upgrade() else { return };
            let mut app = app.borrow_mut();
            app.search = text.trim().to_lowercase();
            refresh_now(&ui, &mut app);
        });
    }
    {
        let (weak, app) = (ui.as_weak(), app.clone());
        ui.on_clear_completed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut app = app.borrow_mut();
            let removed = clear_completed(&mut app);
            refresh_now(&ui, &mut app);
            if removed > 0 {
                let text = ui.global::<Tr>().invoke_cleared(removed as i32);
                show_notice(&ui, text, false);
            }
        });
    }
    {
        let (weak, app) = (ui.as_weak(), app.clone());
        ui.on_open_file(move || {
            let Some(ui) = weak.upgrade() else { return };
            let app = app.borrow();
            if let Some(id) = app.selected {
                let program = app
                    .mgr()
                    .snapshots()
                    .iter()
                    .any(|s| s.id == id && reveal::reveals_instead_of_opening(&s.path));
                let done = if program {
                    ui.global::<Tr>().invoke_program_revealed()
                } else {
                    SharedString::default()
                };
                run_file_action(&ui, &app, id, reveal::open_file, done);
            }
        });
    }
    {
        let (weak, app) = (ui.as_weak(), app.clone());
        ui.on_select_relative(move |delta| {
            let Some(ui) = weak.upgrade() else { return };
            let mut app = app.borrow_mut();
            select_relative(&mut app, delta);
            refresh_now(&ui, &mut app);
        });
    }
    {
        let (weak, app) = (ui.as_weak(), app.clone());
        ui.on_add_clicked(move || {
            let Some(ui) = weak.upgrade() else { return };
            let text = {
                let mut app = app.borrow_mut();
                let text = app
                    .clipboard
                    .as_mut()
                    .and_then(|c| c.get_text().ok())
                    .unwrap_or_default();
                // The clipboard watcher must not offer the same text again.
                app.last_clip = text.clone();
                text
            };
            open_add(&ui, &clipboard_links(&text));
        });
    }
}

/// Shows the notice banner with its severity. Errors render red; other text is neutral.
pub(super) fn show_notice(ui: &AppWindow, text: impl Into<SharedString>, is_error: bool) {
    ui.set_notice(text.into());
    ui.set_notice_is_error(is_error);
}

/// Reveals a download's file in its folder, or opens a directory directly.
pub(super) fn reveal_download(ui: &AppWindow, app: &App, id: u64) {
    run_file_action(ui, app, id, reveal::reveal, SharedString::default());
}

/// Runs `action` on a worker thread for the download `id`; `done` is the notice shown on success. Ignored while another
/// file action is still running, so repeated clicks cannot start several at once.
fn run_file_action(ui: &AppWindow, app: &App, id: u64, action: FileAction, done: SharedString) {
    if ui.get_reveal_busy() {
        return;
    }
    let Some(snapshot) = app.mgr().snapshots().into_iter().find(|s| s.id == id) else {
        return;
    };
    ui.set_reveal_busy(true);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = action(snapshot.status, &snapshot.path);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            match result {
                Ok(()) => show_notice(&ui, done, false),
                Err(error) => show_notice(&ui, locate_error_text(&ui, error), true),
            }
            ui.set_reveal_busy(false);
        });
    });
}

/// Localized banner text for a file operation that failed.
fn locate_error_text(ui: &AppWindow, error: RevealError) -> SharedString {
    let tr = ui.global::<Tr>();
    match error {
        RevealError::NotCompleted => tr.invoke_unfinished(),
        RevealError::Missing | RevealError::NotFile => tr.invoke_missing_file(),
        RevealError::Io(detail) => format!("{} {detail}", tr.invoke_reveal_failed()).into(),
    }
}

/// Ids of every completed download, in list order.
pub(super) fn completed_ids(snaps: &[Snapshot]) -> Vec<u64> {
    snaps
        .iter()
        .filter(|s| s.status == Status::Completed)
        .map(|s| s.id)
        .collect()
}

/// Removes the completed downloads the user can currently see (sidebar filter and
/// search applied) from the list. Files stay on disk.
pub(super) fn clear_completed(app: &mut App) -> usize {
    let ids: Vec<u64> = completed_ids(&app.mgr().snapshots())
        .into_iter()
        .filter(|id| app.visible.contains(id))
        .collect();
    for id in &ids {
        app.mgr().remove(*id, false);
    }
    if app.selected.is_some_and(|id| ids.contains(&id)) {
        app.selected = None;
    }
    ids.len()
}

/// Moves the selection by `delta` inside the visible list. With nothing selected,
/// a positive step picks the first row and a negative step the last one.
pub(super) fn select_relative(app: &mut App, delta: i32) {
    let current = app
        .selected
        .and_then(|id| app.visible.iter().position(|&visible| visible == id));
    if let Some(index) = relative_index(current, app.visible.len(), delta) {
        app.selected = app.visible.get(index).copied();
    }
}

/// Index after moving `delta` steps from `current`, clamped to `0..len`.
/// Returns `None` when there is nothing to select or `delta` is zero.
pub(super) fn relative_index(current: Option<usize>, len: usize, delta: i32) -> Option<usize> {
    if len == 0 || delta == 0 {
        return None;
    }
    let last = len - 1;
    let index = match current {
        Some(index) => (index.min(last) as i64 + i64::from(delta)).clamp(0, last as i64) as usize,
        None if delta > 0 => 0,
        None => last,
    };
    Some(index)
}

/// Lines of clipboard text that are download sources, trimmed and capped at
/// `PREFILL_LIMIT`. Empty when nothing in the text looks like a source.
pub(super) fn clipboard_links(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| looks_like_url(line))
        .take(PREFILL_LIMIT)
        .collect::<Vec<_>>()
        .join("\n")
}
