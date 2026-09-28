//! Monitor enumeration and geometry.
//!
//! wini lays windows out per-monitor (each output gets its own set of
//! scrollable workspaces later). This module snapshots the current monitor
//! topology using `EnumDisplayMonitors` / `GetMonitorInfoW`.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW,
        MonitorFromPoint, MonitorFromWindow,
};
use windows::core::BOOL;

/// One physical/virtual output we can lay windows out on.
#[derive(Debug, Clone)]
pub struct Monitor {
        /// Win32 monitor handle; can change after mode changes / replug.
        pub handle: HMONITOR,
        /// Full pixel rectangle (includes taskbar area).
        pub full: RECT,
        /// Working area (taskbar excluded).
        pub work: RECT,
        /// Device adapter name, e.g. `\\.\DISPLAY1`.
        pub device: String,
        /// True for the primary monitor.
        pub is_primary: bool,
}

impl Monitor {
        pub fn width(&self) -> i32 {
                self.work.right - self.work.left
        }

        pub fn height(&self) -> i32 {
                self.work.bottom - self.work.top
        }

        /// Top-left corner of the working area in virtual-screen coordinates.
        pub fn origin(&self) -> (i32, i32) {
                (self.work.left, self.work.top)
        }

        /// Does the working area contain this point (virtual-screen coords)?
        pub fn contains(
                &self,
                x: i32,
                y: i32,
        ) -> bool {
                x >= self.work.left
                        && x < self.work.right
                        && y >= self.work.top
                        && y < self.work.bottom
        }
}

/// Enumerate all monitors, in the order Windows reports them.
pub fn enumerate() -> Vec<Monitor> {
        extern "system" fn callback(
                hmonitor: HMONITOR,
                _hdc: windows::Win32::Graphics::Gdi::HDC,
                rect: *mut RECT,
                lparam: LPARAM,
        ) -> BOOL {
                let monitors = unsafe { &mut *(lparam.0 as *mut Vec<Monitor>) };
                if let Some(m) = snapshot(hmonitor, unsafe { *rect }) {
                        monitors.push(m);
                }
                BOOL(1)
        }

        let mut monitors: Vec<Monitor> = Vec::new();
        unsafe {
                let _ = EnumDisplayMonitors(
                        None,
                        None,
                        Some(callback),
                        LPARAM(&mut monitors as *mut _ as isize),
                );
        }
        monitors
}

/// Read details for one monitor handle.
fn snapshot(
        handle: HMONITOR,
        full: RECT,
) -> Option<Monitor> {
        const MONITORINFOF_PRIMARY: u32 = 1;

        unsafe {
                let mut info = MONITORINFOEXW {
                        monitorInfo: windows::Win32::Graphics::Gdi::MONITORINFO {
                                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                                ..Default::default()
                        },
                        ..Default::default()
                };
                if !GetMonitorInfoW(handle, &mut info.monitorInfo).as_bool() {
                        log::warn!("GetMonitorInfoW failed for monitor {handle:?}");
                        return None;
                }
                let device_len = info
                        .szDevice
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(info.szDevice.len());
                Some(Monitor {
                        handle,
                        full,
                        work: info.monitorInfo.rcWork,
                        device: String::from_utf16_lossy(&info.szDevice[..device_len]),
                        is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                })
        }
}

/// Which monitor does this window currently live on? (nearest, if the
/// window straddles several.)
pub fn monitor_of_window(
        hwnd: HWND,
        monitors: &[Monitor],
) -> Option<&Monitor> {
        let handle = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        monitors.iter().find(|m| m.handle == handle)
}

/// The monitor containing the cursor. Used for deciding where new
/// windows / focus go.
pub fn monitor_at_cursor(monitors: &[Monitor]) -> Option<&Monitor> {
        let mut pt = POINT::default();
        unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
        }
        let handle = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
        monitors.iter().find(|m| m.handle == handle)
}

/// A compass direction for monitor navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
        Left,
        Right,
        Up,
        Down,
}

