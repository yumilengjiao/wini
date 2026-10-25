//! Active-window focus border (niri's focus-ring): a topless, topmost,
//! click-through frame that outlines the focused window.
//!
//! Pixels, size and position are submitted atomically through a layered
//! bitmap. Pure translations reuse the compositor's existing image instead
//! of reshaping and invalidating a visible window on every animation tick.

mod bitmap;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
        CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, HRGN, RGN_AND, RGN_DIFF,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, GW_HWNDPREV, GetWindow, HWND_TOPMOST, RegisterClassW,
        SW_HIDE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
        SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, WS_EX_LAYERED,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

/// Ring geometry keeps the original window edge. Intersect the RING with
/// work area, not the window rect: clipping must never invent an edge along
/// the taskbar when the actual window edge has scrolled out of view.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RingShape {
        w: i32,
        h: i32,
        t: i32,
        r: i32,
        clip: RECT,
}

impl RingShape {
        fn clipped(
                rect: (i32, i32, i32, i32),
                t: i32,
                r: i32,
                work: RECT,
        ) -> Option<Self> {
                let (x, y, w, h) = rect;
                let (ox, oy) = (x - t, y - t);
                let clip = RECT {
                        left: (work.left - ox).max(0),
                        top: (work.top - oy).max(0),
                        right: (work.right - ox).min(w + 2 * t),
                        bottom: (work.bottom - oy).min(h + 2 * t),
                };
                (w > 0 && h > 0 && clip.left < clip.right && clip.top < clip.bottom)
                        .then_some(Self { w, h, t, r, clip })
        }

        /// Caller owns the returned GDI region.
        unsafe fn region(self) -> Option<HRGN> {
                let Self { w, h, t, r, clip } = self;
                unsafe {
                        let full = if r > 0 {
                                CreateRoundRectRgn(
                                        0,
                                        0,
                                        w + 2 * t,
                                        h + 2 * t,
                                        2 * (r + t),
                                        2 * (r + t),
                                )
                        } else {
                                CreateRectRgn(0, 0, w + 2 * t, h + 2 * t)
                        };
                        let inner = if r > 0 {
                                CreateRoundRectRgn(t, t, t + w, t + h, 2 * r, 2 * r)
                        } else {
                                CreateRectRgn(t, t, t + w, t + h)
                        };
                        let bounds = CreateRectRgn(clip.left, clip.top, clip.right, clip.bottom);
                        let ring = CreateRectRgn(0, 0, 0, 0);
                        let ok = !full.is_invalid()
                                && !inner.is_invalid()
                                && !bounds.is_invalid()
                                && !ring.is_invalid()
                                && CombineRgn(Some(ring), Some(full), Some(inner), RGN_DIFF).0 != 0
                                && CombineRgn(Some(ring), Some(ring), Some(bounds), RGN_AND).0 != 0;
                        for region in [full, inner, bounds] {
                                let _ = DeleteObject(region.into());
                        }
                        if ok {
                                Some(ring)
                        } else {
                                let _ = DeleteObject(ring.into());
                                None
                        }
                }
        }
}

pub struct FocusBorder {
        hwnd: HWND,
        shape: std::cell::Cell<Option<RingShape>>,
        color: std::cell::Cell<Option<COLORREF>>,
        last_rect: std::cell::Cell<Option<(i32, i32, i32, i32)>>,
        visible: std::cell::Cell<bool>,
}

impl FocusBorder {
        pub fn new() -> Option<Self> {
                let class_name = w!("wini_focus");
                unsafe {
                        let hinstance = GetModuleHandleW(None).ok()?;
                        let wc = WNDCLASSW {
                                lpfnWndProc: Some(wnd_proc),
                                hInstance: hinstance.into(),
                                lpszClassName: class_name,
                                ..Default::default()
                        };
                        let _ = RegisterClassW(&wc);

                        // Click-through (WS_EX_TRANSPARENT), never activates, no
                        // taskbar/alt-tab presence, always on top.
                        let hwnd = CreateWindowExW(
                                WINDOW_EX_STYLE(
                                        WS_EX_LAYERED.0
                                                | WS_EX_TOPMOST.0
                                                | WS_EX_TOOLWINDOW.0
                                                | WS_EX_NOACTIVATE.0
                                                | WS_EX_TRANSPARENT.0,
                                ),
                                class_name,
                                w!(""),
                                WINDOW_STYLE(WS_POPUP.0),
                                0,
                                0,
                                0,
                                0,
                                None,
                                None,
                                Some(hinstance.into()),
                                None,
                        )
                        .ok()?;
                        // Do not call SetLayeredWindowAttributes: it disables
                        // UpdateLayeredWindow until WS_EX_LAYERED is reset.
                        Some(FocusBorder {
                                hwnd,
                                shape: std::cell::Cell::new(None),
                                color: std::cell::Cell::new(None),
                                last_rect: std::cell::Cell::new(None),
                                visible: std::cell::Cell::new(false),
                        })
                }
        }

