//! Active-window focus border (niri's focus-ring): a topless, topmost,
//! click-through frame that outlines the focused window.
//!
//! The window itself is shaped into a ring (outer rect minus inner rect
//! via `SetWindowRgn`) and painted a solid color, which keeps the frame
//! path cheap — no per-pixel alpha needed. The app repositions it
//! whenever the focused window's geometry changes (including during
//! animations, driven from the animation tick).

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CombineRgn, CreateRectRgn, CreateRoundRectRgn, CreateSolidBrush, DeleteObject,
    EndPaint, FillRect, InvalidateRect, PAINTSTRUCT, RGN_DIFF, SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, SetLayeredWindowAttributes, SetWindowPos,
    ShowWindow, HWND_TOPMOST, LWA_ALPHA, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE,
    SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// Current ring color, reached from wnd_proc (main thread only).
struct ColorCell(std::cell::UnsafeCell<COLORREF>);
unsafe impl Sync for ColorCell {}

static COLOR: ColorCell = ColorCell(std::cell::UnsafeCell::new(COLORREF(0x00A3_AE7D)));

impl ColorCell {
    /// Safety: main thread only.
    unsafe fn set(&self, c: COLORREF) {
        unsafe { *self.0.get() = c }
    }

    /// Safety: main thread only.
    unsafe fn get(&self) -> COLORREF {
        unsafe { *self.0.get() }
    }
}

pub struct FocusBorder {
    hwnd: HWND,
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
            // A WS_EX_LAYERED window is not displayed until its
            // alpha/color key is set — without this the ring exists
            // but never shows.
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            Some(FocusBorder { hwnd })
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
    ) {
        let t = thickness.max(1);
        let r = radius.max(0);
        let (ow, oh) = (w + 2 * t, h + 2 * t);
        unsafe {
            COLOR.set(color);
            // Shape the window into the ring: full client minus the
            // inner rect. SetWindowRgn takes ownership of `ring`.
            // CreateRoundRectRgn's ellipse params are the diameter, so
            // pass 2*radius for a corner of that radius.
            let full = if r > 0 {
                CreateRoundRectRgn(0, 0, ow, oh, 2 * (r + t), 2 * (r + t))
            } else {
                CreateRectRgn(0, 0, ow, oh)
            };
            let inner = if r > 0 {
                CreateRoundRectRgn(t, t, t + w, t + h, 2 * r, 2 * r)
            } else {
                CreateRectRgn(t, t, t + w, t + h)
            };
            let ring = CreateRectRgn(0, 0, 0, 0);
            CombineRgn(Some(ring), Some(full), Some(inner), RGN_DIFF);
            let _ = DeleteObject(full.into());
            let _ = DeleteObject(inner.into());
            let _ = SetWindowRgn(self.hwnd, Some(ring), true);
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x - t,
                y - t,
                ow,
                oh,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
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

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == windows::Win32::UI::WindowsAndMessaging::WM_PAINT {
        unsafe { paint(hwnd) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        if hdc.is_invalid() {
            return;
        }
        let mut rc = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
        let brush = CreateSolidBrush(COLOR.get());
        FillRect(hdc, &rc, brush);
        let _ = DeleteObject(brush.into());
        let _ = EndPaint(hwnd, &ps);
    }
}
