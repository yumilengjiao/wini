//! Visible window geometry for real HWNDs and native DWM thumbnail destinations.
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

        /// Convert an animated outer destination to its visible frame. Resize
        /// margins stay physical; only overview zoom scales them. A width
        /// animation must NOT multiply margins by target_width/source_width.
        pub fn apply(
                self,
                rect: (f64, f64, f64, f64),
                zoom: f64,
        ) -> (i32, i32, i32, i32) {
                let (x, y, w, h) = rect;
                let (l, t, r, b) = (
                        (x + self.left * zoom).round() as i32,
                        (y + self.top * zoom).round() as i32,
                        (x + w - self.right * zoom).round() as i32,
                        (y + h - self.bottom * zoom).round() as i32,
                );
                (l, t, (r - l).max(1), (b - t).max(1))
        }
}

/// Measure margins only when the outer bounds bracket a stable DWM read.
/// The two APIs are not an atomic snapshot; during asynchronous window resize,
/// combining different frames can manufacture huge or negative margins.
pub fn frame_insets(hwnd: HWND) -> FrameInsets {
        for _ in 0..2 {
                let Some(before) = window_rect(hwnd) else {
                        break;
                };
                let Some(frame) = frame_rect(hwnd) else {
                        break;
                };
                if window_rect(hwnd) == Some(before) {
                        return FrameInsets::between(before, frame);
                }
        }
        FrameInsets::default()
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
                        margins.apply((-1000.0, 48.0, 1000.0, 752.0), 1.0),
                        (-993, 48, 986, 745)
                );
                assert_eq!(
                        margins.apply((100.0, 100.0, 500.0, 376.0), 0.5),
                        (104, 100, 493, 373)
                );
        }

        #[test]
        fn resize_keeps_physical_margins_instead_of_stretching_them() {
                let margins = FrameInsets::between(
                        (4.0, 3.0, 1235.0, 1594.0),
                        (14.0, 3.0, 1215.0, 1584.0),
                );
                assert_eq!(
                        margins.apply((4.0, 3.0, 2470.0, 1594.0), 1.0),
                        (14, 3, 2450, 1584)
                );
                assert_eq!(
                        margins.apply((100.0, 100.0, 1235.0, 797.0), 0.5),
                        (105, 100, 1225, 792)
                );
        }

        #[test]
        fn real_terminal_fixture_returns_same_edge_before_and_after_zoom() {
                // Read-only HWND/DWM probe on the user's desktop: native
                // thumbnail is 1215x1584, NOT the 1235x1594 outer HWND.
                let outer = (4.0, 3.0, 1235.0, 1594.0);
                let frame = (14.0, 3.0, 1215.0, 1584.0);
                let insets = FrameInsets::between(outer, frame);
                let (fx, fy, fw, fh) = (0.0, 0.0, 2478.0, 1600.0);
                for zoom in [1.0, 0.75, 0.35, 0.75, 1.0] {
                        let destination = (
                                fx + fw * (1.0 - zoom) / 2.0 + (outer.0 - fx) * zoom,
                                fy + fh * (1.0 - zoom) / 2.0 + (outer.1 - fy) * zoom,
                                outer.2 * zoom,
                                outer.3 * zoom,
                        );
                        let actual = insets.apply(destination, zoom);
                        let expected_left = (fx + fw * (1.0 - zoom) / 2.0 + (frame.0 - fx) * zoom)
                                .round() as i32;
                        let expected_right =
                                (fx + fw * (1.0 - zoom) / 2.0 + (frame.0 + frame.2 - fx) * zoom)
                                        .round() as i32;
                        assert_eq!(actual.0, expected_left);
                        assert_eq!(actual.0 + actual.2, expected_right);
                }
                assert_eq!(insets.apply(outer, 1.0), (14, 3, 1215, 1584));
        }

        #[test]
        fn negative_monitor_visible_edges_use_same_coordinate_space() {
                let outer = (-2467.0, 3.0, 2470.0, 1594.0);
                let frame = (-2457.0, 3.0, 2450.0, 1584.0);
                assert_eq!(
                        FrameInsets::between(outer, frame).apply(outer, 1.0),
                        (-2457, 3, 2450, 1584)
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
