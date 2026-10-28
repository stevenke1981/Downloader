//! Download input, OS drops, sharing and preview commands.
use crate::{
    App, AppWindow, CompactWindow, LogWindow, PreviewWindow, Tr, batch, open_add, restore_main,
};
use dlcore::task::DownloadKind;
use dlcore::{Checksum, NewDownload};
use slint::ComponentHandle;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub struct Controller {
    logs: LogWindow,
    preview: PreviewWindow,
    timer: slint::Timer,
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.timer.stop();
        let _ = self.logs.hide();
        let _ = self.preview.hide();
    }
}

fn connection_override(value: i32) -> Option<u32> {
    (value > 0).then_some(value.clamp(1, 32) as u32)
}

pub fn bind(
    ui: &AppWindow,
    compact: &CompactWindow,
    app: &Rc<RefCell<App>>,
) -> Result<Controller, slint::PlatformError> {
    let logs = LogWindow::new()?;
    let preview = PreviewWindow::new()?;
    {
        let (weak, select_app) = (ui.as_weak(), app.clone());
        ui.on_download_added(move |id| {
            if let Some(ui) = weak.upgrade() {
                let mut app = select_app.borrow_mut();
                app.selected = Some(id as u64);
                crate::refresh_now(&ui, &mut app);
            }
        });
    }
    {
        let (weak, paste_app) = (ui.as_weak(), app.clone());
        ui.on_paste_links(move || {
            if let Some(ui) = weak.upgrade() {
                let text = paste_app
                    .borrow_mut()
                    .clipboard
                    .as_mut()
                    .and_then(|c| c.get_text().ok());
                if let Some(text) = text {
                    ui.set_add_url(text.into());
                }
            }
        });
        let weak = ui.as_weak();
        ui.on_import_files(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_add_busy() {
                return;
            }
            ui.set_add_busy(true);
            let weak = ui.as_weak();
            std::thread::spawn(move || {
                let chosen = rfd::FileDialog::new()
                    .add_filter("Torrent / Metalink", &["torrent", "metalink", "meta4"])
                    .pick_files();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_add_busy(false);
                    if let Some(paths) = chosen {
                        let mut input = ui.get_add_url().to_string();
                        for path in paths {
                            if !input.is_empty() {
                                input.push('\n');
                            }
                            input.push_str(&path.display().to_string());
                        }
                        ui.set_add_url(input.into());
                    }
                });
            });
        });
        let (weak, manager) = (ui.as_weak(), app.borrow().mgr().clone());
        ui.on_add_ok(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_add_busy() {
                return;
            }
            let sum = ui.get_add_checksum().trim().to_owned();
            let checksum = if sum.is_empty() {
                None
            } else {
                let Some(checksum) = Checksum::guess(&sum) else {
                    ui.set_add_error(ui.global::<Tr>().invoke_invalid_checksum());
                    return;
                };
                Some(checksum)
            };
            let folder = ui.get_add_folder().trim().to_owned();
            let template = NewDownload {
                dir: (!folder.is_empty()).then(|| PathBuf::from(folder)),
                connections: connection_override(ui.get_add_conns()),
                kind: if ui.get_add_kind() == 1 {
                    DownloadKind::Video
                } else {
                    DownloadKind::Auto
                },
                checksum,
                start: ui.get_add_start(),
                ..Default::default()
            };
            let (input, manager, weak) =
                (ui.get_add_url().to_string(), manager.clone(), ui.as_weak());
            ui.set_add_busy(true);
            ui.set_add_error("".into());
            std::thread::spawn(move || {
                let result = batch::enqueue(&manager, &input, template);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_add_busy(false);
                    match result {
                        Ok(ids) => {
                            ui.set_add_open(false);
                            if let Some(id) = ids.last() {
                                ui.invoke_download_added(*id as i32);
                            }
                            ui.set_notice(ui.global::<Tr>().invoke_links_added(ids.len() as i32));
                        }
                        Err(error) => ui.set_add_error(batch::message(&ui, error)),
                    }
                });
            });
        });
    }
    for window in [ui.window(), compact.window()] {
        let (weak, compact) = (ui.as_weak(), compact.as_weak());
        window.on_winit_window_event(move |_, event| {
            if let winit::event::WindowEvent::DroppedFile(path) = event {
                if let (Some(ui), Some(compact)) = (weak.upgrade(), compact.upgrade()) {
                    restore_main(&ui, &compact);
                    if ui.get_add_busy() {
                        return EventResult::PreventDefault;
                    }
                    if !ui.get_add_open() {
                        open_add(&ui, "");
                    }
                    let mut input = ui.get_add_url().to_string();
                    if !input.is_empty() {
                        input.push('\n');
                    }
                    input.push_str(&path.display().to_string());
                    ui.set_add_url(input.into());
                }
                return EventResult::PreventDefault;
            }
            EventResult::Propagate
        });
    }
    {
        let (weak, copy_app) = (ui.as_weak(), app.clone());
        ui.on_copy_url(move || {
            let Some(ui) = weak.upgrade() else { return };
            let result = copy_app
                .borrow_mut()
                .clipboard
                .as_mut()
                .ok_or_else(|| "Clipboard unavailable".to_owned())
                .and_then(|clipboard| {
                    clipboard
                        .set_text(ui.get_d_url().to_string())
                        .map_err(|e| e.to_string())
                });
            ui.set_notice(match result {
                Ok(()) => ui.global::<Tr>().invoke_copied(),
                Err(error) => error.into(),
            });
        });
        let (weak, app, preview_weak) = (ui.as_weak(), app.clone(), preview.as_weak());
        ui.on_preview(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_reveal_busy() {
                return;
            }
            let app = app.borrow();
            let Some(snapshot) = app
                .selected
                .and_then(|id| app.mgr().snapshots().into_iter().find(|s| s.id == id))
            else {
                return;
            };
            ui.set_reveal_busy(true);
            let weak = ui.as_weak();
            let preview_weak = preview_weak.clone();
            std::thread::spawn(move || {
                let result = crate::reveal::preview(snapshot.status, &snapshot.path);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_reveal_busy(false);
                    if let Ok(content) = result {
                        if let Some(preview) = preview_weak.upgrade() {
                            preview.set_download_name(content.name.into());
                            preview.set_content(content.text.into());
                            let image = content.image.map(|(width, height, bytes)| {
                                let pixels =
                                    slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                        &bytes, width, height,
                                    );
                                slint::Image::from_rgba8(pixels)
                            });
                            preview.set_has_image(image.is_some());
                            preview.set_picture(image.unwrap_or_default());
                            let _ = preview.show();
                            preview.window().set_minimized(false);
                        }
                    } else if let Err(error) = result {
                        use crate::reveal::{PreviewError, RevealError};
                        let tr = ui.global::<Tr>();
                        ui.set_notice(match error {
                            PreviewError::Unsupported => tr.invoke_preview_unsupported(),
                            PreviewError::File(RevealError::NotCompleted) => tr.invoke_unfinished(),
                            PreviewError::File(RevealError::Missing | RevealError::NotFile) => {
                                tr.invoke_missing_file()
                            }
                            PreviewError::File(RevealError::Io(error))
                            | PreviewError::Open(error) => {
                                format!("{} {error}", tr.invoke_preview_failed()).into()
                            }
                        });
                    }
                });
            });
        });
        let (weak, log_weak) = (ui.as_weak(), logs.as_weak());
        ui.on_show_log(move || {
            if let (Some(ui), Some(logs)) = (weak.upgrade(), log_weak.upgrade()) {
                logs.set_download_name(ui.get_d_name());
                logs.set_log(ui.get_d_log());
                let _ = logs.show();
                logs.window().set_minimized(false);
            }
        });
    }
    let timer = slint::Timer::default();
    let (weak, log_weak) = (ui.as_weak(), logs.as_weak());
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(500),
        move || {
            if let (Some(ui), Some(logs)) = (weak.upgrade(), log_weak.upgrade())
                && logs.window().is_visible()
            {
                logs.set_download_name(ui.get_d_name());
                logs.set_log(ui.get_d_log());
            }
        },
    );
    Ok(Controller {
        logs,
        preview,
        timer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dlcore::{Manager, site::SiteRule};

    #[test]
    fn add_form_automatic_uses_each_site_default_and_preserves_override() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        ui.set_add_conns(8);
        open_add(&ui, "");
        assert_eq!(ui.get_add_conns(), 0);
        let state = tempfile::tempdir().unwrap();
        let manager = Manager::new(state.path());
        let mut settings = manager.settings();
        settings.max_connections = 8;
        settings.site_rules.push(SiteRule {
            host: "https://site.example".into(),
            connections: 2,
            ..Default::default()
        });
        manager.set_settings(settings).unwrap();
        let ids = batch::enqueue(
            &manager,
            "https://site.example/a.bin\nhttps://global.example/b.bin",
            NewDownload {
                connections: connection_override(ui.get_add_conns()),
                ..Default::default()
            },
        )
        .unwrap();
        let snapshots = manager.snapshots();
        assert_eq!(
            snapshots
                .iter()
                .find(|s| s.id == ids[0])
                .unwrap()
                .connections,
            2
        );
        assert_eq!(
            snapshots
                .iter()
                .find(|s| s.id == ids[1])
                .unwrap()
                .connections,
            8
        );
        ui.set_add_conns(5);
        let ids = batch::enqueue(
            &manager,
            "https://site.example/c.bin",
            NewDownload {
                connections: connection_override(ui.get_add_conns()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            manager
                .snapshots()
                .iter()
                .find(|s| s.id == ids[0])
                .unwrap()
                .connections,
            5
        );
        manager.shutdown();
    }
}
