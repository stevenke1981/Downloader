use super::*;
use crate::actions::{clear_completed, clipboard_links, completed_ids, relative_index};
use crate::view::matches_search;
use std::time::Instant;

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

/// Queues a real download without starting it, so no network is touched.
fn queued(mgr: &Manager, url: &str) -> Snapshot {
    let id = mgr.add(NewDownload {
        url: url.into(),
        start: false,
        ..Default::default()
    });
    mgr.snapshots().into_iter().find(|s| s.id == id).unwrap()
}

#[test]
fn blank_search_matches_everything() {
    let state = tempfile::tempdir().unwrap();
    let mgr = Manager::new(state.path());
    let s = queued(&mgr, "https://example.invalid/alpha.bin");
    assert!(matches_search(&s, ""));
    assert!(matches_search(&s, "   "));
}

#[test]
fn search_is_case_insensitive_on_name_and_url() {
    let state = tempfile::tempdir().unwrap();
    let mgr = Manager::new(state.path());
    let s = queued(&mgr, "https://Example.INVALID/Dir/Alpha-Report.PDF");
    assert!(matches_search(&s, "alpha-report"));
    assert!(matches_search(&s, "ALPHA-REPORT.PDF"));
    assert!(matches_search(&s, "example.invalid"));
    assert!(matches_search(&s, "dir/alpha"));
    assert!(!matches_search(&s, "beta"));
}

#[test]
fn search_ignores_surrounding_whitespace_and_matches_non_ascii_names() {
    let state = tempfile::tempdir().unwrap();
    let mgr = Manager::new(state.path());
    let s = queued(&mgr, "https://example.invalid/%E6%B8%AC%E8%A9%A6.bin");
    assert!(matches_search(&s, "  example.invalid  "));
    let name = s.name.clone();
    assert!(!name.is_empty());
    assert!(matches_search(&s, &format!("  {}\t", name.to_uppercase())));
    assert!(!matches_search(&s, "not present"));
}

#[test]
fn search_filters_list_rows_but_not_sidebar_counts() {
    i_slint_backend_testing::init_no_event_loop();
    let state = tempfile::tempdir().unwrap();
    let mgr = Manager::new(state.path());
    for url in [
        "https://example.invalid/alpha.bin",
        "https://mirror.invalid/beta/data.zip",
        "https://example.invalid/gamma.iso",
    ] {
        mgr.add(NewDownload {
            url: url.into(),
            start: false,
            ..Default::default()
        });
    }
    let app = Rc::new(RefCell::new(App {
        mgr: Some(mgr),
        filter: 0,
        selected: None,
        visible: vec![],
        search: String::new(),
        detail_for: None,
        last_clip: String::new(),
        clipboard: None,
        models: Models::new(),
    }));
    let ui = AppWindow::new().unwrap();
    let compact = CompactWindow::new().unwrap();
    app.borrow().models.bind(&ui, &compact);
    let rows_for = |search: &str, filter: i32| {
        let mut a = app.borrow_mut();
        a.search = search.into();
        a.filter = filter;
        refresh_now(&ui, &mut a);
        (a.models.rows.row_count(), a.models.counts.row_data(0))
    };
    assert_eq!(rows_for("", 0), (3, Some(3)));
    assert_eq!(rows_for("alpha", 0), (1, Some(3)));
    assert_eq!(rows_for("example.invalid", 0), (2, Some(3)));
    assert_eq!(rows_for("data.zip", 0), (1, Some(3)));
    assert_eq!(rows_for("nothing matches", 0), (0, Some(3)));
    // Search and the status filter combine. Downloads added with start: false are
    // paused, so neither the completed nor the active filter shows them.
    assert_eq!(rows_for("alpha", 2), (0, Some(3)));
    assert_eq!(rows_for("alpha", 1), (0, Some(3)));
}

#[test]
fn relative_index_moves_and_clamps_inside_the_list() {
    assert_eq!(relative_index(Some(0), 3, 1), Some(1));
    assert_eq!(relative_index(Some(1), 3, 1), Some(2));
    assert_eq!(relative_index(Some(2), 3, 1), Some(2), "clamped at the end");
    assert_eq!(relative_index(Some(2), 3, -1), Some(1));
    assert_eq!(
        relative_index(Some(0), 3, -1),
        Some(0),
        "clamped at the start"
    );
    assert_eq!(relative_index(Some(1), 3, 10), Some(2));
    assert_eq!(relative_index(Some(1), 3, -10), Some(0));
}

