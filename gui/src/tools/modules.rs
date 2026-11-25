//! Modules tab of the Tools window: list, approve and revoke add-on modules.
use super::{Context, run};
use crate::{ToolTr, ToolsWindow};
use dlcore::Manager;
use dlcore::modules::ModuleInfo;
use dlcore::task::ConflictPolicy;
use slint::{ComponentHandle, ModelRc, VecModel};

/// 0 built-in, 1 external and not approved, 2 approved, 3 failed to load but approved
/// (can be revoked), 4 failed to load and not approved (nothing to do until it is fixed).
fn state(info: &ModuleInfo) -> i32 {
    if info.error.is_some() {
        if info.approved { 3 } else { 4 }
    } else if info.builtin {
        0
    } else if info.approved {
        2
    } else {
        1
    }
}

pub(super) fn conflict_index(policy: ConflictPolicy) -> i32 {
    match policy {
        ConflictPolicy::Rename => 0,
        ConflictPolicy::Overwrite => 1,
        ConflictPolicy::Skip => 2,
    }
}

fn conflict_policy(index: i32) -> ConflictPolicy {
    match index {
        1 => ConflictPolicy::Overwrite,
        2 => ConflictPolicy::Skip,
        _ => ConflictPolicy::Rename,
    }
}

fn title(info: &ModuleInfo) -> String {
    let kinds = info.kinds.join(", ");
    if kinds.is_empty() {
        format!("{} {}", info.name, info.version)
    } else {
        format!("{} {} · {kinds}", info.name, info.version)
    }
}

pub(super) fn populate(ui: &ToolsWindow, manager: &Manager) {
    let settings = manager.settings();
    ui.set_conflict_index(conflict_index(settings.conflict));
    ui.set_modules_enabled(settings.modules.enabled);
    let modules = manager.modules();
    let tr = ui.global::<ToolTr>();
    let states: Vec<i32> = modules.iter().map(state).collect();
    ui.set_module_names(ModelRc::new(VecModel::from(
        modules.iter().map(|m| title(m).into()).collect::<Vec<_>>(),
    )));
    ui.set_module_status(ModelRc::new(VecModel::from(
        modules
            .iter()
            .zip(&states)
            .map(|(m, s)| tr.invoke_module_status(*s, m.error.clone().unwrap_or_default().into()))
            .collect::<Vec<_>>(),
    )));
    ui.set_module_state(ModelRc::new(VecModel::from(states)));
}

/// Opens the folder with the platform file manager; the path is one argv entry.
fn open_folder() -> Result<(), String> {
    let path = dlcore::modules::modules_dir();
    std::fs::create_dir_all(&path).map_err(|e| format!("Cannot create {}: {e}", path.display()))?;
    #[cfg(windows)]
    let program = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").ok_or("Windows SystemRoot is unavailable")?,
    )
    .join("explorer.exe");
    #[cfg(target_os = "macos")]
    let program = std::path::PathBuf::from("open");
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let program = std::path::PathBuf::from("xdg-open");
    std::process::Command::new(program)
        .arg(&path)
        .spawn()
        .map(|mut child| {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        })
        .map_err(|e| format!("Cannot open {}: {e}", path.display()))
}

pub(super) fn module_id(manager: &Manager, index: i32) -> Option<String> {
    manager
        .modules()
        .get(usize::try_from(index).ok()?)
        .map(|m| m.id.clone())
}

pub(super) fn bind(ui: &ToolsWindow, context: &Context) {
    let (weak, ctx) = (ui.as_weak(), context.clone());
    ui.on_conflict_save(move || {
        let Some(ui) = weak.upgrade() else { return };
        let policy = conflict_policy(ui.get_conflict_index());
        run(&ui, &ctx, move |context| {
            let mut settings = context.manager.settings();
            settings.conflict = policy;
            context.manager.set_settings(settings)
        });
    });
    let (weak, ctx) = (ui.as_weak(), context.clone());
    ui.on_modules_save(move || {
        let Some(ui) = weak.upgrade() else { return };
        let enabled = ui.get_modules_enabled();
        run(&ui, &ctx, move |context| {
            let mut settings = context.manager.settings();
            settings.modules.enabled = enabled;
            context.manager.set_settings(settings)
        });
    });
    let (weak, ctx) = (ui.as_weak(), context.clone());
    ui.on_module_approve(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        run(&ui, &ctx, move |context| {
            let id = module_id(&context.manager, index).ok_or("Module list changed.")?;
            context.manager.approve_module(&id)
        });
    });
    let (weak, ctx) = (ui.as_weak(), context.clone());
    ui.on_module_revoke(move |index| {
        let Some(ui) = weak.upgrade() else { return };
        run(&ui, &ctx, move |context| {
            let id = module_id(&context.manager, index).ok_or("Module list changed.")?;
            context.manager.revoke_module(&id)
        });
    });
    let weak = ui.as_weak();
    ui.on_modules_open_folder(move || {
        let Some(ui) = weak.upgrade() else { return };
        if let Err(error) = open_folder() {
            ui.set_message(error.into());
        }
    });
}
