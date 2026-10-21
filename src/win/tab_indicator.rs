//! Click-through, no-activate tab indicator overlay. Geometry and hit targets
//! are supplied by the app; painting is confined to shaped GDI regions.

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
        BeginPaint, CombineRgn, CreateRectRgn, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
        EndPaint, FillRgn, PAINTSTRUCT, RDW_INVALIDATE, RDW_UPDATENOW, RGN_OR, RedrawWindow,
        SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW,
        HWND_TOPMOST, LWA_ALPHA, RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_NOSENDCHANGING,
        SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow,
        WM_ERASEBKGND, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabMark {
        pub id: isize,
        pub rect: (i32, i32, i32, i32),
        pub color: u32,
        pub radius: i32,
}

impl TabMark {
        fn colorref(&self) -> COLORREF {
                let rgb = self.color;
                COLORREF(((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff))
        }

        /// Both shaping and painting use this exact region, relative to the
        /// host origin. Caller owns and must delete the returned GDI region.
        unsafe fn region(
                &self,
                origin: (i32, i32),
        ) -> windows::Win32::Graphics::Gdi::HRGN {
                let (x, y, w, h) = self.rect;
                let (x, y) = (x - origin.0, y - origin.1);
                let radius = self.radius.max(0).min(w.min(h) / 2);
                unsafe {
                        if radius > 0 {
                                CreateRoundRectRgn(x, y, x + w, y + h, 2 * radius, 2 * radius)
                        } else {
                                CreateRectRgn(x, y, x + w, y + h)
                        }
                }
        }
}

#[derive(Default)]
struct PaintData {
        marks: Vec<TabMark>,
        origin: (i32, i32),
}

pub struct TabOverlay {
        hwnd: HWND,
        data: Box<PaintData>,
        visible: bool,
}

impl TabOverlay {
        pub fn new() -> Option<Self> {
                unsafe {
                        let instance = GetModuleHandleW(None).ok()?;
                        let class = w!("wini_tabs");
                        let wc = WNDCLASSW {
                                lpfnWndProc: Some(wnd_proc),
                                hInstance: instance.into(),
                                lpszClassName: class,
                                ..Default::default()
                        };
                        let _ = RegisterClassW(&wc);
                        let hwnd = CreateWindowExW(
                                WS_EX_LAYERED
                                        | WS_EX_NOACTIVATE
                                        | WS_EX_TOOLWINDOW
                                        | WS_EX_TOPMOST
                                        | WS_EX_TRANSPARENT,
                                class,
                                w!(""),
                                WS_POPUP,
                                0,
                                0,
                                1,
                                1,
                                None,
                                None,
                                Some(instance.into()),
                                None,
                        )
                        .ok()?;
                        let mut data = Box::<PaintData>::default();
                        SetWindowLongPtrW(
                                hwnd,
                                GWLP_USERDATA,
                                (&mut *data as *mut PaintData) as isize,
                        );
                        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                        Some(Self {
                                hwnd,
                                data,
                                visible: false,
                        })
                }
        }

        pub fn marks(&self) -> &[TabMark] {
                &self.data.marks
        }