        /// Outline the rect (x, y, w, h) — in screen coordinates — with a
        /// ring of `thickness` pixels drawn just outside it, in `color`.
        /// `radius` rounds the ring's corners: the inner edge gets radius
        /// `radius`, the outer edge `radius + thickness`, keeping the band
        /// a constant width around the bend. 0 = square corners.
        #[allow(clippy::too_many_arguments)]
        pub fn update(
                &self,
                x: i32,
                y: i32,
                w: i32,
                h: i32,
                thickness: i32,
                radius: i32,
                color: COLORREF,
                work: RECT,
        ) {
                let t = thickness.max(1);
                let r = radius.max(0);
                let (ow, oh) = (w + 2 * t, h + 2 * t);
                let Some(shape) = RingShape::clipped((x, y, w, h), t, r, work) else {
                        self.hide();
                        return;
                };
                let rect = (x - t, y - t, ow, oh);
                let redraw = self.shape.get() != Some(shape) || self.color.get() != Some(color);
                if redraw {
                        let Some(image) = bitmap::RingBitmap::new(shape, color) else {
                                self.hide();
                                return;
                        };
                        if !image.present(self.hwnd, rect.0, rect.1) {
                                self.hide();
                                return;
                        }
                        self.shape.set(Some(shape));
                        self.color.set(Some(color));
                        self.last_rect.set(Some(rect));
                } else if self.last_rect.get() != Some(rect) {
                        // Same shape: translate its existing bitmap without
                        // repaint or a separate resize of the hollow frame.
                        if unsafe {
                                SetWindowPos(
                                        self.hwnd,
                                        None,
                                        rect.0,
                                        rect.1,
                                        0,
                                        0,
                                        SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
                                )
                        }
                        .is_err()
                        {
                                self.hide();
                                return;
                        }
                        self.last_rect.set(Some(rect));
                }
                // No unconditional second SetWindowPos per update. Only
                // reveal or repair Z-order when another overlay is above us.
                if !self.visible.get() || unsafe { GetWindow(self.hwnd, GW_HWNDPREV) }.is_ok() {
                        if unsafe {
                                SetWindowPos(
                                        self.hwnd,
                                        Some(HWND_TOPMOST),
                                        0,
                                        0,
                                        0,
                                        0,
                                        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
                                )
                        }
                        .is_err()
                        {
                                self.hide();
                                return;
                        }
                        self.visible.set(true);
                }
        }

        pub fn hide(&self) {
                if self.visible.replace(false) {
                        unsafe {
                                let _ = ShowWindow(self.hwnd, SW_HIDE);
                        }
                }
        }
}

impl Drop for FocusBorder {
        fn drop(&mut self) {
                unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.hwnd);
                }
        }
}

extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

#[cfg(test)]
mod tests {
        use super::*;
        use windows::Win32::Graphics::Gdi::PtInRegion;

        #[test]
        fn rounded_ring_pixels_never_cover_taskbar_on_any_edge_or_negative_monitor() {
                for work in [
                        RECT {
                                left: -920,
                                top: 0,
                                right: 0,
                                bottom: 800,
                        },
                        RECT {
                                left: -1000,
                                top: 0,
                                right: -80,
                                bottom: 800,
                        },
                        RECT {
                                left: -1000,
                                top: 48,
                                right: 0,
                                bottom: 800,
                        },
                        RECT {
                                left: -1000,
                                top: 0,
                                right: 0,
                                bottom: 752,
                        },
                ] {
                        // Window crosses every border during a slide. Test the
                        // actual shaped region, including round corners.
                        let (x, y, w, h) = (-960, 20, 960, 700);
                        let shape = RingShape::clipped((x, y, w, h), 4, 12, work).unwrap();
                        let region = unsafe { shape.region().unwrap() };
                        let mut pixels = 0;
                        for sy in -40..840 {
                                for sx in -1050..50 {
                                        if unsafe {
                                                PtInRegion(region, sx - (x - 4), sy - (y - 4))
                                                        .as_bool()
                                        } {
                                                pixels += 1;
                                                assert!(sx >= work.left && sx < work.right);
                                                assert!(sy >= work.top && sy < work.bottom);
                                        }
                                }
                        }
                        assert!(pixels > 0, "test must contain visible border pixels");
                        // Work-area clipping must not draw a fake vertical
                        // focus edge through the middle of the actual window.
                        assert!(!unsafe {
                                PtInRegion(region, work.left - (x - 4), 400 - (y - 4)).as_bool()
                        });
                        let _ = unsafe { DeleteObject(region.into()) };
                }
        }

        #[test]
        fn movement_updates_clip_even_when_size_is_unchanged() {
                let work = RECT {
                        left: 80,
                        top: 0,
                        right: 1000,
                        bottom: 800,
                };
                let a = RingShape::clipped((70, 30, 400, 600), 4, 12, work).unwrap();
                let b = RingShape::clipped((90, 30, 400, 600), 4, 12, work).unwrap();
                assert_ne!(a, b);
                assert_eq!(a.clip.left, 14);
                assert_eq!(b.clip.left, 0);
                assert!(RingShape::clipped((-500, 30, 400, 600), 4, 12, work).is_none());
                let region = unsafe { b.region().unwrap() };
                assert!(unsafe { PtInRegion(region, 1, 200).as_bool() });
                assert!(!unsafe { PtInRegion(region, 200, 200).as_bool() });
                let _ = unsafe { DeleteObject(region.into()) };
        }
}
