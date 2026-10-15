//! Keep the initial Windows frame inside the monitor's usable work area.
//! Layout dimensions are logical; all calculations here use physical pixels.

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[cfg(any(windows, test))]
fn fit(frame: Rect, work: Rect) -> Rect {
    let width = frame.width.min(work.width);
    let height = frame.height.min(work.height);
    Rect {
        x: frame.x.clamp(work.x, work.x + (work.width - width) as i32),
        y: frame
            .y
            .clamp(work.y, work.y + (work.height - height) as i32),
        width,
        height,
    }
}

#[cfg(windows)]
pub fn fit_initial_window(window: &slint::Window) {
    use slint::winit_030::{
        WinitWindowAccessor,
        winit::{
            dpi::{PhysicalPosition, PhysicalSize},
            raw_window_handle::{HasWindowHandle, RawWindowHandle},
        },
    };
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        },
    };

    window.with_winit_window(|w| {
        let Ok(handle) = w.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let mut monitor = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // The borrowed HWND belongs to this live winit window. MONITORINFO is initialized
        // with its required size, and no pointer escapes this synchronous call.
        let ok = unsafe {
            let monitor_handle =
                MonitorFromWindow(HWND(handle.hwnd.get() as *mut _), MONITOR_DEFAULTTONEAREST);
            GetMonitorInfoW(monitor_handle, &mut monitor).as_bool()
        };
        let Ok(position) = w.outer_position() else {
            return;
        };
        if !ok {
            return;
        }
        let outer = w.outer_size();
        let inner = w.inner_size();
        let work = monitor.rcWork;
        if work.right <= work.left || work.bottom <= work.top {
            return;
        }
        let bounds = Rect {
            x: work.left,
            y: work.top,
            width: (work.right - work.left) as u32,
            height: (work.bottom - work.top) as u32,
        };
        let target = fit(
            Rect {
                x: position.x,
                y: position.y,
                width: outer.width,
                height: outer.height,
            },
            bounds,
        );
        if target.width != outer.width || target.height != outer.height {
            // Account for the title bar and resize borders instead of treating the
            // available desktop as client size. The OS still enforces Slint's minimum.
            let _ = w.request_inner_size(PhysicalSize::new(
                target
                    .width
                    .saturating_sub(outer.width.saturating_sub(inner.width))
                    .max(1),
                target
                    .height
                    .saturating_sub(outer.height.saturating_sub(inner.height))
                    .max(1),
            ));
        }
        w.set_outer_position(PhysicalPosition::new(target.x, target.y));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_frame_is_reachable_on_1024_desktop_with_taskbar() {
        let result = fit(
            Rect {
                x: 137,
                y: 130,
                width: 976,
                height: 679,
            },
            Rect {
                x: 0,
                y: 0,
                width: 1024,
                height: 728,
            },
        );
        assert_eq!(
            result,
            Rect {
                x: 48,
                y: 49,
                width: 976,
                height: 679
            }
        );
    }

    #[test]
    fn oversized_frame_and_negative_monitor_coordinates_are_bounded() {
        let work = Rect {
            x: -1280,
            y: 40,
            width: 1280,
            height: 984,
        };
        let result = fit(
            Rect {
                x: -100,
                y: -50,
                width: 1700,
                height: 1100,
            },
            work,
        );
        assert_eq!(result, work);
        let inside = Rect {
            x: -1200,
            y: 80,
            width: 900,
            height: 700,
        };
        assert_eq!(fit(inside, work), inside);
    }
}
