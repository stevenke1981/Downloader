//! UI adapter for explicit browser setup actions. Registry/filesystem work stays
//! off Slint's event loop; clipboard ownership stays with the existing App.
use crate::browser_setup::{self, Browser, NativeHostStatus};
use crate::{App, AppWindow, Tr};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Copy)]
enum Action {
    Refresh,
    Register,
    OpenPage,
    OpenFolder,
}

fn start(ui: &AppWindow, action: Action) {
    if ui.get_browser_busy() {
        return;
    }
    let Some(browser) = Browser::from_index(ui.get_s_browser()) else {
        ui.set_browser_message(ui.global::<Tr>().invoke_browser_unavailable());
        return;
    };
    ui.set_browser_busy(true);
    ui.set_browser_message(if matches!(action, Action::Register) {
        ui.global::<Tr>().invoke_browser_registering()
    } else {
        "".into()
    });
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = match action {
            Action::Refresh => Ok(()),
            Action::Register => browser_setup::register(),
            Action::OpenPage => browser_setup::open_extensions_page(browser),
            Action::OpenFolder => browser_setup::open_extension_folder(),
        };
        let status = browser_setup::status(browser);
        let folder = browser_setup::extension_dir();
        let installed = browser_setup::browser_executable(browser);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            apply(&ui, action, result, status, folder, installed);
        });
    });
}

fn apply(
    ui: &AppWindow,
    action: Action,
    result: Result<(), String>,
    status: NativeHostStatus,
    folder: Result<PathBuf, String>,
    installed: Result<PathBuf, String>,
) {
    let tr = ui.global::<Tr>();
    ui.set_browser_status(if status.registered {
        tr.invoke_browser_host_ready()
    } else if let Some(issue) = status.issue {
        format!("{} {issue}", tr.invoke_browser_host_missing()).into()
    } else {
        tr.invoke_browser_host_missing()
    });
    ui.set_browser_folder(
        folder
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    let message = match result {
        Err(error) => {
            let prefix = match action {
                Action::Register => tr.invoke_browser_register_failed(),
                Action::Refresh => tr.invoke_browser_check_failed(),
                Action::OpenPage | Action::OpenFolder => tr.invoke_browser_open_failed(),
            };
            format!("{prefix} {error}").into()
        }
        Ok(()) => match action {
            Action::Register => tr.invoke_browser_registered(),
            Action::OpenPage => tr.invoke_browser_opened(),
            Action::OpenFolder => tr.invoke_browser_folder_opened(),
            Action::Refresh => match (folder, installed) {
                (Err(error), _) => format!("{} {error}", tr.invoke_browser_path_missing()).into(),
                (_, Err(error)) => format!("{} {error}", tr.invoke_browser_unavailable()).into(),
                _ => "".into(),
            },
        },
    };
    ui.set_browser_message(message);
    ui.set_browser_busy(false);
}

pub fn bind(ui: &AppWindow, app: &Rc<RefCell<App>>) {
    let weak = ui.as_weak();
    ui.on_browser_refresh(move || {
        if let Some(ui) = weak.upgrade() {
            start(&ui, Action::Refresh);
        }
    });
    let weak = ui.as_weak();
    ui.on_browser_register(move || {
        if let Some(ui) = weak.upgrade() {
            start(&ui, Action::Register);
        }
    });
    let weak = ui.as_weak();
    ui.on_browser_open_page(move || {
        if let Some(ui) = weak.upgrade() {
            start(&ui, Action::OpenPage);
        }
    });
    let weak = ui.as_weak();
    ui.on_browser_open_folder(move || {
        if let Some(ui) = weak.upgrade() {
            start(&ui, Action::OpenFolder);
        }
    });
    let weak = ui.as_weak();
    let app = app.clone();
    ui.on_browser_copy_path(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_browser_busy() {
            return;
        }
        let mut app = app.borrow_mut();
        let result = app
            .clipboard
            .as_mut()
            .ok_or_else(|| "System clipboard is unavailable".to_string())
            .and_then(browser_setup::copy_extension_path);
        let tr = ui.global::<Tr>();
        ui.set_browser_message(match result {
            Ok(()) => tr.invoke_browser_path_copied(),
            Err(error) => format!("{} {error}", tr.invoke_browser_copy_failed()).into(),
        });
    });
}
