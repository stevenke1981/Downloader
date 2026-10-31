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
    // Translations can only be selected once a component exists; earlier calls are
    // ignored and Slint keeps the system-locale bundle.
    let ui = ToolsWindow::new().unwrap();
    crate::apply_lang("en");
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
    // Translations can only be selected once a component exists; earlier calls are
    // ignored and Slint keeps the system-locale bundle.
    let ui = ToolsWindow::new().unwrap();
    crate::apply_lang("zh_TW");
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
