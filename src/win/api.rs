//! Small helpers around the `windows` crate to keep call sites readable.

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::WINDOW_LONG_PTR_INDEX;
use windows::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetClassNameW, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
        GetWindowTextLengthW, GetWindowTextW, IsWindow, IsWindowVisible, WindowFromPoint,
};
use windows::core::PWSTR;

/// Window rect as (x, y, w, h) in screen coordinates.
pub fn window_rect(hwnd: HWND) -> Option<(f64, f64, f64, f64)> {
        let mut rc = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rc).ok()? };
        Some((
                rc.left as f64,
                rc.top as f64,
                (rc.right - rc.left) as f64,
                (rc.bottom - rc.top) as f64,
        ))
}

/// Visible window bounds (DWM extended frame) as (x, y, w, h) in
/// screen coordinates. Unlike `window_rect`, this excludes the
/// invisible resize borders that modern framed windows reserve inside
/// their window rect (~7-10px on the sides/bottom), so anything that
/// must hug the *visible* edge of a window — the focus ring — should
/// use this. Falls back to the raw window rect if DWM is unavailable.
pub fn frame_rect(hwnd: HWND) -> Option<(f64, f64, f64, f64)> {
        use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
        let mut rc = RECT::default();
        let ok = unsafe {
                DwmGetWindowAttribute(
                        hwnd,
                        DWMWA_EXTENDED_FRAME_BOUNDS,
                        &mut rc as *mut RECT as *mut core::ffi::c_void,
                        std::mem::size_of::<RECT>() as u32,
                )
                .is_ok()
        };
        if ok {
                Some((
                        rc.left as f64,
                        rc.top as f64,
                        (rc.right - rc.left) as f64,
                        (rc.bottom - rc.top) as f64,
                ))
        } else {
                window_rect(hwnd)
        }
}

/// Visible-frame margins measured per window, rather than a fixed border
/// thickness which varies with DPI, decorations and Windows versions.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameInsets {
        left: f64,
        top: f64,
        right: f64,
        bottom: f64,
}

impl FrameInsets {
        fn between(
                win: (f64, f64, f64, f64),
                frame: (f64, f64, f64, f64),
        ) -> Self {
                let clamp = |v: f64, size: f64| v.clamp(0.0, (size / 2.0 - 0.5).max(0.0));
                Self {
                        left: clamp(frame.0 - win.0, win.2),
                        top: clamp(frame.1 - win.1, win.3),
                        right: clamp(win.0 + win.2 - frame.0 - frame.2, win.2),
                        bottom: clamp(win.1 + win.3 - frame.1 - frame.3, win.3),
                }
        }

        /// Convert an outer HWND/thumbnail rectangle to its visible edge.
        /// Overview margins scale with zoom; HWND margins stay physical.
        pub fn apply(
                self,
                rect: (f64, f64, f64, f64),
                scale: f64,
        ) -> (i32, i32, i32, i32) {
                let (x, y, w, h) = rect;
                let (l, t, r, b) = (
                        (x + self.left * scale).round() as i32,
                        (y + self.top * scale).round() as i32,
                        (x + w - self.right * scale).round() as i32,
                        (y + h - self.bottom * scale).round() as i32,
                );
                (l, t, (r - l).max(1), (b - t).max(1))
        }
}

pub fn frame_insets(hwnd: HWND) -> FrameInsets {
        match (window_rect(hwnd), frame_rect(hwnd)) {
                (Some(win), Some(frame)) => FrameInsets::between(win, frame),
                _ => FrameInsets::default(),
        }
}

#[cfg(test)]
mod frame_tests {
        use super::*;
        #[test]
        fn visible_bounds_agree_at_thumbnail_handoff() {
                let margins = FrameInsets::between(
                        (-1000.0, 48.0, 1000.0, 752.0),
                        (-993.0, 48.0, 986.0, 745.0),
                );
                assert_eq!(
                        margins.apply((-1000.0, 48.0, 1000.0, 752.0), 1.0),
                        (-993, 48, 986, 745)
                );
                assert_eq!(
                        margins.apply((100.0, 100.0, 500.0, 376.0), 0.5),
                        (104, 100, 493, 373)
                );
        }
        #[test]
        fn custom_decorations_do_not_expand_the_ring() {
                let margins = FrameInsets::between(
                        (0.0, 0.0, 800.0, 600.0),
                        (-10.0, -10.0, 820.0, 620.0),
                );
                assert_eq!(
                        margins.apply((0.0, 0.0, 800.0, 600.0), 1.0),
                        (0, 0, 800, 600)
                );
        }
}

/// Current cursor position in screen coordinates.
pub fn cursor_pos() -> Option<(f64, f64)> {
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt).ok()? };
        Some((pt.x as f64, pt.y as f64))
}

/// The top-level (root) window at a screen point, if any. Hits on
/// child controls are resolved up to their root owner.
pub fn root_window_at(
        x: i32,
        y: i32,
) -> Option<HWND> {
        unsafe {
                let hwnd = WindowFromPoint(POINT { x, y });
                if hwnd.0.is_null() {
                        return None;
                }
                let root = GetAncestor(hwnd, GA_ROOT);
                if root.0.is_null() { None } else { Some(root) }
        }
}

/// Convert the last Win32 error into a readable string.
pub fn last_error(context: &str) -> String {
        let code = unsafe { windows::Win32::Foundation::GetLastError() };
        format!("{context} failed: Win32 error {}", code.0)
}

