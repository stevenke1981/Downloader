//! Typed desktop forms for the management services. Filesystem, network and
//! process work runs on workers; only weak handles return to the UI thread.
use crate::{App, AppWindow, ToolTr, ToolsWindow, batch, kbs, parse_kbs};
use dlcore::automation::{Action, ScheduledAction};
use dlcore::remote::{RemoteServer, RemoteSettings};
use dlcore::settings::{FolderRule, TrafficMode};
use dlcore::site::SiteRule;
use dlcore::{Manager, NewDownload};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
struct Context {
    manager: Manager,
    remote: Arc<Mutex<Option<RemoteServer>>>,
}

pub struct Controller {
    window: ToolsWindow,
    context: Context,
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.context.remote.lock().unwrap().take();
        let _ = self.window.hide();
    }
}

fn mode(index: i32) -> TrafficMode {
    match index {
        1 => TrafficMode::Low,
        2 => TrafficMode::Medium,
        3 => TrafficMode::High,
        _ => TrafficMode::Custom,
    }
}

fn ftp_directory(directory: &str) -> Result<url::Url, String> {
    let mut base = url::Url::parse(directory.trim()).map_err(|error| error.to_string())?;
    if base.scheme() != "ftp"
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Err("FTP directory must be an ftp:// URL without embedded credentials".into());
    }
    base.set_query(None);
    base.set_fragment(None);
    if !base.path().ends_with('/') {
        base.path_segments_mut()
            .map_err(|()| "FTP URL has no directory path")?
            .push("");
    }
    Ok(base)
}

/// NLST returns raw names, so append them as path segments rather than treating
/// their punctuation as URL syntax or replacing the FTP directory's last part.
fn ftp_sources(directory: &url::Url, names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .map(|name| {
            let mut source = directory.clone();
            source
                .path_segments_mut()
                .expect("validated FTP directory has path segments")
                .pop_if_empty()
                .push(&name);
            source.into()
        })
        .collect()
}

fn populate(ui: &ToolsWindow, context: &Context) {
    let settings = context.manager.settings();
    ui.set_traffic(match settings.traffic_mode {
        TrafficMode::Custom => 0,
        TrafficMode::Low => 1,
        TrafficMode::Medium => 2,
        TrafficMode::High => 3,
    });
    ui.set_low(kbs(settings.traffic_presets[0]).into());
    ui.set_medium(kbs(settings.traffic_presets[1]).into());
    ui.set_high(kbs(settings.traffic_presets[2]).into());
    ui.set_custom(kbs(settings.global_limit).into());
    ui.set_sites(ModelRc::new(VecModel::from(
        settings
            .site_rules
            .iter()
            .map(|s| s.host.clone().into())
            .collect::<Vec<_>>(),
    )));
    ui.set_folders(ModelRc::new(VecModel::from(
        settings
            .folder_rules
            .iter()
            .map(|r| format!("{} → {}", r.extensions.join(", "), r.folder.display()).into())
            .collect::<Vec<_>>(),
    )));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    ui.set_actions(ModelRc::new(VecModel::from(
        settings
            .scheduled_actions
            .iter()
            .map(|a| {
                let tr = ui.global::<ToolTr>();
                let label = tr.invoke_action_name(match a.action {
                    Action::StartAll => 0,
                    Action::PauseAll => 1,
                    Action::Run { .. } => 2,
                    Action::Shutdown => 3,
                });
                if a.consumed {
                    tr.invoke_consumed(a.id as i32, label)
                } else {
                    tr.invoke_scheduled(a.id as i32, label, ((a.at - now).max(0) / 60) as i32)
                }
            })
            .collect::<Vec<_>>(),
    )));
    ui.set_automation_log(context.manager.automation_log().join("\n").into());
    ui.set_remote_enabled(settings.remote.enabled);
    ui.set_bind_address(settings.remote.bind.into());
    ui.set_token(settings.remote.token.into());
    let tr = ui.global::<ToolTr>();
    ui.set_remote_status(match context.remote.lock().unwrap().as_ref() {
        Some(server) => tr.invoke_remote_on(server.address().to_string().into()),
        None => tr.invoke_remote_off(),
    });
}