/// The monitor adjacent to `current` in direction `dir`, if any.
/// Chooses among monitors whose center lies on the correct side by the
/// smallest center-to-center distance (niri only moves between directly
/// adjacent outputs; nearest-center is a good Windows-side approximation).
pub fn monitor_in_direction<'a>(
        monitors: &'a [Monitor],
        current: &Monitor,
        dir: Dir,
) -> Option<&'a Monitor> {
        let cx = (current.full.left + current.full.right) as f64 / 2.0;
        let cy = (current.full.top + current.full.bottom) as f64 / 2.0;
        monitors.iter()
                .filter(|m| m.device != current.device)
                .filter_map(|m| {
                        let mx = (m.full.left + m.full.right) as f64 / 2.0;
                        let my = (m.full.top + m.full.bottom) as f64 / 2.0;
                        let (dx, dy) = (mx - cx, my - cy);
                        let ok = match dir {
                                Dir::Left => dx < -1.0 && dx.abs() >= dy.abs(),
                                Dir::Right => dx > 1.0 && dx.abs() >= dy.abs(),
                                Dir::Up => dy < -1.0 && dy.abs() >= dx.abs(),
                                Dir::Down => dy > 1.0 && dy.abs() >= dx.abs(),
                        };
                        ok.then_some((m, dx * dx + dy * dy))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(m, _)| m)
}

/// Primary monitor, if enumerated.
pub fn primary(monitors: &[Monitor]) -> Option<&Monitor> {
        monitors.iter().find(|m| m.is_primary)
}

#[cfg(test)]
mod tests {
        use super::*;
        use windows::Win32::Graphics::Gdi::HMONITOR;

        fn mon(
                device: &str,
                l: i32,
                t: i32,
                r: i32,
                b: i32,
        ) -> Monitor {
                Monitor {
                        handle: HMONITOR(std::ptr::null_mut()),
                        full: RECT {
                                left: l,
                                top: t,
                                right: r,
                                bottom: b,
                        },
                        work: RECT {
                                left: l,
                                top: t,
                                right: r,
                                bottom: b,
                        },
                        device: device.to_string(),
                        is_primary: false,
                }
        }

        #[test]
        fn direction_picks_adjacent_output() {
                // Three monitors in a row: A | B | C.
                let a = mon("A", 0, 0, 1000, 1000);
                let b = mon("B", 1000, 0, 2000, 1000);
                let c = mon("C", 2000, 0, 3000, 1000);
                let all = vec![a.clone(), b.clone(), c.clone()];
                // From B: right = C, left = A.
                assert_eq!(
                        monitor_in_direction(&all, &b, Dir::Right).map(|m| m.device.as_str()),
                        Some("C")
                );
                assert_eq!(
                        monitor_in_direction(&all, &b, Dir::Left).map(|m| m.device.as_str()),
                        Some("A")
                );
                // No monitor above/below.
                assert!(monitor_in_direction(&all, &b, Dir::Up).is_none());
                // From A: nothing to the left.
                assert!(monitor_in_direction(&all, &a, Dir::Left).is_none());
                // From A: right picks the NEAREST (B, not C).
                assert_eq!(
                        monitor_in_direction(&all, &a, Dir::Right).map(|m| m.device.as_str()),
                        Some("B")
                );
        }

        #[test]
        fn direction_vertical_stack() {
                let top = mon("T", 0, 0, 1000, 1000);
                let bot = mon("B", 0, 1000, 1000, 2000);
                let all = vec![top.clone(), bot.clone()];
                assert_eq!(
                        monitor_in_direction(&all, &top, Dir::Down).map(|m| m.device.as_str()),
                        Some("B")
                );
                assert_eq!(
                        monitor_in_direction(&all, &bot, Dir::Up).map(|m| m.device.as_str()),
                        Some("T")
                );
                assert!(monitor_in_direction(&all, &top, Dir::Right).is_none());
        }
}
