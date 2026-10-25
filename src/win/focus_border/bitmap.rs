//! Rasterize the clipped ring off-screen. UpdateLayeredWindow publishes its
//! pixels, size and position together; no visible intermediate SetWindowRgn
//! or WM_PAINT frame. The compositor retains the bitmap after submission.

use super::RingShape;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
        FillRect, FillRgn, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_COLORKEY, UpdateLayeredWindow};

pub(super) struct RingBitmap {
        dc: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
        size: SIZE,
        transparent: COLORREF,
}

impl RingBitmap {
        pub(super) fn new(
                shape: RingShape,
                color: COLORREF,
        ) -> Option<Self> {
                let size = SIZE {
                        cx: shape.w + 2 * shape.t,
                        cy: shape.h + 2 * shape.t,
                };
                // A key distinct from every configured ring color, including black.
                let transparent = COLORREF(color.0 ^ 0x00FF_FFFF);
                unsafe {
                        let screen = GetDC(None);
                        if screen.is_invalid() {
                                return None;
                        }
                        let dc = CreateCompatibleDC(Some(screen));
                        let bitmap = CreateCompatibleBitmap(screen, size.cx, size.cy);
                        ReleaseDC(None, screen);
                        if dc.is_invalid() || bitmap.is_invalid() {
                                let _ = DeleteDC(dc);
                                let _ = DeleteObject(bitmap.into());
                                return None;
                        }
                        let previous = SelectObject(dc, bitmap.into());
                        if previous.is_invalid() {
                                let _ = DeleteObject(bitmap.into());
                                let _ = DeleteDC(dc);
                                return None;
                        }
                        let image = Self {
                                dc,
                                bitmap,
                                previous,
                                size,
                                transparent,
                        };
                        let region = shape.region()?;
                        let background = CreateSolidBrush(transparent);
                        let foreground = CreateSolidBrush(color);
                        let bounds = RECT {
                                left: 0,
                                top: 0,
                                right: size.cx,
                                bottom: size.cy,
                        };
                        let ok = !background.is_invalid()
                                && !foreground.is_invalid()
                                && FillRect(dc, &bounds, background) != 0
                                && FillRgn(dc, region, foreground).as_bool();
                        let _ = DeleteObject(region.into());
                        let _ = DeleteObject(background.into());
                        let _ = DeleteObject(foreground.into());
                        ok.then_some(image)
                }
        }

        pub(super) fn present(
                &self,
                hwnd: HWND,
                x: i32,
                y: i32,
        ) -> bool {
                unsafe {
                        UpdateLayeredWindow(
                                hwnd,
                                None,
                                Some(&POINT { x, y }),
                                Some(&self.size),
                                Some(self.dc),
                                Some(&POINT::default()),
                                self.transparent,
                                None,
                                ULW_COLORKEY,
                        )
                        .is_ok()
                }
        }
}

impl Drop for RingBitmap {
        fn drop(&mut self) {
                unsafe {
                        SelectObject(self.dc, self.previous);
                        let _ = DeleteObject(self.bitmap.into());
                        let _ = DeleteDC(self.dc);
                }
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use windows::Win32::Graphics::Gdi::GetPixel;

        #[test]
        fn layered_submission_changes_size_and_position_without_showing_a_window() {
                // Our own invisible HWND only; never activate or move an app window.
                let border = super::super::FocusBorder::new().unwrap();
                let work = RECT {
                        left: -30000,
                        top: -30000,
                        right: -10000,
                        bottom: -10000,
                };
                for (x, y, w, h) in [(-20000, -20000, 400, 300), (-21000, -21000, 600, 500)] {
                        let shape = RingShape::clipped((x, y, w, h), 4, 12, work).unwrap();
                        let image = RingBitmap::new(shape, COLORREF(0x00A3_AE7D)).unwrap();
                        assert!(image.present(border.hwnd, x - 4, y - 4));
                        unsafe {
                                use windows::Win32::UI::WindowsAndMessaging::{
                                        GetWindowRect, IsWindowVisible,
                                };
                                let mut actual = RECT::default();
                                GetWindowRect(border.hwnd, &mut actual).unwrap();
                                assert_eq!(
                                        actual,
                                        RECT {
                                                left: x - 4,
                                                top: y - 4,
                                                right: x + w + 4,
                                                bottom: y + h + 4
                                        }
                                );
                                assert!(!IsWindowVisible(border.hwnd).as_bool());
                        }
                }
        }

        #[test]
        fn bitmap_is_hollow_and_clipped_even_with_black_ring_color() {
                let work = RECT {
                        left: 80,
                        top: 0,
                        right: 1000,
                        bottom: 800,
                };
                for color in [COLORREF(0), COLORREF(0x00A3_AE7D)] {
                        let shape = RingShape::clipped((70, 30, 400, 600), 4, 12, work).unwrap();
                        let image = RingBitmap::new(shape, color).unwrap();
                        unsafe {
                                // Left edge is outside work area; right edge stays colored.
                                assert_eq!(GetPixel(image.dc, 1, 200), image.transparent);
                                assert_eq!(GetPixel(image.dc, 200, 200), image.transparent);
                                assert_eq!(GetPixel(image.dc, 405, 200), color);
                        }
                }
        }
}
