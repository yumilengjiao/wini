//! Applying computed layout geometry to real windows.
//!
//! The layout engine computes pixel rectangles; this module pushes them
//! to the actual HWNDs with `SetWindowPos`. Geometry does not change Z-order
//! or activation; explicit layering helpers never change keyboard focus.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
        HWND_TOP, IsWindowVisible, SW_HIDE, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowPos, ShowWindow,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

use crate::layout::geometry::TileRect;

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
pub fn set_shown(
        hwnd: HWND,
        shown: bool,
) {
        unsafe {
                if shown {
                        // ShowWindow(SW_SHOWNOACTIVATE) can reorder the revealed
                        // HWND. Its geometry AND tiled band have already been
                        // staged while hidden; preserve both when exposing it.
                        let _ = SetWindowPos(
                                hwnd,
                                None,
                                0,
                                0,
                                0,
                                0,
                                SWP_SHOWWINDOW
                                        | SWP_NOMOVE
                                        | SWP_NOSIZE
                                        | SWP_NOZORDER
                                        | SWP_NOOWNERZORDER
                                        | SWP_NOACTIVATE,
                        );
                } else {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                }
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
// outside the work area on any of the four edges. Native Explorer taskbars
// are recognized by class, without requiring third-party tool-window styles.

// HWNDs found by the `EnumWindows` callback (it takes a raw fn
// pointer, so results go through a module-level thread-local).
thread_local! {
    static BARS_FOUND: std::cell::RefCell<Vec<HWND>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Cached bar list (see [`raise_bars`]): (when enumerated, bars).
    /// Enumerating is cross-process (EnumWindows + GetWindowRect on
    /// every window) and raise_bars runs EVERY ANIMATION FRAME while a
    /// slide or overview animates — the per-call cost is a visible
    /// frame hitch. Refresh at most once a second; invalidate on
    /// display-topology changes.
    static BARS_CACHE:
        std::cell::RefCell<(Option<std::time::Instant>, Vec<HWND>)> =
        const { std::cell::RefCell::new((None, Vec::new())) };
}

/// Drop the cached bar list (monitor topology changed / bars
/// themselves restarted) so the next raise_bars re-enumerates.
pub fn invalidate_bar_cache() {
        BARS_CACHE.with(|c| *c.borrow_mut() = (None, Vec::new()));
}

pub fn desktop_bars(monitors: &[crate::win::monitor::Monitor]) -> Vec<HWND> {
        const CACHE_TTL: std::time::Duration = std::time::Duration::from_millis(1000);
        BARS_CACHE.with(|cell| {
                let mut cache = cell.borrow_mut();
                if cache.0.is_some_and(|t| t.elapsed() < CACHE_TTL) && !cache.1.is_empty() {
                        return cache.1.clone();
                }
                let found = enumerate_bars(monitors);
                *cache = (Some(std::time::Instant::now()), found.clone());
                found
        })
}

pub fn raise_bars(monitors: &[crate::win::monitor::Monitor]) {
        let bars = desktop_bars(monitors);
        log::debug!("raise_bars: re-raising {} bar(s)", bars.len());
        for bar in bars {
                log::debug!(
                        "raise_bars: hwnd={:?} class={}",
                        bar,
                        crate::win::api::window_class(bar)
                );
                unsafe {
                        match SetWindowPos(
                                bar,
                                Some(HWND_TOP),
                                0,
                                0,
                                0,
                                0,
                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                        ) {
                                Ok(()) => {},
                                Err(e) => {
                                        log::warn!("raise_bars: SetWindowPos({bar:?}) failed: {e}")
                                },
                        }
                }
        }
}

fn enumerate_bars(monitors: &[crate::win::monitor::Monitor]) -> Vec<HWND> {
        BARS_FOUND.with(|f| f.borrow_mut().clear());
        // Pass a pointer to the (thin) reference to the slice: a plain
        // `slice as *const _` would be a fat pointer and truncate under
        // `as isize`.
        let mons_ptr: *const &[crate::win::monitor::Monitor] = &monitors;
        unsafe {
                let _ = EnumWindows(Some(enum_bar), LPARAM(mons_ptr as isize));
        }
        BARS_FOUND.with(|f| std::mem::take(&mut *f.borrow_mut()))
}

/// `EnumWindows` callback for [`raise_bars`]: collects bar-like
/// windows (see its doc comment) of OTHER processes into the
/// thread-local FOUND list. `lparam` is `*const Vec<Monitor>`.
unsafe extern "system" fn enum_bar(
        hwnd: HWND,
        lparam: LPARAM,
) -> BOOL {
        unsafe fn check(
                hwnd: HWND,
                lparam: LPARAM,
        ) -> bool {
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
                        // Explorer's native taskbar need not have the
                        // TOOLWINDOW|NOACTIVATE pair used by yasb/zebar.
                        if super::bar_geometry::native_taskbar(&super::api::window_class(hwnd)) {
                                return true;
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
                        // Include vertical bars, but not small tooltips or
                        // windows in a reserved strip on a different monitor.
                        let mons: &[crate::win::monitor::Monitor] =
                                &*(lparam.0 as *const &[crate::win::monitor::Monitor]);
                        mons.iter().any(|m| {
                                super::bar_geometry::in_reserved_strip(rc, m.full, m.work)
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
        apply_geometry_with(rects, SWP_ASYNCWINDOWPOS);
}

/// Synchronous variant for the FINAL frame of a slide settle: the
/// hide of the non-active participants and the reflow that follow
/// must not race a still-queued async move (a window could be hidden
/// before its last move lands, then get repositioned while hidden
/// and briefly re-shown at a stale rect by the reflow — the "flash
/// of the previous workspace" bug). One blocking frame per slide is
/// an acceptable price for the ordering guarantee.
pub fn apply_geometry_sync(rects: &[TileRect]) {
        apply_geometry_with(
                rects,
                windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS(0),
        );
}

fn apply_geometry_with(
        rects: &[TileRect],
        extra: windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS,
) {
        for r in rects {
                let hwnd = HWND(r.id as *mut core::ffi::c_void);
                if !super::api::is_alive(hwnd) {
                        continue;
                }
                let ok = unsafe {
                        let mut current = RECT::default();
                        // Re-applying an unchanged size can still trigger an
                        // application resize/repaint. At the overview handoff
                        // it produced a second repaint after the cover dropped.
                        let unchanged_size = GetWindowRect(hwnd, &mut current).is_ok()
                                && current.right - current.left == r.w
                                && current.bottom - current.top == r.h;
                        let size_flags = if unchanged_size {
                                SWP_NOSIZE
                        } else {
                                windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS(0)
                        };
                        SetWindowPos(
                                hwnd,
                                None,
                                r.x,
                                r.y,
                                r.w,
                                r.h,
                                SWP_NOACTIVATE
                                        | SWP_NOZORDER
                                        | SWP_NOOWNERZORDER
                                        | extra
                                        | size_flags,
                        )
                };
                if ok.is_err() {
                        log::warn!("SetWindowPos failed for window {}", r.id);
                }
        }
}
