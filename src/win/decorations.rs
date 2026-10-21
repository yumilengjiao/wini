//! Borderless fullscreen decorations: remember only the style bits changed
//! by wini, and restore those exact bits without adding a classic frame to CSD
//! windows. Application-owned unrelated style changes remain untouched.

use std::collections::HashMap;
use windows::Win32::Foundation::{ERROR_SUCCESS, GetLastError, HWND, SetLastError};
use windows::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
        WS_SYSMENU, WS_THICKFRAME,
};

const STRIP: u32 =
        WS_CAPTION.0 | WS_THICKFRAME.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0;

#[derive(Debug, Clone, Copy)]
struct SavedStyle {
        decoration_bits: u32,
}

impl SavedStyle {
        fn capture(style: u32) -> Self {
                Self {
                        decoration_bits: style & STRIP,
                }
        }
        fn restore(
                self,
                current: u32,
        ) -> u32 {
                (current & !STRIP) | self.decoration_bits
        }
}

#[derive(Default)]
pub struct Decorations {
        saved: HashMap<isize, SavedStyle>,
}

impl Decorations {
        pub fn ids(&self) -> Vec<isize> {
                self.saved.keys().copied().collect()
        }

        pub fn make_borderless(
                &mut self,
                hwnd: HWND,
        ) {
                let id = hwnd.0 as isize;
                if self.saved.contains_key(&id) || !super::api::is_alive(hwnd) {
                        return;
                }
                let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
                let saved = SavedStyle::capture(style);
                // Save before changing anything: even a frame-refresh failure
                // must not lose the information needed to undo our mutation.
                self.saved.insert(id, saved);
                if let Err(e) = set_style(hwnd, style, style & !STRIP) {
                        log::warn!("borderless style change failed for {id}: {e}");
                }
        }

        pub fn restore(
                &mut self,
                id: isize,
        ) {
                let Some(&saved) = self.saved.get(&id) else {
                        return;
                };
                let hwnd = HWND(id as *mut _);
                if !super::api::is_alive(hwnd) {
                        self.saved.remove(&id);
                        return;
                }
                let current = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
                match set_style(hwnd, current, saved.restore(current)) {
                        Ok(()) => {
                                self.saved.remove(&id);
                        },
                        Err(e) => log::warn!("restoring decorations failed for {id}: {e}"),
                }
        }
}

fn set_style(
        hwnd: HWND,
        current: u32,
        target: u32,
) -> windows::core::Result<()> {
        if current == target {
                return Ok(());
        }
        unsafe {
                SetLastError(ERROR_SUCCESS);
                let old = SetWindowLongPtrW(hwnd, GWL_STYLE, target as isize);
                if old == 0 && GetLastError() != ERROR_SUCCESS {
                        return Err(windows::core::Error::from_thread());
                }
                SetWindowPos(
                        hwnd,
                        None,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                )
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use windows::Win32::UI::WindowsAndMessaging::{WS_DISABLED, WS_POPUP, WS_VISIBLE};

        #[test]
        fn csd_window_does_not_gain_caption_or_resize_frame_on_restore() {
                let original = WS_POPUP.0 | WS_VISIBLE.0;
                assert_eq!(
                        SavedStyle::capture(original).restore(original & !STRIP),
                        original
                );
        }

        #[test]
        fn restore_reinstates_exact_decoration_bits_and_preserves_app_changes() {
                let original = WS_VISIBLE.0 | WS_CAPTION.0 | WS_SYSMENU.0;
                let current = (original & !STRIP) | WS_DISABLED.0;
                let restored = SavedStyle::capture(original).restore(current);
                assert_eq!(restored, original | WS_DISABLED.0);
                assert_eq!(restored & WS_THICKFRAME.0, 0);
                assert_eq!(restored & WS_MAXIMIZEBOX.0, 0);
        }
}