        pub fn update(
                &mut self,
                marks: Vec<TabMark>,
        ) {
                if marks.is_empty() {
                        self.hide();
                        self.data.marks.clear();
                        return;
                }
                if marks == self.data.marks && self.visible {
                        // The overview host may have been re-raised since the
                        // last frame, even when marker geometry stayed fixed.
                        unsafe {
                                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0,0,0,0,
                                SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE);
                        }
                        return;
                }
                let left = marks.iter().map(|m| m.rect.0).min().unwrap();
                let top = marks.iter().map(|m| m.rect.1).min().unwrap();
                let right = marks.iter().map(|m| m.rect.0 + m.rect.2).max().unwrap();
                let bottom = marks.iter().map(|m| m.rect.1 + m.rect.3).max().unwrap();
                self.data.origin = (left, top);
                self.data.marks = marks;
                unsafe {
                        let region = CreateRectRgn(0, 0, 0, 0);
                        for mark in &self.data.marks {
                                let part = mark.region((left, top));
                                CombineRgn(Some(region), Some(region), Some(part), RGN_OR);
                                let _ = DeleteObject(part.into());
                        }
                        if SetWindowRgn(self.hwnd, Some(region), false) == 0 {
                                let _ = DeleteObject(region.into());
                                // Never expose the entire bounding box after
                                // shape failure; it contains transparent gaps.
                                self.hide();
                                return;
                        }
                        // Resize first, paint synchronously, then reveal/raise.
                        // Otherwise DWM can display newly exposed unpainted
                        // pixels as black before the queued WM_PAINT runs.
                        if SetWindowPos(
                                self.hwnd,
                                None,
                                left,
                                top,
                                right - left,
                                bottom - top,
                                SWP_NOACTIVATE
                                        | SWP_NOSENDCHANGING
                                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
                        )
                        .is_err()
                        {
                                self.hide();
                                return;
                        }
                        let _ = RedrawWindow(
                                Some(self.hwnd),
                                None,
                                None,
                                RDW_INVALIDATE | RDW_UPDATENOW,
                        );
                        self.visible = SetWindowPos(
                                self.hwnd,
                                Some(HWND_TOPMOST),
                                0,
                                0,
                                0,
                                0,
                                SWP_NOACTIVATE
                                        | SWP_SHOWWINDOW
                                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE,
                        )
                        .is_ok();
                        if self.visible {
                                // A never-shown layered window may defer paint
                                // while hidden. Complete its first frame now,
                                // not after returning to the message loop.
                                let _ = RedrawWindow(
                                        Some(self.hwnd),
                                        None,
                                        None,
                                        RDW_INVALIDATE | RDW_UPDATENOW,
                                );
                        }
                }
        }

        pub fn hide(&mut self) {
                if self.visible {
                        unsafe {
                                let _ = ShowWindow(self.hwnd, SW_HIDE);
                        }
                        self.visible = false;
                }
        }
}

impl Drop for TabOverlay {
        fn drop(&mut self) {
                unsafe {
                        SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                        let _ = DestroyWindow(self.hwnd);
                }
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use windows::Win32::Graphics::Gdi::PtInRegion;

        #[test]
        fn active_and_inactive_green_channels_are_not_swapped() {
                let mark = |color| TabMark {
                        id: 1,
                        rect: (0, 0, 10, 100),
                        color,
                        radius: 5,
                };
                assert_eq!(mark(0x7daea3).colorref(), COLORREF(0xa3ae7d));
                assert_eq!(mark(0x355f55).colorref(), COLORREF(0x555f35));
        }

        #[test]
        fn rounded_regions_keep_segment_gaps_unpainted() {
                let a = TabMark {
                        id: 1,
                        rect: (-90, 50, 10, 100),
                        color: 0x355f55,
                        radius: 5,
                };
                let b = TabMark {
                        id: 2,
                        rect: (-90, 155, 10, 100),
                        color: 0x7daea3,
                        radius: 5,
                };
                unsafe {
                        let first = a.region((-90, 50));
                        let second = b.region((-90, 50));
                        assert!(PtInRegion(first, 5, 50).as_bool());
                        assert!(!PtInRegion(first, 0, 0).as_bool());
                        assert!(!PtInRegion(first, 5, 102).as_bool());
                        assert!(!PtInRegion(second, 5, 102).as_bool());
                        assert!(PtInRegion(second, 5, 155).as_bool());
                        let _ = DeleteObject(first.into());
                        let _ = DeleteObject(second.into());
                }
        }
}

extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
) -> LRESULT {
        if msg == WM_ERASEBKGND {
                // Each visible segment is completely painted below. Default
                // erasure between shape updates introduces a black flash.
                return LRESULT(1);
        }
        if msg == WM_PAINT {
                unsafe {
                        let mut ps = PAINTSTRUCT::default();
                        let dc = BeginPaint(hwnd, &mut ps);
                        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const PaintData;
                        if !pointer.is_null() && !dc.is_invalid() {
                                let data = &*pointer;
                                for mark in &data.marks {
                                        let brush = CreateSolidBrush(mark.colorref());
                                        // Paint precisely the same rounded region
                                        // used by the window shape. Rectangular
                                        // fills can overwrite adjacent rounded
                                        // segments with another tab's color.
                                        let region = mark.region(data.origin);
                                        let _ = FillRgn(dc, region, brush);
                                        let _ = DeleteObject(region.into());
                                        let _ = DeleteObject(brush.into());
                                }
                        }
                        let _ = EndPaint(hwnd, &ps);
                }
                return LRESULT(0);
        }
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