/// Read a window's title (caption).
pub fn window_title(hwnd: HWND) -> String {
        unsafe {
                let len = GetWindowTextLengthW(hwnd);
                if len <= 0 {
                        return String::new();
                }
                let mut buf = vec![0u16; len as usize + 1];
                let copied = GetWindowTextW(hwnd, &mut buf);
                String::from_utf16_lossy(&buf[..copied.max(0) as usize])
        }
}

/// Read a window's class name.
pub fn window_class(hwnd: HWND) -> String {
        unsafe {
                let mut buf = [0u16; 256];
                let copied = GetClassNameW(hwnd, &mut buf);
                String::from_utf16_lossy(&buf[..copied.max(0) as usize])
        }
}

/// Name of the executable that owns the window (lowercased), if it can be
/// determined.
pub fn window_exe(hwnd: HWND) -> String {
        use windows::Win32::System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        };

        unsafe {
                let mut pid = 0u32;
                let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                        hwnd,
                        Some(&mut pid),
                );
                if pid == 0 {
                        return String::new();
                }
                let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                        return String::new();
                };
                let mut buf = [0u16; 1024];
                let mut len = buf.len() as u32;
                let ok = QueryFullProcessImageNameW(
                        process,
                        windows::Win32::System::Threading::PROCESS_NAME_WIN32,
                        PWSTR(buf.as_mut_ptr()),
                        &mut len,
                );
                let _ = windows::Win32::Foundation::CloseHandle(process);
                if ok.is_err() {
                        return String::new();
                }
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                path.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase()
        }
}

/// Wrapper for `GetWindowLongPtrW` with the given index.
pub fn get_window_long_ptr(
        hwnd: HWND,
        index: i32,
) -> isize {
        unsafe { GetWindowLongPtrW(hwnd, WINDOW_LONG_PTR_INDEX(index)) }
}

/// True while the HWND is still a valid window.
pub fn is_alive(hwnd: HWND) -> bool {
        unsafe { IsWindow(Some(hwnd)).as_bool() }
}

/// True if the window is visible.
pub fn is_visible(hwnd: HWND) -> bool {
        unsafe { IsWindowVisible(hwnd).as_bool() }
}

/// Pack a pointer-sized value into `WPARAM`/`LPARAM`.
pub fn wparam(v: usize) -> WPARAM {
        WPARAM(v)
}

pub fn lparam(v: isize) -> LPARAM {
        LPARAM(v)
}

/// Close an owned handle, ignoring errors.
pub fn close_handle(handle: HANDLE) {
        unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
}

/// The window's minimum track size (WM_GETMINMAXINFO), in physical
/// pixels. Apps that enforce a minimum size (e.g. Windows Terminal)
/// silently clamp SetWindowPos to it — the layout must respect that
/// or columns visually overlap their neighbors. Returns (0, 0) when
/// the window doesn't answer (default/none).
pub fn min_track_size(hwnd: HWND) -> (f64, f64) {
        use windows::Win32::UI::WindowsAndMessaging::{
                MINMAXINFO, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_GETMINMAXINFO,
        };
        let mut mmi = MINMAXINFO::default();
        let res = unsafe {
                SendMessageTimeoutW(
                        hwnd,
                        WM_GETMINMAXINFO,
                        WPARAM(0),
                        LPARAM(&mut mmi as *mut _ as isize),
                        SMTO_ABORTIFHUNG,
                        100,
                        None,
                )
        };
        if res.0 != 0 {
                (
                        mmi.ptMinTrackSize.x.max(0) as f64,
                        mmi.ptMinTrackSize.y.max(0) as f64,
                )
        } else {
                (0.0, 0.0)
        }
}

/// Force a background process to bring `hwnd` to the foreground.
///
/// Plain `SetForegroundWindow` is silently rejected by the OS when the
/// caller isn't the foreground process (foreground lock). The classic
/// workaround is to attach our input queue to the current foreground
/// thread (and the target's) with `AttachThreadInput`, which lifts the
/// lock, then bring the window up and detach again.
/// Returns true when the window ended up in the foreground.
pub fn force_set_foreground(hwnd: HWND) -> bool {
        use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
        use windows::Win32::UI::WindowsAndMessaging::{
                BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId,
                SetForegroundWindow,
        };

        unsafe {
                if hwnd.0.is_null() || !IsWindow(Some(hwnd)).as_bool() {
                        return false;
                }
                let me = GetCurrentThreadId();
                let fg = GetForegroundWindow();
                // Overview prepares focus under its cover. Revealing it must
                // not trigger another activation/z-order cycle for the same
                // HWND (some applications repaint their decorations on it).
                if fg == hwnd {
                        return true;
                }
                let fg_thread = if fg.0.is_null() {
                        0
                } else {
                        GetWindowThreadProcessId(fg, None)
                };
                let target_thread = GetWindowThreadProcessId(hwnd, None);

                // Attach to the foreground thread (lifts the foreground lock) and
                // to the target's thread so SetFocus reaches its queue.
                let attach_fg = fg_thread != 0 && fg_thread != me;
                let attach_target =
                        target_thread != 0 && target_thread != me && target_thread != fg_thread;
                if attach_fg {
                        let _ = AttachThreadInput(me, fg_thread, true);
                }
                if attach_target {
                        let _ = AttachThreadInput(me, target_thread, true);
                }

                let _ = BringWindowToTop(hwnd);
                let ok = SetForegroundWindow(hwnd).as_bool();
                if !ok {
                        // Fall back to at least moving the keyboard focus.
                        let _ = SetFocus(Some(hwnd));
                }

                if attach_target {
                        let _ = AttachThreadInput(me, target_thread, false);
                }
                if attach_fg {
                        let _ = AttachThreadInput(me, fg_thread, false);
                }
                ok
        }
}
