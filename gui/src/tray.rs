//! The tray is owned by the UI thread and uses its existing native event loop.
// Only the Windows tray produces actions; elsewhere `actions()` is always empty.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy)]
pub enum Action {
    Restore,
    Compact,
    Exit,
}

#[cfg(windows)]
pub struct Tray {
    icon: tray_icon::TrayIcon,
    status: tray_icon::menu::MenuItem,
    restore: tray_icon::menu::MenuItem,
    compact: tray_icon::menu::MenuItem,
    exit: tray_icon::menu::MenuItem,
}

#[cfg(windows)]
impl Tray {
    pub fn new() -> Result<Self, String> {
        use tray_icon::{
            Icon, TrayIconBuilder,
            menu::{Menu, MenuItem},
        };
        let menu = Menu::new();
        let status = MenuItem::new("Downloader", false, None);
        let restore = MenuItem::new("還原主視窗 / Restore", true, None);
        let compact = MenuItem::new("精簡視窗 / Compact", true, None);
        let exit = MenuItem::new("退出 / Exit", true, None);
        menu.append_items(&[&status, &restore, &compact, &exit])
            .map_err(|e| e.to_string())?;
        // Small opaque blue tile with a white download arrow; no external runtime asset.
        let mut rgba = Vec::with_capacity(32 * 32 * 4);
        for y in 0..32 {
            for x in 0i32..32 {
                let arrow = (13..19).contains(&x) && (5..21).contains(&y)
                    || (16..25).contains(&y) && (x - 16).abs() <= 24 - y
                    || (7..25).contains(&x) && (26..29).contains(&y);
                rgba.extend_from_slice(if arrow {
                    &[255, 255, 255, 255]
                } else {
                    &[21, 101, 192, 255]
                });
            }
        }
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(Icon::from_rgba(rgba, 32, 32).map_err(|e| e.to_string())?)
            .with_tooltip("Downloader")
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            icon,
            status,
            restore,
            compact,
            exit,
        })
    }

    pub fn update(&self, text: &str) {
        self.status.set_text(text);
        let _ = self.icon.set_tooltip(Some(text));
    }

    pub fn available(&self) -> bool {
        self.icon.rect().is_some()
    }

    pub fn actions(&self) -> Vec<Action> {
        use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent, menu::MenuEvent};
        let mut actions = Vec::new();
        while let Ok(e) = MenuEvent::receiver().try_recv() {
            if e.id == *self.restore.id() {
                actions.push(Action::Restore);
            }
            if e.id == *self.compact.id() {
                actions.push(Action::Compact);
            }
            if e.id == *self.exit.id() {
                actions.push(Action::Exit);
            }
        }
        while let Ok(e) = TrayIconEvent::receiver().try_recv() {
            if matches!(
                e,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                actions.push(Action::Restore);
            }
        }
        actions
    }
}

#[cfg(not(windows))]
pub struct Tray;
#[cfg(not(windows))]
impl Tray {
    pub fn new() -> Result<Self, String> {
        Err("System tray is currently supported on Windows.".into())
    }
    pub fn update(&self, _: &str) {}
    pub fn available(&self) -> bool {
        false
    }
    pub fn actions(&self) -> Vec<Action> {
        Vec::new()
    }
}
