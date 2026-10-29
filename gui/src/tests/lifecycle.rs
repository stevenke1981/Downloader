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
fn batch_input_preserves_lines_and_disables_submission_while_working() {
    use i_slint_backend_testing::ElementHandle;
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().unwrap();
    apply_lang("en");
    ui.set_add_open(true);
    ui.set_add_url("https://example.org/a\nhttps://example.org/b".into());
    ui.set_add_kind(1);
    ui.show().unwrap();
    assert!(ui.get_add_url().contains('\n'));
    for label in ["Download URLs", "Paste links", "Import torrent / Metalink…"] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, label)
                .next()
                .is_some(),
            "missing {label}"
        );
    }
    assert_eq!(ui.get_add_kind(), 1);
    ui.set_add_busy(true);
    let paste = ElementHandle::find_by_accessible_label(&ui, "Paste links")
        .next()
        .unwrap();
    assert_eq!(paste.accessible_enabled(), Some(false));
    let working = ElementHandle::find_by_accessible_label(&ui, "Working…")
        .next()
        .unwrap();
    assert_eq!(working.accessible_enabled(), Some(false));
    ui.set_add_busy(false);
    apply_lang("zh_TW");
    for label in ["貼上連結", "匯入 torrent／Metalink…"] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, label)
                .next()
                .is_some(),
            "missing Traditional Chinese control {label}"
        );
    }
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
                    p.x >= 28.0 && p.y >= 16.0 && p.x + s.width <= 732.5 && p.y + s.height <= 464.5,
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
    app.borrow().models.bind(&ui, &compact);
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
