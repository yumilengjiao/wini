//! Applying computed layout geometry to real windows.
//!
//! The layout engine computes pixel rectangles; this module pushes them
//! to the actual HWNDs with `SetWindowPos`. We never touch Z-order or
//! activation from here — focus is the OS's (and later the focus
//! module's) business.

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, HWND_TOP, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOOWNERZORDER, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    GWL_EXSTYLE, GWL_STYLE, WINDOW_LONG_PTR_INDEX, WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
    WS_SYSMENU, WS_THICKFRAME, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

use crate::layout::geometry::TileRect;

/// Decorations to strip for the borderless windowed-fullscreen look.
const FULLSCREEN_STRIP: u32 =
    WS_CAPTION.0 | WS_THICKFRAME.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0;

/// Strip (on) / restore (off) window decorations for the windowed
/// fullscreen mode. `on=false` only re-adds what we previously stripped
/// — callers must only call it for windows they borderlessed.
pub fn set_borderless(hwnd: HWND, on: bool) {
    let style = super::api::get_window_long_ptr(hwnd, GWL_STYLE.0) as u32;
    let new_style = if on {
        style & !FULLSCREEN_STRIP
    } else {
        style | FULLSCREEN_STRIP
    };
    if new_style == style {
        return;
    }
    unsafe {
        let _ = SetWindowLongPtrW(
            hwnd,
            WINDOW_LONG_PTR_INDEX(GWL_STYLE.0),
            new_style as isize,
        );
        // Frame-changed without moving/resizing so the app redraws its
        // frame area immediately.
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// Bring a window to the top of the Z order without moving it (used
/// for the windowed-fullscreen window, which must cover its tile
/// siblings).
pub fn raise(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

/// Show/hide a window (windows on inactive workspaces are not
/// rendered, exactly like niri).
pub fn set_shown(hwnd: HWND, shown: bool) {
    unsafe {
        let _ = ShowWindow(hwnd, if shown { SW_SHOW } else { SW_HIDE });
    }
}

// Re-raise desktop bars (yasb, zebar, ...) above our windows.
// Bars like yasb are plain non-topmost tool windows: they only stay
// visible because tiled windows never enter the bar zone. During a
// workspace-switch slide (and under a windowed-fullscreen window)
// our windows DO cross that zone, and `raise` would put them on top
// of the bar. After any such raise, call this to bring the bars
// back up (niri: the layer-shell bar renders above everything).
//
// A "bar" is a visible window of another process carrying both
// WS_EX_TOOLWINDOW and WS_EX_NOACTIVATE (yasb's recipe; zebar is
// built the same way) whose rect intersects a monitor's strip
// outside the work area (top or bottom taskbar zone).

// HWNDs found by the `EnumWindows` callback (it takes a raw fn
// pointer, so results go through a module-level thread-local).
thread_local! {
    static BARS_FOUND: std::cell::RefCell<Vec<HWND>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub fn raise_bars(monitors: &[crate::win::monitor::Monitor]) {
    BARS_FOUND.with(|f| f.borrow_mut().clear());
    // Pass a pointer to the (thin) reference to the slice: a plain
    // `slice as *const _` would be a fat pointer and truncate under
    // `as isize`.
    let mons_ptr: *const &[crate::win::monitor::Monitor] = &monitors;
    unsafe {
        let _ = EnumWindows(Some(enum_bar), LPARAM(mons_ptr as isize));
        let bars: Vec<HWND> = BARS_FOUND.with(|f| std::mem::take(&mut *f.borrow_mut()));
        log::debug!("raise_bars: re-raising {} bar(s)", bars.len());
        for bar in bars {
            log::debug!(
                "raise_bars: hwnd={:?} class={}",
                bar,
                crate::win::api::window_class(bar)
            );
            match SetWindowPos(
                bar,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            ) {
                Ok(()) => {}
                Err(e) => log::warn!("raise_bars: SetWindowPos({bar:?}) failed: {e}"),
            }
        }
    }
}

/// `EnumWindows` callback for [`raise_bars`]: collects bar-like
/// windows (see its doc comment) of OTHER processes into the
/// thread-local FOUND list. `lparam` is `*const Vec<Monitor>`.
unsafe extern "system" fn enum_bar(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe fn check(hwnd: HWND, lparam: LPARAM) -> bool {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return false;
            }
            // Never touch windows of our own process (focus ring,
            // overlay, ...).
            let mut win_pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut win_pid));
            if win_pid == std::process::id() {
                return false;
            }
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0)
                != (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0)
            {
                return false;
            }
            let mut rc = RECT::default();
            if GetWindowRect(hwnd, &mut rc).is_err() {
                return false;
            }
            // Does it live in a taskbar strip of some monitor — and
            // look like a bar there? A bar spans most of the monitor's
            // width and is short; this filters out tray helpers,
            // tooltips and similar small tool windows in the strip.
            let mons: &[crate::win::monitor::Monitor] =
                &*(lparam.0 as *const &[crate::win::monitor::Monitor]);
            mons.iter().any(|m| {
                let top_strip = (m.full.top, m.work.top);
                let bottom_strip = (m.work.bottom, m.full.bottom);
                let overlaps = |strip: (i32, i32)| {
                    rc.bottom > strip.0 && rc.top < strip.1 && strip.0 < strip.1
                };
                let mon_w = (m.full.right - m.full.left).max(1) as f64;
                let mon_h = (m.full.bottom - m.full.top).max(1) as f64;
                let w = (rc.right - rc.left).max(1) as f64;
                let h = (rc.bottom - rc.top).max(1) as f64;
                (overlaps(top_strip) || overlaps(bottom_strip))
                    && w >= mon_w * 0.5
                    && h <= mon_h * 0.25
            })
        }
    }
    if unsafe { check(hwnd, lparam) } {
        BARS_FOUND.with(|f| f.borrow_mut().push(hwnd));
    }
    BOOL(1)
}

/// Move/resize windows to their computed tiles. Skips dead handles
/// silently (a close race with WinEvents is normal). The move is
/// async (`SWP_ASYNCWINDOWPOS`): without it, SetWindowPos blocks until
/// the target window's own message loop services the move — one busy
/// app (e.g. a terminal under load) stalls the whole animation frame.
pub fn apply_geometry(rects: &[TileRect]) {
    for r in rects {
        let hwnd = HWND(r.id as *mut core::ffi::c_void);
        if !super::api::is_alive(hwnd) {
            continue;
        }
        let ok = unsafe {
            SetWindowPos(
                hwnd,
                None,
                r.x,
                r.y,
                r.w,
                r.h,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
            )
        };
        if ok.is_err() {
            log::warn!("SetWindowPos failed for window {}", r.id);
        }
    }
}
