//! Applying computed layout geometry to real windows.
//!
//! The layout engine computes pixel rectangles; this module pushes them
//! to the actual HWNDs with `SetWindowPos`. We never touch Z-order or
//! activation from here — focus is the OS's (and later the focus
//! module's) business.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowLongPtrW, SetWindowPos, ShowWindow, HWND_TOP, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOOWNERZORDER, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    GWL_STYLE, WINDOW_LONG_PTR_INDEX, WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU,
    WS_THICKFRAME,
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
