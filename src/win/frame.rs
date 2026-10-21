//! Visible window geometry and DWM thumbnail cropping.
//!
//! All coordinates are physical pixels; margins are measured per HWND, not
//! guessed from a particular Windows version, DPI, or application class.

use super::api::window_rect;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};

/// Visible DWM frame (excluding invisible resize borders), with HWND fallback.
pub fn frame_rect(hwnd: HWND) -> Option<(f64, f64, f64, f64)> {
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
        if ok && rc.right > rc.left && rc.bottom > rc.top {
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

        /// DWM can report a full outer surface or an already-trimmed surface.
        /// Avoid cropping the latter twice; otherwise map margins to source px.
        pub fn source_crop(
                self,
                outer_size: (f64, f64),
                source_size: (i32, i32),
        ) -> RECT {
                let (sw, sh) = (source_size.0.max(1), source_size.1.max(1));
                let visible_w = outer_size.0 - self.left - self.right;
                let visible_h = outer_size.1 - self.top - self.bottom;
                if (sw as f64 - visible_w).abs() <= 1.0 && (sh as f64 - visible_h).abs() <= 1.0 {
                        return RECT {
                                left: 0,
                                top: 0,
                                right: sw,
                                bottom: sh,
                        };
                }
                let sx = sw as f64 / outer_size.0.max(1.0);
                let sy = sh as f64 / outer_size.1.max(1.0);
                let left = ((self.left * sx).round() as i32).clamp(0, sw - 1);
                let top = ((self.top * sy).round() as i32).clamp(0, sh - 1);
                RECT {
                        left,
                        top,
                        right: (sw - (self.right * sx).round() as i32).clamp(left + 1, sw),
                        bottom: (sh - (self.bottom * sy).round() as i32).clamp(top + 1, sh),
                }
        }

        /// Independent X/Y scaling is required while a parked source window
        /// is stretched by a width animation in addition to overview zoom.
        pub fn apply(
                self,
                rect: (f64, f64, f64, f64),
                source_size: (f64, f64),
        ) -> (i32, i32, i32, i32) {
                let (x, y, w, h) = rect;
                let sx = w / source_size.0.max(1.0);
                let sy = h / source_size.1.max(1.0);
                let (l, t, r, b) = (
                        (x + self.left * sx).round() as i32,
                        (y + self.top * sy).round() as i32,
                        (x + w - self.right * sx).round() as i32,
                        (y + h - self.bottom * sy).round() as i32,
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
mod tests {
        use super::*;

        #[test]
        fn visible_bounds_agree_at_thumbnail_handoff() {
                let margins = FrameInsets::between(
                        (-1000.0, 48.0, 1000.0, 752.0),
                        (-993.0, 48.0, 986.0, 745.0),
                );
                assert_eq!(
                        margins.apply((-1000.0, 48.0, 1000.0, 752.0), (1000.0, 752.0)),
                        (-993, 48, 986, 745)
                );
                assert_eq!(
                        margins.apply((100.0, 100.0, 500.0, 376.0), (1000.0, 752.0)),
                        (104, 100, 493, 373)
                );
        }

        #[test]
        fn thumbnail_margins_follow_independent_resize_axes() {
                let margins =
                        FrameInsets::between((0.0, 0.0, 1000.0, 800.0), (10.0, 0.0, 980.0, 790.0));
                assert_eq!(
                        margins.apply((100.0, 50.0, 800.0, 400.0), (1000.0, 800.0)),
                        (108, 50, 784, 395)
                );
        }

        #[test]
        fn source_crop_handles_full_and_already_visible_dwm_surfaces() {
                let margins =
                        FrameInsets::between((0.0, 0.0, 1000.0, 800.0), (10.0, 0.0, 980.0, 790.0));
                let full = margins.source_crop((1000.0, 800.0), (1000, 800));
                assert_eq!(
                        (full.left, full.top, full.right, full.bottom),
                        (10, 0, 990, 790)
                );
                let visible = margins.source_crop((1000.0, 800.0), (980, 790));
                assert_eq!(
                        (visible.left, visible.top, visible.right, visible.bottom),
                        (0, 0, 980, 790)
                );
                let scaled = margins.source_crop((1000.0, 800.0), (2000, 1600));
                assert_eq!(
                        (scaled.left, scaled.top, scaled.right, scaled.bottom),
                        (20, 0, 1980, 1580)
                );
        }

        #[test]
        fn tiny_source_crop_remains_nonempty() {
                let margins =
                        FrameInsets::between((0.0, 0.0, 100.0, 100.0), (49.0, 49.0, 2.0, 2.0));
                let crop = margins.source_crop((100.0, 100.0), (1, 1));
                assert_eq!((crop.left, crop.top, crop.right, crop.bottom), (0, 0, 1, 1));
        }

        #[test]
        fn custom_decorations_do_not_expand_the_ring() {
                let margins = FrameInsets::between(
                        (0.0, 0.0, 800.0, 600.0),
                        (-10.0, -10.0, 820.0, 620.0),
                );
                assert_eq!(
                        margins.apply((0.0, 0.0, 800.0, 600.0), (800.0, 600.0)),
                        (0, 0, 800, 600)
                );
        }
}
