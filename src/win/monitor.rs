//! Monitor enumeration and geometry.
//!
//! wini lays windows out per-monitor (each output gets its own set of
//! scrollable workspaces later). This module snapshots the current monitor
//! topology using `EnumDisplayMonitors` / `GetMonitorInfoW`.

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HMONITOR, MonitorFromPoint, MonitorFromWindow,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};

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
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.work.left && x < self.work.right && y >= self.work.top && y < self.work.bottom
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
fn snapshot(handle: HMONITOR, full: RECT) -> Option<Monitor> {
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
pub fn monitor_of_window(hwnd: HWND, monitors: &[Monitor]) -> Option<&Monitor> {
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

/// Primary monitor, if enumerated.
pub fn primary(monitors: &[Monitor]) -> Option<&Monitor> {
    monitors.iter().find(|m| m.is_primary)
}
