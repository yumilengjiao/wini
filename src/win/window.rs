//! Tracking of top-level application windows.
//!
//! yumi-wini does not own application windows; it observes them. This
//! module decides *which* HWNDs are worth managing and keeps a snapshot
//! of their basic properties.

use std::collections::BTreeMap;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowLongW, GetWindowRect, GWL_EXSTYLE, GWL_STYLE, GW_OWNER,
    WS_CAPTION, WS_EX_TOOLWINDOW,
};

use super::api;

/// Classes of system chrome that must never be managed.
const IGNORED_CLASSES: &[&str] = &[
    "shell_traywnd",
    "shell_secondarytraywnd",
    "progman",
    "workerw",
    "windows.ui.composition.desktopwindowcontentbridge",
    "windows.ui.core.corewindow",
    "applicationframeinputsinkwindow",
    "msctls ime",
    "default ime",
    "tooltips_class32",
];

/// A snapshot of a top-level window we track.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub hwnd: HWND,
    pub title: String,
    pub class: String,
    pub exe: String,
    pub rect: RECT,
    /// The window's minimum size (WM_GETMINMAXINFO ptMinTrackSize),
    /// (0, 0) when it doesn't care. Apps like Windows Terminal silently
    /// clamp SetWindowPos to this, so the layout must respect it or
    /// columns visually overlap.
    pub min_size: (f64, f64),
}

impl WindowInfo {
    /// Numeric identity usable as a map key / in logs.
    pub fn id(&self) -> isize {
        self.hwnd.0 as isize
    }
}

/// Should this HWND be managed by the layout engine?
///
/// Heuristics (standard for WM-on-Windows layers):
/// - must be a visible top-level window without an owner,
/// - must not be a tool window / cloaked (UWP ghost) / system chrome,
/// - must have a non-empty title (filters transient junk popups).
pub fn is_manageable(hwnd: HWND) -> bool {
    unsafe {
        if !api::is_visible(hwnd) {
            return false;
        }
        // Reject windows that have an owner (dialogs, popups): we only
        // manage independent top-level windows. Note GetWindow(GW_OWNER)
        // returns Ok(null-HWND) for ownerless windows — reject only when
        // the owner is a real, non-null window.
        if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.0.is_null()) {
            return false;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        // Cloaked windows (suspended UWP apps, virtual desktops): ignore.
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return false;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if style & WS_CAPTION.0 == 0 {
            // Only manage "normal" app windows with a title bar frame.
            // This keeps console borderless popups and splashes out of the
            // layout until a window-rule system exists.
            return false;
        }
        if api::window_title(hwnd).trim().is_empty() {
            return false;
        }
        let class = api::window_class(hwnd);
        if IGNORED_CLASSES.contains(&class.to_lowercase().as_str()) {
            return false;
        }
        true
    }
}

/// Snapshot a single window's properties.
pub fn snapshot(hwnd: HWND) -> WindowInfo {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rect);
    }
    WindowInfo {
        hwnd,
        title: api::window_title(hwnd),
        class: api::window_class(hwnd),
        exe: api::window_exe(hwnd),
        rect,
        min_size: api::min_track_size(hwnd),
    }
}

/// Enumerate all currently manageable top-level windows, in top-down
/// Z order (topmost first).
pub fn enumerate_manageable() -> Vec<WindowInfo> {
    extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
        if is_manageable(hwnd) {
            list.push(hwnd);
        }
        BOOL(1)
    }
    use windows::core::BOOL;

    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(
            Some(callback),
            LPARAM(&mut hwnds as *mut _ as isize),
        );
    }
    hwnds.into_iter().map(snapshot).collect()
}

/// Registry of tracked windows, keyed by HWND value.
#[derive(Debug, Default)]
pub struct WindowRegistry {
    windows: BTreeMap<isize, WindowInfo>,
}

impl WindowRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adopt every manageable window currently on screen.
    pub fn adopt_existing(&mut self) -> usize {
        for info in enumerate_manageable() {
            log::info!(
                "tracking window {:?} [{}] \"{}\" ({})",
                info.id(),
                info.exe,
                info.title,
                info.class
            );
            self.windows.insert(info.id(), info);
        }
        self.windows.len()
    }

    pub fn insert(&mut self, info: WindowInfo) {
        self.windows.insert(info.id(), info);
    }

    pub fn remove(&mut self, hwnd: HWND) -> Option<WindowInfo> {
        self.windows.remove(&(hwnd.0 as isize))
    }

    pub fn get(&self, hwnd: HWND) -> Option<&WindowInfo> {
        self.windows.get(&(hwnd.0 as isize))
    }

    pub fn contains(&self, hwnd: HWND) -> bool {
        self.windows.contains_key(&(hwnd.0 as isize))
    }

    pub fn iter(&self) -> impl Iterator<Item = &WindowInfo> {
        self.windows.values()
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}
