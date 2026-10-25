//! Desktop bar classification, including vertical appbars. Independent of
//! HWND state so all four reserved edges and negative outputs can be tested.

use windows::Win32::Foundation::RECT;

pub(super) fn native_taskbar(class: &str) -> bool {
        matches!(class, "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

pub(super) fn in_reserved_strip(
        bar: RECT,
        full: RECT,
        work: RECT,
) -> bool {
        let overlaps = |a0: i32, a1: i32, b0: i32, b1: i32| b0 < b1 && a0 < b1 && a1 > b0;
        let w = (bar.right - bar.left).max(0) as f64;
        let h = (bar.bottom - bar.top).max(0) as f64;
        let mw = (full.right - full.left).max(1) as f64;
        let mh = (full.bottom - full.top).max(1) as f64;
        let horizontal = (overlaps(bar.top, bar.bottom, full.top, work.top)
                || overlaps(bar.top, bar.bottom, work.bottom, full.bottom))
                && overlaps(bar.left, bar.right, full.left, full.right)
                && w >= mw * 0.5
                && h > 0.0
                && h <= mh * 0.25;
        let vertical = (overlaps(bar.left, bar.right, full.left, work.left)
                || overlaps(bar.left, bar.right, work.right, full.right))
                && overlaps(bar.top, bar.bottom, full.top, full.bottom)
                && h >= mh * 0.5
                && w > 0.0
                && w <= mw * 0.25;
        horizontal || vertical
}

#[cfg(test)]
mod tests {
        use super::*;
        #[test]
        fn recognizes_all_four_edges_but_not_tooltips_or_other_outputs() {
                let full = RECT {
                        left: -1000,
                        top: -100,
                        right: 0,
                        bottom: 700,
                };
                for (work, bar) in [
                        (
                                RECT { left: -920, ..full },
                                RECT {
                                        right: -920,
                                        ..full
                                },
                        ),
                        (RECT { right: -80, ..full }, RECT { left: -80, ..full }),
                        (
                                RECT { top: -52, ..full },
                                RECT {
                                        bottom: -52,
                                        ..full
                                },
                        ),
                        (
                                RECT {
                                        bottom: 652,
                                        ..full
                                },
                                RECT { top: 652, ..full },
                        ),
                ] {
                        assert!(in_reserved_strip(bar, full, work));
                        assert!(!in_reserved_strip(full, full, work));
                        assert!(!in_reserved_strip(
                                RECT {
                                        left: 100,
                                        top: 100,
                                        right: 180,
                                        bottom: 600
                                },
                                full,
                                work
                        ));
                        assert!(!in_reserved_strip(
                                RECT {
                                        left: -1000,
                                        top: -100,
                                        right: -980,
                                        bottom: -80
                                },
                                full,
                                work
                        ));
                }
                assert!(!in_reserved_strip(full, full, full));
        }
        #[test]
        fn native_taskbars_do_not_require_third_party_toolwindow_style() {
                assert!(native_taskbar("Shell_TrayWnd"));
                assert!(native_taskbar("Shell_SecondaryTrayWnd"));
                assert!(!native_taskbar("wini_focus"));
                assert!(!native_taskbar("tooltips_class32"));
        }
}