fn run(
    ui: &ToolsWindow,
    context: &Context,
    operation: impl FnOnce(&Context) -> Result<(), String> + Send + 'static,
) {
    if ui.get_busy() {
        return;
    }
    ui.set_busy(true);
    ui.set_message(ui.global::<ToolTr>().invoke_working());
    let weak = ui.as_weak();
    let context = context.clone();
    std::thread::spawn(move || {
        let result = operation(&context);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            populate(&ui, &context);
            ui.set_busy(false);
            ui.set_message(match result {
                Ok(()) => ui.global::<ToolTr>().invoke_saved(),
                Err(error) => error.into(),
            });
        });
    });
}

fn restart_remote(context: &Context, settings: RemoteSettings) -> Result<(), String> {
    let mut running = context.remote.lock().unwrap();
    running.take();
    if settings.enabled {
        *running = Some(dlcore::remote::serve(context.manager.clone(), settings)?);
    }
    Ok(())
}

fn refresh_capabilities(ui: &ToolsWindow) {
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let capabilities = dlcore::protocols::capabilities();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let tr = ui.global::<ToolTr>();
            ui.set_capabilities(
                capabilities
                    .iter()
                    .map(|capability| {
                        format!(
                            "{}: {}",
                            capability.name,
                            if capability.available {
                                tr.invoke_ready_tool()
                            } else {
                                tr.invoke_missing_tool()
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into(),
            );
        });
    });
}