#[test]
fn relative_index_without_selection_starts_at_the_nearest_edge() {
    assert_eq!(relative_index(None, 4, 1), Some(0));
    assert_eq!(relative_index(None, 4, -1), Some(3));
    assert_eq!(relative_index(None, 1, -1), Some(0));
}

#[test]
fn relative_index_is_none_when_nothing_can_change() {
    assert_eq!(relative_index(None, 0, 1), None, "empty list");
    assert_eq!(relative_index(Some(0), 0, -1), None, "empty list");
    assert_eq!(relative_index(Some(1), 3, 0), None, "zero step");
    assert_eq!(relative_index(None, 3, 0), None, "zero step");
}

#[test]
fn clipboard_links_keeps_trimmed_sources_and_drops_other_text() {
    let text = "  https://a.example/one.bin \r\nplain words here\n\nmagnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567\n\thttp://b.example/two\n";
    assert_eq!(
        clipboard_links(text),
        "https://a.example/one.bin\nmagnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567\nhttp://b.example/two"
    );
}

#[test]
fn clipboard_links_is_empty_without_any_source() {
    assert_eq!(clipboard_links(""), "");
    assert_eq!(clipboard_links("just some text\nand more"), "");
    assert_eq!(clipboard_links("ftp://"), "", "scheme without a host");
}

#[test]
fn clipboard_links_is_capped_at_the_batch_limit() {
    let text: String = (0..1500)
        .map(|i| format!("https://example.invalid/{i}.bin\n"))
        .collect();
    let links = clipboard_links(&text);
    assert_eq!(links.lines().count(), 1000);
    assert!(links.starts_with("https://example.invalid/0.bin\n"));
    assert!(links.ends_with("https://example.invalid/999.bin"));
}

#[test]
fn completed_ids_selects_only_finished_downloads_in_order() {
    let state = tempfile::tempdir().unwrap();
    let mgr = Manager::new(state.path());
    let mut done = queued(&mgr, "https://example.invalid/done.bin");
    let waiting = queued(&mgr, "https://example.invalid/waiting.bin");
    done.status = Status::Completed;
    let mut failed = waiting.clone();
    failed.id = waiting.id + 100;
    failed.status = Status::Error;
    assert_eq!(
        completed_ids(&[waiting.clone(), done.clone(), failed]),
        vec![done.id]
    );
}

#[test]
fn clear_completed_removes_finished_rows_and_keeps_their_files() {
    i_slint_backend_testing::init_no_event_loop();
    let state = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let mgr = Manager::new(state.path());
    let mut settings = mgr.settings();
    settings.download_dir = downloads.path().into();
    settings.auto_verify = false;
    mgr.set_settings(settings).unwrap();
    let bytes = fixture_server::test_data(64 * 1024);
    let server = mgr
        .runtime()
        .block_on(fixture_server::start(bytes.clone(), true, 1024 * 1024));
    let done = mgr.add(NewDownload {
        url: server.url("kept.bin"),
        start: true,
        ..Default::default()
    });
    let waiting = mgr.add(NewDownload {
        url: "https://example.invalid/waiting.bin".into(),
        start: false,
        ..Default::default()
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    while mgr
        .snapshots()
        .iter()
        .find(|s| s.id == done)
        .is_none_or(|s| s.status != Status::Completed)
    {
        assert!(Instant::now() < deadline, "fixture download timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut app = App {
        mgr: Some(mgr),
        filter: 0,
        selected: Some(done),
        visible: vec![done, waiting],
        search: String::new(),
        detail_for: None,
        last_clip: String::new(),
        clipboard: None,
        models: Models::new(),
    };
    app.visible = vec![waiting];
    assert_eq!(clear_completed(&mut app), 0, "hidden completed rows stay");
    app.visible = vec![done, waiting];
    assert_eq!(clear_completed(&mut app), 1);
    let remaining: Vec<u64> = app.mgr().snapshots().iter().map(|s| s.id).collect();
    assert_eq!(remaining, vec![waiting]);
    assert_eq!(app.selected, None, "the removed row was selected");
    assert_eq!(
        std::fs::read(downloads.path().join("kept.bin")).unwrap(),
        bytes,
        "clear-completed must not delete the file"
    );
    app.mgr.take().unwrap().shutdown();
}
