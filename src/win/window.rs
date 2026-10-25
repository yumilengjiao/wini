//! Tracking of top-level application windows.
//!
//! wini does not own application windows; it observes them. This
//! module decides *which* HWNDs are worth managing and keeps a snapshot
//! of their basic properties.

use std::collections::BTreeMap;

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetWindow, GetWindowLongW, GetWindowRect,
        GetWindowThreadProcessId, IsIconic, WS_CAPTION, WS_CHILD, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW,
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
        rejection_reason(hwnd).is_none()
}

/// Readiness is rechecked on SHOW, title/foreground changes and periodic
/// discovery. Rejections are not permanent (title/frame may not exist yet).
pub fn rejection_reason(hwnd: HWND) -> Option<&'static str> {
        unsafe {
                if !api::is_alive(hwnd) || !api::is_visible(hwnd) {
                        return Some("not visible/alive");
                }
                if IsIconic(hwnd).as_bool() {
                        return Some("minimized");
                }
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if pid == std::process::id() {
                        return Some("manager-owned window");
                }
                // Reject windows that have an owner (dialogs, popups): we only
                // manage independent top-level windows. Note GetWindow(GW_OWNER)
                // returns Ok(null-HWND) for ownerless windows — reject only when
                // the owner is a real, non-null window.
                if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.0.is_null()) {
                        return Some("owned dialog/popup");
                }
                let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
                // Cloaked windows (suspended UWP apps, virtual desktops): ignore.
                let mut cloaked = 0u32;
                if DwmGetWindowAttribute(
                        hwnd,
                        DWMWA_CLOAKED,
                        &mut cloaked as *mut u32 as *mut core::ffi::c_void,
                        std::mem::size_of::<u32>() as u32,
                )
                .is_ok() && cloaked != 0
                {
                        return Some("cloaked");
                }
                let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
                style_rejection(
                        style,
                        ex,
                        &api::window_class(hwnd),
                        &api::window_title(hwnd),
                )
        }
}

fn style_rejection(
        style: u32,
        ex: u32,
        class: &str,
        title: &str,
) -> Option<&'static str> {
        if style & WS_CHILD.0 != 0 {
                return Some("child control");
        }
        if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
                return Some("tool/no-activate window");
        }
        if IGNORED_CLASSES.contains(&class.to_lowercase().as_str()) {
                return Some("system chrome");
        }
        if style & WS_CAPTION.0 == 0 {
                return Some("frame not ready or borderless popup");
        }
        if title.trim().is_empty() {
                return Some("title not ready");
        }
        None
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
        enumerate_top_level()
                .into_iter()
                .filter(|&hwnd| is_manageable(hwnd))
                .map(snapshot)
                .collect()
}

/// Cheap handle enumeration; callers can skip known windows BEFORE reading
/// titles/styles/minimum sizes. Never unhide or activate a window here.
pub fn enumerate_top_level() -> Vec<HWND> {
        extern "system" fn callback(
                hwnd: HWND,
                lparam: LPARAM,
        ) -> BOOL {
                let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
                list.push(hwnd);
                BOOL(1)
        }
        use windows::core::BOOL;

        let mut hwnds: Vec<HWND> = Vec::new();
        unsafe {
                let _ = EnumWindows(Some(callback), LPARAM(&mut hwnds as *mut _ as isize));
        }
        hwnds
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn zen_live_main_window_is_eligible_without_application_specific_exception() {
                // Read-only probe of Zen's MozillaWindowClass: visible,
                // ownerless, not minimized/cloaked, and these frame styles.
                assert_eq!(
                        style_rejection(0x16CF0000, 0x100, "MozillaWindowClass", "Zen"),
                        None
                );
        }

        #[test]
        fn title_and_frame_readiness_can_be_retried_after_early_show() {
                for class in [
                        "MozillaWindowClass",
                        "Chrome_WidgetWin_1",
                        "ApplicationFrameWindow",
                ] {
                        assert_eq!(
                                style_rejection(WS_CAPTION.0, 0, class, ""),
                                Some("title not ready")
                        );
                        assert_eq!(
                                style_rejection(0, 0, class, "Ready"),
                                Some("frame not ready or borderless popup")
                        );
                        // No cached rejection: NAMECHANGE, foreground or the
                        // next periodic discovery all use this same predicate.
                        for _ in 0..3 {
                                assert_eq!(style_rejection(WS_CAPTION.0, 0, class, "Ready"), None);
                        }
                }
        }

        #[test]
        fn retry_does_not_relax_system_popup_and_child_filters() {
                assert!(style_rejection(WS_CAPTION.0, 0, "Shell_TrayWnd", "taskbar").is_some());
                assert!(style_rejection(WS_CAPTION.0 | WS_CHILD.0, 0, "Edit", "text").is_some());
                assert!(style_rejection(
                        WS_CAPTION.0,
                        WS_EX_TOOLWINDOW.0,
                        "MozillaDropShadowWindowClass",
                        "shadow"
                )
                .is_some());
                assert!(
                        style_rejection(WS_CAPTION.0, WS_EX_NOACTIVATE.0, "overlay", "title")
                                .is_some()
                );
        }
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

        pub fn insert(
                &mut self,
                info: WindowInfo,
        ) {
                self.windows.insert(info.id(), info);
        }

        pub fn remove(
                &mut self,
                hwnd: HWND,
        ) -> Option<WindowInfo> {
                self.windows.remove(&(hwnd.0 as isize))
        }

        pub fn get(
                &self,
                hwnd: HWND,
        ) -> Option<&WindowInfo> {
                self.windows.get(&(hwnd.0 as isize))
        }

        pub fn contains(
                &self,
                hwnd: HWND,
        ) -> bool {
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