pub fn bind(main: &AppWindow, app: &Rc<RefCell<App>>) -> Result<Controller, slint::PlatformError> {
    let ui = ToolsWindow::new()?;
    let context = Context {
        manager: app.borrow().mgr().clone(),
        remote: Arc::new(Mutex::new(None)),
    };
    populate(&ui, &context);
    {
        let (weak, context) = (ui.as_weak(), context.clone());
        main.on_tools_clicked(move || {
            if let Some(ui) = weak.upgrade() {
                populate(&ui, &context);
                let _ = ui.show();
                ui.window().set_minimized(false);
            }
        });
        let weak = ui.as_weak();
        ui.on_dismiss(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
        });
        let weak = ui.as_weak();
        ui.window().on_close_requested(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
    macro_rules! bind {
        ($callback:ident, |$ui:ident, $context:ident| $body:block) => {{
            let weak = ui.as_weak();
            let context = context.clone();
            ui.$callback(move || {
                let Some($ui) = weak.upgrade() else { return };
                let $context = &context;
                $body
            });
        }};
    }
    bind!(on_traffic_save, |ui, context| {
        let limits = [
            parse_kbs(&ui.get_low()),
            parse_kbs(&ui.get_medium()),
            parse_kbs(&ui.get_high()),
            parse_kbs(&ui.get_custom()),
        ];
        let [Some(low), Some(medium), Some(high), Some(custom)] = limits else {
            ui.set_message(ui.global::<ToolTr>().invoke_invalid_number());
            return;
        };
        let traffic_mode = mode(ui.get_traffic());
        run(&ui, context, move |context| {
            let mut settings = context.manager.settings();
            settings.traffic_presets = [low, medium, high];
            settings.traffic_mode = traffic_mode;
            settings.global_limit = custom;
            context.manager.set_settings(settings)
        });
    });
    bind!(on_site_load, |ui, context| {
        if let Some(site) = context
            .manager
            .settings()
            .site_rules
            .get(ui.get_site_index() as usize)
        {
            ui.set_host(site.host.clone().into());
            ui.set_username(site.username.clone().into());
            ui.set_password("".into());
            ui.set_concurrent(site.max_concurrent as i32);
            ui.set_connections(site.connections as i32);
        }
    });
    bind!(on_site_save, |ui, context| {
        let site = SiteRule {
            host: ui.get_host().trim().to_owned(),
            max_concurrent: ui.get_concurrent() as u32,
            connections: ui.get_connections() as u32,
            username: ui.get_username().trim().to_owned(),
        };
        let password = ui.get_password().to_string();
        ui.set_password("".into());
        let index = ui.get_site_index();
        run(&ui, context, move |context| {
            let mut settings = context.manager.settings();
            if let Some(existing) = settings.site_rules.get_mut(index as usize) {
                *existing = site.clone();
            } else {
                settings.site_rules.push(site.clone());
            }
            context.manager.set_settings(settings)?;
            if !password.is_empty() {
                context
                    .manager
                    .set_site_credentials(&site.host, &site.username, &password)?;
            }
            Ok(())
        });
    });
    bind!(on_site_remove, |ui, context| {
        let index = ui.get_site_index();
        run(&ui, context, move |context| {
            let mut settings = context.manager.settings();
            if index >= 0 && (index as usize) < settings.site_rules.len() {
                let site = settings.site_rules.remove(index as usize);
                context.manager.set_site_credentials(&site.host, "", "")?;
                context.manager.set_settings(settings)?;
            }
            Ok(())
        });
        ui.set_site_index(-1);
        ui.set_host("".into());
        ui.set_username("".into());
        ui.set_password("".into());
    });
    bind!(on_folder_load, |ui, context| {
        if let Some(rule) = context
            .manager
            .settings()
            .folder_rules
            .get(ui.get_folder_index() as usize)
        {
            ui.set_extensions(rule.extensions.join(", ").into());
            ui.set_folder(rule.folder.display().to_string().into());
        }
    });
    bind!(on_folder_save, |ui, context| {
        let index = ui.get_folder_index();
        let rule = FolderRule {
            extensions: ui
                .get_extensions()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            folder: PathBuf::from(ui.get_folder().trim()),
        };
        run(&ui, context, move |context| {
            let mut settings = context.manager.settings();
            if let Some(existing) = settings.folder_rules.get_mut(index as usize) {
                *existing = rule;
            } else {
                settings.folder_rules.push(rule);
            }
            context.manager.set_settings(settings)
        });
    });
    bind!(on_folder_remove, |ui, context| {
        let index = ui.get_folder_index();
        run(&ui, context, move |context| {
            let mut settings = context.manager.settings();
            if index >= 0 && (index as usize) < settings.folder_rules.len() {
                settings.folder_rules.remove(index as usize);
                context.manager.set_settings(settings)?;
            }
            Ok(())
        });
        ui.set_folder_index(-1);
    });
    bind!(on_folder_browse, |ui, _context| {
        if ui.get_busy() {
            return;
        }
        ui.set_busy(true);
        let weak = ui.as_weak();
        let current = ui.get_folder().to_string();
        std::thread::spawn(move || {
            let chosen = rfd::FileDialog::new().set_directory(current).pick_folder();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_busy(false);
                if let Some(path) = chosen {
                    ui.set_folder(path.display().to_string().into());
                }
            });
        });
    });
    bind!(on_crawl, |ui, context| {
        if ui.get_busy() {
            return;
        }
        let url = ui.get_web_url().to_string();
        let options = dlcore::grabber::GrabOptions {
            max_depth: ui.get_depth() as usize,
            max_pages: ui.get_pages() as usize,
        };
        ui.set_busy(true);
        ui.set_message(ui.global::<ToolTr>().invoke_working());
        let (weak, manager) = (ui.as_weak(), context.manager.clone());
        std::thread::spawn(move || {
            let result = manager.crawl_client().and_then(|client| {
                manager
                    .runtime()
                    .block_on(dlcore::grabber::crawl(&client, &url, options))
            });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_busy(false);
                match result {
                    Ok(links) => {
                        ui.set_links(links.join("\n").into());
                        ui.set_message("".into());
                    }
                    Err(error) => ui.set_message(error.into()),
                }
            });
        });
    });
    bind!(on_ftp_list, |ui, context| {
        if ui.get_busy() {
            return;
        }
        let (url, username, password) = (
            ui.get_ftp_url().to_string(),
            ui.get_ftp_user().to_string(),
            ui.get_ftp_password().to_string(),
        );
        ui.set_ftp_password("".into());
        ui.set_busy(true);
        ui.set_message(ui.global::<ToolTr>().invoke_working());
        let (weak, manager) = (ui.as_weak(), context.manager.clone());
        std::thread::spawn(move || {
            let result = ftp_directory(&url).and_then(|directory| {
                if !username.is_empty() || !password.is_empty() {
                    manager.set_site_credentials(
                        &directory.origin().ascii_serialization(),
                        &username,
                        &password,
                    )?;
                }
                let names = manager.runtime().block_on(dlcore::protocols::list_ftp(
                    directory.as_str(),
                    &username,
                    &password,
                ))?;
                Ok(ftp_sources(&directory, names))
            });
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_busy(false);
                match result {
                    Ok(links) => {
                        ui.set_links(links.join("\n").into());
                        ui.set_message("".into());
                    }
                    Err(error) => ui.set_message(error.into()),
                }
            });
        });
    });
    {
        let (weak, main_weak, manager) = (ui.as_weak(), main.as_weak(), context.manager.clone());
        ui.on_enqueue(move || {
            let Some(ui) = weak.upgrade() else { return };
            if ui.get_busy() {
                return;
            }
            ui.set_busy(true);
            let (weak, main_weak, manager, links) = (
                ui.as_weak(),
                main_weak.clone(),
                manager.clone(),
                ui.get_links().to_string(),
            );
            std::thread::spawn(move || {
                let result = batch::enqueue(
                    &manager,
                    &links,
                    NewDownload {
                        start: true,
                        ..Default::default()
                    },
                );
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_busy(false);
                    match result {
                        Ok(ids) => {
                            if let (Some(main), Some(id)) = (main_weak.upgrade(), ids.last()) {
                                main.invoke_download_added(*id as i32);
                            }
                            ui.set_message(
                                ui.global::<ToolTr>().invoke_links_added(ids.len() as i32),
                            );
                        }
                        Err(error) => {
                            if let Some(main) = main_weak.upgrade() {
                                ui.set_message(batch::message(&main, error));
                            }
                        }
                    }
                });
            });
        });
    }
    bind!(on_schedule_add, |ui, context| {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            + i64::from(ui.get_delay()) * 60;
        let action = match ui.get_action() {
            1 => Action::PauseAll,
            2 => Action::Run {
                program: ui.get_program().trim().to_owned(),
                args: ui.get_args().lines().map(str::to_owned).collect(),
            },
            3 => Action::Shutdown,
            _ => Action::StartAll,
        };
        run(&ui, context, move |context| {
            let mut settings = context.manager.settings();
            let id = settings
                .scheduled_actions
                .iter()
                .map(|a| a.id)
                .max()
                .unwrap_or(0)
                + 1;
            settings.scheduled_actions.push(ScheduledAction {
                id,
                at,
                action,
                consumed: false,
            });
            context.manager.set_settings(settings)
        });
    });
    bind!(on_schedule_remove, |ui, context| {
        let index = ui.get_action_index();
        run(&ui, context, move |context| {
            let mut settings = context.manager.settings();
            if index >= 0 && (index as usize) < settings.scheduled_actions.len() {
                settings.scheduled_actions.remove(index as usize);
                context.manager.set_settings(settings)?;
            }
            Ok(())
        });
        ui.set_action_index(-1);
    });
    bind!(on_token_generate, |ui, _context| {
        if ui.get_busy() {
            return;
        }
        ui.set_busy(true);
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let result = dlcore::remote::generate_token();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_busy(false);
                match result {
                    Ok(token) => {
                        ui.set_token(token.into());
                        ui.set_message("".into());
                    }
                    Err(error) => ui.set_message(error.into()),
                }
            });
        });
    });
    {
        let (weak, copy_app) = (ui.as_weak(), app.clone());
        ui.on_token_copy(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut app = copy_app.borrow_mut();
            let Some(clipboard) = app.clipboard.as_mut() else {
                ui.set_message(ui.global::<ToolTr>().invoke_clipboard_unavailable());
                return;
            };
            ui.set_message(match clipboard.set_text(ui.get_token().to_string()) {
                Ok(()) => ui.global::<ToolTr>().invoke_token_copied(),
                Err(error) => error.to_string().into(),
            });
        });
    }
    bind!(on_remote_save, |ui, context| {
        let remote = RemoteSettings {
            enabled: ui.get_remote_enabled(),
            bind: ui.get_bind_address().trim().to_owned(),
            token: ui.get_token().to_string(),
        };
        run(&ui, context, move |context| {
            let mut settings = context.manager.settings();
            settings.remote = remote.clone();
            context.manager.set_settings(settings)?;
            restart_remote(context, remote)
        });
    });
    bind!(on_refresh, |ui, context| {
        populate(&ui, context);
        refresh_capabilities(&ui);
    });
    refresh_capabilities(&ui);
    // Startup honours the persisted opt-in, but never turns the server on itself.
    let (weak, background) = (ui.as_weak(), context.clone());
    std::thread::spawn(move || {
        let result = restart_remote(&background, background.manager.settings().remote);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            populate(&ui, &background);
            if let Err(error) = result {
                ui.set_message(error.into());
            }
        });
    });
    Ok(Controller {
        window: ui,
        context,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ftp_listing_names_become_encoded_downloadable_sources() {
        let directory = ftp_directory(" ftp://example.org/releases ").unwrap();
        assert_eq!(directory.as_str(), "ftp://example.org/releases/");
        let sources = ftp_sources(
            &directory,
            vec![
                "payload.bin".into(),
                "space #?%.bin".into(),
                "中文.bin".into(),
                "ftp://other.invalid/filename".into(),
            ],
        );
        assert_eq!(sources[0], "ftp://example.org/releases/payload.bin");
        assert_eq!(
            sources[1],
            "ftp://example.org/releases/space%20%23%3F%25.bin"
        );
        assert_eq!(
            sources[2],
            "ftp://example.org/releases/%E4%B8%AD%E6%96%87.bin"
        );
        assert!(sources[3].starts_with("ftp://example.org/releases/ftp:"));
        assert_eq!(
            ftp_sources(
                &ftp_directory("ftp://example.org/releases/").unwrap(),
                vec!["payload.bin".into()]
            )[0],
            sources[0]
        );
        assert!(ftp_directory("ftp://user:secret@example.org/releases").is_err());
        let state = tempfile::tempdir().unwrap();
        let manager = Manager::new(state.path());
        let ids = batch::enqueue(&manager, &sources.join("\n"), NewDownload::default()).unwrap();
        assert_eq!(ids.len(), sources.len());
        assert_eq!(
            manager
                .snapshots()
                .iter()
                .map(|s| &s.url)
                .collect::<Vec<_>>(),
            sources.iter().collect::<Vec<_>>()
        );
        manager.shutdown();
    }

    #[test]
    fn traffic_indices_keep_custom_and_three_presets_distinct() {
        assert_eq!(mode(0), TrafficMode::Custom);
        assert_eq!(mode(1), TrafficMode::Low);
        assert_eq!(mode(2), TrafficMode::Medium);
        assert_eq!(mode(3), TrafficMode::High);
    }

    #[test]
    fn tools_tabs_expose_typed_controls_and_pinned_close() {
        use i_slint_backend_testing::ElementHandle;
        i_slint_backend_testing::init_no_event_loop();
        crate::apply_lang("en");
        let ui = ToolsWindow::new().unwrap();
        ui.show().unwrap();
        ui.window().set_size(slint::LogicalSize::new(660.0, 480.0));
        for (index, label) in [
            (0, "Apply traffic limits"),
            (1, "Save site profile"),
            (2, "Collect links"),
            (3, "Schedule action"),
            (4, "Apply remote API"),
        ] {
            ui.set_tab(index);
            assert!(
                ElementHandle::find_by_accessible_label(&ui, label)
                    .next()
                    .is_some(),
                "missing {label}"
            );
            let close = ElementHandle::find_by_accessible_label(&ui, "Close")
                .next()
                .unwrap();
            let position = close.absolute_position();
            let size = close.size();
            assert!(position.y >= 0.0 && position.y + size.height <= 480.5);
        }
        assert!(!ui.get_remote_enabled());
        ui.set_busy(true);
        let apply = ElementHandle::find_by_accessible_label(&ui, "Apply remote API")
            .next()
            .unwrap();
        assert_eq!(apply.accessible_enabled(), Some(false));
    }

    #[test]
    #[ignore = "native tools rendering fixture; set DOWNLOADER_UI_SNAPSHOT"]
    fn render_tools_fixture() {
        let output =
            std::env::var_os("DOWNLOADER_UI_SNAPSHOT").expect("explicit screenshot destination");
        crate::select_renderer();
        crate::apply_lang("zh_TW");
        let ui = ToolsWindow::new().unwrap();
        ui.set_tab(
            std::env::var("DOWNLOADER_TOOLS_TAB")
                .unwrap_or_default()
                .parse()
                .unwrap_or(1),
        );
        ui.set_low("64".into());
        ui.set_medium("512".into());
        ui.set_high("0".into());
        ui.set_custom("0".into());
        ui.set_host("https://example.invalid".into());
        ui.set_username("fixture".into());
        ui.set_folder("C:\\fixture\\下載".into());
        ui.set_extensions("zip, 7z, rar".into());
        ui.set_bind_address("127.0.0.1:8787".into());
        ui.set_message("".into());
        ui.show().unwrap();
        ui.window().set_size(slint::LogicalSize::new(780.0, 640.0));
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(800), move || {
            use std::io::Write;
            let ui = weak.upgrade().unwrap();
            let snapshot = ui.window().take_snapshot().unwrap();
            let mut file = std::io::BufWriter::new(std::fs::File::create(output).unwrap());
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
            slint::quit_event_loop().unwrap();
        });
        slint::Timer::single_shot(std::time::Duration::from_secs(30), || {
            let _ = slint::quit_event_loop();
        });
        slint::run_event_loop_until_quit().unwrap();
    }
}
