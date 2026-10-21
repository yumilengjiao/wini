//! Click-through, no-activate tab indicator overlay. Geometry and hit targets
//! are supplied by the app; painting is confined to shaped GDI regions.

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
        BeginPaint, CombineRgn, CreateRectRgn, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
        EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, RGN_OR, SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW,
        HWND_TOPMOST, LWA_ALPHA, RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
        SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_PAINT,
        WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabMark {
        pub id: isize,
        pub rect: (i32, i32, i32, i32),
        pub color: u32,
        pub radius: i32,
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
                                let (x, y, w, h) = mark.rect;
                                let (x, y) = (x - left, y - top);
                                let r = mark.radius.max(0);
                                let part = if r > 0 {
                                        CreateRoundRectRgn(x, y, x + w, y + h, 2 * r, 2 * r)
                                } else {
                                        CreateRectRgn(x, y, x + w, y + h)
                                };
                                CombineRgn(Some(region), Some(region), Some(part), RGN_OR);
                                let _ = DeleteObject(part.into());
                        }
                        if SetWindowRgn(self.hwnd, Some(region), false) == 0 {
                                let _ = DeleteObject(region.into());
                        }
                        let _ = SetWindowPos(
                                self.hwnd,
                                Some(HWND_TOPMOST),
                                left,
                                top,
                                right - left,
                                bottom - top,
                                SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                        self.visible = true;
                        let _ = InvalidateRect(Some(self.hwnd), None, false);
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

extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
) -> LRESULT {
        if msg == WM_PAINT {
                unsafe {
                        let mut ps = PAINTSTRUCT::default();
                        let dc = BeginPaint(hwnd, &mut ps);
                        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const PaintData;
                        if !pointer.is_null() && !dc.is_invalid() {
                                let data = &*pointer;
                                for mark in &data.marks {
                                        let (x, y, w, h) = mark.rect;
                                        let rgb = mark.color;
                                        let bgr = ((rgb & 0xff) << 16)
                                                | (rgb & 0xff00)
                                                | ((rgb >> 16) & 0xff);
                                        let brush = CreateSolidBrush(COLORREF(bgr));
                                        let rect = RECT {
                                                left: x - data.origin.0,
                                                top: y - data.origin.1,
                                                right: x - data.origin.0 + w,
                                                bottom: y - data.origin.1 + h,
                                        };
                                        FillRect(dc, &rect, brush);
                                        let _ = DeleteObject(brush.into());
                                }
                        }
                        let _ = EndPaint(hwnd, &ps);
                }
                return LRESULT(0);
        }
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
