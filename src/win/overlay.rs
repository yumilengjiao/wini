//! Workspace indicator overlay: a small borderless topmost window that
//! shows "Workspace N" briefly when the workspace changes (niri-style
//! feedback), then hides itself on a timer.
//!
//! Simple GDI painting (solid dark pill, white centered text) — no
//! UpdateLayeredWindow alpha, to keep the frame path cheap.

use windows::core::w;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect,
    SetBkMode, SetTextColor, DT_CENTER, DT_SINGLELINE, DT_VCENTER, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, KillTimer, RegisterClassW, SetTimer, SetWindowPos, ShowWindow,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE,
    WINDOW_STYLE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::win::monitor::Monitor;

/// Auto-hide timer id.
const TIMER_HIDE: usize = 2;
/// How long the indicator stays on screen.
const HIDE_MS: u32 = 1200;
/// Overlay size.
const WIDTH: i32 = 280;
const HEIGHT: i32 = 64;
/// Screen-transition (do-screen-transition) auto-end timer id.
const TIMER_TRANSITION: usize = 3;
/// How long the screen transition covers the monitor.
const TRANSITION_MS: u32 = 1000;

/// Current label, reached from wnd_proc (single-threaded: main thread).
struct LabelCell(std::cell::UnsafeCell<String>);
unsafe impl Sync for LabelCell {}

static LABEL: LabelCell = LabelCell(std::cell::UnsafeCell::new(String::new()));

/// Display-topology-change handler. The overlay is a regular
/// top-level window (unlike the message-only window), so it receives
/// the WM_DISPLAYCHANGE broadcast — we forward it to the app from
/// here.
type DisplayChangeHandler = Box<dyn Fn()>;
struct DisplayCell(std::cell::UnsafeCell<Option<DisplayChangeHandler>>);
unsafe impl Sync for DisplayCell {}

static DISPLAY_HANDLER: DisplayCell = DisplayCell(std::cell::UnsafeCell::new(None));

/// Invoked when a screen transition ends (its cover hides itself on
/// a timer); the app resumes window management here.
type TransitionEndHandler = Box<dyn Fn()>;
struct TransitionCell(std::cell::UnsafeCell<Option<TransitionEndHandler>>);
unsafe impl Sync for TransitionCell {}

static TRANSITION_END: TransitionCell = TransitionCell(std::cell::UnsafeCell::new(None));

impl TransitionCell {
    const fn new() -> Self {
        TransitionCell(std::cell::UnsafeCell::new(None))
    }

    /// Safety: main thread only.
    unsafe fn set(&self, handler: TransitionEndHandler) {
        unsafe { *self.0.get() = Some(handler) }
    }

    /// Safety: main thread only; must not outlive the next `set`.
    unsafe fn get(&self) -> Option<&TransitionEndHandler> {
        unsafe { (*self.0.get()).as_ref() }
    }
}

impl DisplayCell {
    const fn new() -> Self {
        DisplayCell(std::cell::UnsafeCell::new(None))
    }

    /// Safety: main thread only.
    unsafe fn set(&self, handler: DisplayChangeHandler) {
        unsafe { *self.0.get() = Some(handler) }
    }

    /// Safety: main thread only; must not outlive the next `set`.
    unsafe fn get(&self) -> Option<&DisplayChangeHandler> {
        unsafe { (*self.0.get()).as_ref() }
    }
}

impl LabelCell {
    const fn new() -> Self {
        LabelCell(std::cell::UnsafeCell::new(String::new()))
    }

    unsafe fn set(&self, s: String) {
        unsafe { *self.0.get() = s }
    }

    unsafe fn get(&self) -> &String {
        unsafe { &*self.0.get() }
    }
}

pub struct OverlayWindow {
    hwnd: HWND,
}

impl OverlayWindow {
    pub fn new() -> Option<Self> {
        let class_name = w!("wini_overlay");
        unsafe {
            let hinstance = GetModuleHandleW(None).ok()?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            // Click-through (WS_EX_TRANSPARENT) so the fullscreen
            // transition cover never eats clicks; never activates, no
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
            Some(OverlayWindow { hwnd })
        }
    }

    /// Show "Workspace N" centered at the top of `monitor`.
    pub fn show_workspace(&self, monitor: &Monitor, index_1based: usize) {
        unsafe {
            LABEL.set(format!("Workspace {index_1based}"));
            let cx = (monitor.work.left + monitor.work.right) / 2;
            let x = cx - WIDTH / 2;
            let y = monitor.work.top + 24;
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                WIDTH,
                HEIGHT,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(self.hwnd), None, true);
            let _ = SetTimer(Some(self.hwnd), TIMER_HIDE, HIDE_MS, None);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Cover `monitor` entirely with a black screen for a moment
    /// (niri's do-screen-transition: privacy for screenshot tools —
    /// the WM suspends management for the duration). When the cover
    /// hides itself, the transition-end handler runs.
    pub fn show_transition(&self, monitor: &Monitor) {
        unsafe {
            LABEL.set(String::new()); // no text, just the black fill
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                monitor.full.left,
                monitor.full.top,
                monitor.full.right - monitor.full.left,
                monitor.full.bottom - monitor.full.top,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(self.hwnd), None, true);
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
            let _ = SetTimer(Some(self.hwnd), TIMER_TRANSITION, TRANSITION_MS, None);
        }
    }

    /// Register the handler invoked when the display topology changes
    /// (WM_DISPLAYCHANGE broadcast — only top-level windows get it).
    pub fn set_display_change_handler(&self, handler: impl Fn() + 'static) {
        // Safety: main thread only; wnd_proc runs during dispatch on
        // the main thread.
        unsafe { DISPLAY_HANDLER.set(Box::new(handler)) }
    }

    /// Register the handler invoked when a screen transition ends
    /// (the cover window hid itself after TRANSITION_MS).
    pub fn set_transition_end_handler(&self, handler: impl Fn() + 'static) {
        // Safety: main thread only, same reasoning as above.
        unsafe { TRANSITION_END.set(Box::new(handler)) }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == windows::Win32::UI::WindowsAndMessaging::WM_DISPLAYCHANGE {
        // Safety: reads the handler on the same (main) thread that set
        // it, during message dispatch.
        unsafe {
            if let Some(handler) = DISPLAY_HANDLER.get() {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(handler));
            }
        }
        return LRESULT(0);
    }
    if msg == windows::Win32::UI::WindowsAndMessaging::WM_TIMER && wparam.0 == TIMER_HIDE {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_HIDE);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        return LRESULT(0);
    }
    if msg == windows::Win32::UI::WindowsAndMessaging::WM_TIMER && wparam.0 == TIMER_TRANSITION {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_TRANSITION);
            let _ = ShowWindow(hwnd, SW_HIDE);
            if let Some(handler) = TRANSITION_END.get() {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(handler));
            }
        }
        return LRESULT(0);
    }
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

        // Dark pill background.
        let bg = CreateSolidBrush(COLORREF(0x0020_2020)); // 0x00BBGGRR
        FillRect(hdc, &rc, bg);
        let _ = DeleteObject(bg.into());

        let label = LABEL.get().clone();
        let mut wide: Vec<u16> = label.encode_utf16().collect();

        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00F0_F0F0));
        let _ = DrawTextW(
            hdc,
            &mut wide,
            &mut rc,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );

        let _ = EndPaint(hwnd, &ps);
    }
}

/// Dynamic-name helper kept for future use (e.g. per-monitor labels).
#[allow(dead_code)]
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[allow(dead_code)]
pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    PCWSTR(buf.as_ptr())
}
