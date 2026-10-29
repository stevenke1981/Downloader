//! Presentation state shared by the main and compact windows.
use super::*;

/// List models that live as long as the windows. They are updated in place so
/// ListView rows are only re-created when the list actually changes.
pub(super) struct Models {
    pub(super) rows: Rc<VecModel<Row>>,
    pub(super) compact_rows: Rc<VecModel<Row>>,
    pub(super) counts: Rc<VecModel<i32>>,
}

impl Models {
    pub(super) fn new() -> Self {
        Self {
            rows: Rc::new(VecModel::from(Vec::new())),
            compact_rows: Rc::new(VecModel::from(Vec::new())),
            counts: Rc::new(VecModel::from(vec![0; 10])),
        }
    }
}

impl Models {
    pub(super) fn bind(&self, ui: &AppWindow, compact: &CompactWindow) {
        ui.set_rows(ModelRc::from(self.rows.clone()));
        ui.set_counts(ModelRc::from(self.counts.clone()));
        compact.set_rows(ModelRc::from(self.compact_rows.clone()));
    }
}

/// Makes `model` equal to `items`. Changed rows are replaced in place, surplus rows
/// are removed from the tail and new rows are appended, so unchanged rows are not touched.
pub(super) fn sync_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, items: Vec<T>) {
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
pub(super) fn sync_compact(ui: &AppWindow, compact: &CompactWindow, app: &App, snaps: &[Snapshot]) {
    compact.set_total_speed(ui.get_total_speed());
    compact.set_status_line(ui.get_status_line());
    sync_model(
        &app.models.compact_rows,
        rows_for(snaps.iter(), app.selected),
    );
}

/// Shows the compact window with rows taken now, not left stale until the next timer tick.
pub(super) fn open_compact(ui: &AppWindow, compact: &CompactWindow, app: &RefCell<App>) {
    show_compact(ui, compact);
    let mut a = app.borrow_mut();
    let snaps = a.mgr().snapshots();
    refresh(ui, &mut a, &snaps);
    sync_compact(ui, compact, &a, &snaps);
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

pub(super) fn kbs(bps: u64) -> String {
    (bps / 1024).to_string()
}
pub(super) fn parse_kbs(s: &str) -> Option<u64> {
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
pub(super) fn refresh(ui: &AppWindow, app: &mut App, snaps: &[Snapshot]) {
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
        ui.set_d_completed(s.status == Status::Completed);
        ui.set_d_log(s.log.join("\n").into());
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
        ui.set_d_log("".into());
        ui.set_d_completed(false);
    }
}

/// For callbacks that change state and do not already hold a snapshot.
pub(super) fn refresh_now(ui: &AppWindow, app: &mut App) {
    let snaps = app.mgr().snapshots();
    refresh(ui, app, &snaps);
}
