//! Hidden message-only window.
//!
//! The main thread needs an HWND to receive posted messages: the
//! low-level keyboard hook (running on a hook thread) marshals key
//! events here via `PostMessage`, and timers can be attached to it
//! later for driving animations.

use windows::core::w;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, RegisterClassW, SetTimer,
    SetWindowLongPtrW, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, GWLP_USERDATA,
};

use crate::input::{self, KeyEvent, MouseEvent};
use std::collections::HashMap;

/// Custom message: a key event forwarded from the keyboard hook.
pub const WM_APP_KEY: u32 = 0x8000; // WM_APP
/// Custom message: a mouse event forwarded from a hook.
pub const WM_APP_MOUSE: u32 = 0x8001;
/// Custom message: an animation frame tick from the high-resolution
/// timer thread. `wparam` carries the timer id.
pub const WM_APP_ANIM_FRAME: u32 = 0x8002;
/// Animation frame timer id.
pub const TIMER_ANIM: usize = 1;
/// Config hot-reload poll timer id.
pub const TIMER_CONFIG: usize = 3;
/// Animation frame period (ms). ~60 Hz.
pub const ANIM_TIMER_MS: u32 = 16;
/// Config file poll period (ms).
pub const CONFIG_TIMER_MS: u32 = 1000;

type KeyHandler = Box<dyn Fn(KeyEvent)>;
type TimerHandler = Box<dyn Fn()>;
type MouseHandler = Box<dyn Fn(MouseEvent)>;

/// The single instance owning the key handler; wnd_proc needs to reach
/// it from a static context (single-threaded: all on the main thread).
/// Wrapped in a struct with an unsafe Sync impl instead of `static mut`
/// (banned to reference in edition 2024).
struct HandlerCell(std::cell::UnsafeCell<Option<KeyHandler>>);
unsafe impl Sync for HandlerCell {}

struct TimerHandlerCell(std::cell::UnsafeCell<Option<HashMap<usize, TimerHandler>>>);
unsafe impl Sync for TimerHandlerCell {}

struct MouseHandlerCell(std::cell::UnsafeCell<Option<MouseHandler>>);
unsafe impl Sync for MouseHandlerCell {}

impl HandlerCell {
    const fn new() -> Self {
        HandlerCell(std::cell::UnsafeCell::new(None))
    }

    /// Safety: only call from the main thread.
    unsafe fn set(&self, handler: KeyHandler) {
        unsafe { *self.0.get() = Some(handler) }
    }

    /// Safety: only call from the main thread; the returned reference
    /// must not outlive the next `set` call.
    unsafe fn get(&self) -> Option<&KeyHandler> {
        unsafe { (*self.0.get()).as_ref() }
    }

    /// Safety: only call from the main thread.
    unsafe fn take(&self) -> Option<KeyHandler> {
        unsafe { (*self.0.get()).take() }
    }
}

static KEY_HANDLER: HandlerCell = HandlerCell::new();
static TIMER_HANDLER: TimerHandlerCell = TimerHandlerCell::new();
static MOUSE_HANDLER: MouseHandlerCell = MouseHandlerCell::new();

impl MouseHandlerCell {
    const fn new() -> Self {
        MouseHandlerCell(std::cell::UnsafeCell::new(None))
    }

    /// Safety: main thread only.
    unsafe fn set(&self, handler: MouseHandler) {
        unsafe { *self.0.get() = Some(handler) }
    }

    /// Safety: main thread only.
    unsafe fn get(&self) -> Option<&MouseHandler> {
        unsafe { (*self.0.get()).as_ref() }
    }

    /// Safety: main thread only.
    unsafe fn take(&self) -> Option<MouseHandler> {
        unsafe { (*self.0.get()).take() }
    }
}

impl TimerHandlerCell {
    const fn new() -> Self {
        TimerHandlerCell(std::cell::UnsafeCell::new(None))
    }

    /// Safety: main thread only.
    unsafe fn insert(&self, id: usize, handler: TimerHandler) {
        unsafe {
            (*self.0.get())
                .get_or_insert_with(HashMap::new)
                .insert(id, handler);
        }
    }

    /// Safety: main thread only.
    unsafe fn get(&self, id: usize) -> Option<&TimerHandler> {
        unsafe { (*self.0.get()).as_ref()?.get(&id) }
    }

    /// Safety: main thread only.
    unsafe fn take(&self) -> Option<HashMap<usize, TimerHandler>> {
        unsafe { (*self.0.get()).take() }
    }
}

pub struct MessageWindow {
    hwnd: HWND,
}

impl MessageWindow {
    /// Create a message-only window owned by the calling thread. The
    /// thread must pump messages for deliveries to arrive.
    pub fn new() -> Option<Self> {
        let class_name = w!("yumi_wini_msg");

        unsafe {
            let hinstance = GetModuleHandleW(None).ok()?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            // Registering twice (e.g. in tests) fails harmlessly.
            let _ = RegisterClassW(&wc);

            // Message-only child window: never visible, never activated,
            // invisible to hits. HWND_MESSAGE = -3. Note: no exotic
            // ex-styles — WS_EX_LAYERED makes message-only creation
            // fail outright (Win32 error 1410), and transparency is
            // meaningless for a window that never renders or gets
            // hit-tested.
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND(-3isize as *mut _)),
                None,
                Some(hinstance.into()),
                None,
            );
            if hwnd.is_err() {
                let err = windows::Win32::Foundation::GetLastError();
                log::error!(
                    "message window CreateWindowExW failed: Win32 error {}",
                    err.0
                );
                return None;
            }
            let hwnd = hwnd.ok()?;

            Some(MessageWindow { hwnd })
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Set the handler invoked (on the main thread, during message
    /// dispatch) for each key event forwarded by the keyboard hook.
    pub fn set_key_handler(&self, handler: impl Fn(KeyEvent) + 'static) {
        // Safety: main thread only; wnd_proc runs on the main thread
        // during message dispatch.
        unsafe {
            KEY_HANDLER.set(Box::new(handler));
        }
    }

    /// Start a periodic timer whose handler runs on the main thread
    /// during message dispatch. `id` identifies it (TIMER_ANIM, ...).
    /// Uses WM_TIMER: ~15.6ms granularity, delivered only when the
    /// queue is idle — fine for slow polls (config), not for animation.
    pub fn start_timer(&self, id: usize, period_ms: u32, handler: impl Fn() + 'static) {
        // Safety: main thread only; wnd_proc runs on the main thread
        // during message dispatch.
        unsafe {
            TIMER_HANDLER.insert(id, Box::new(handler));
            let _ = SetTimer(Some(self.hwnd), id, period_ms, None);
        }
    }

    /// Start a high-resolution periodic timer: a dedicated thread with
    /// a CREATE_WAITABLE_TIMER_HIGH_RESOLUTION timer posts
    /// WM_APP_ANIM_FRAME every period, and the handler runs on the
    /// main thread during message dispatch like `start_timer`.
    /// Posted messages are normal-priority (never starved by WinEvents
    /// like WM_TIMER), and the high-res timer isn't quantized to the
    /// ~15.6ms scheduler tick — this is the animation frame clock.
    pub fn start_hires_timer(&self, id: usize, period_ms: u32, handler: impl Fn() + 'static) {
        // Safety: main thread only; wnd_proc runs on the main thread
        // during message dispatch.
        unsafe {
            TIMER_HANDLER.insert(id, Box::new(handler));
        }
        let hwnd = self.hwnd;
        // HWND is not Send: pass the raw handle as an isize. The thread
        // lives for the process lifetime; after the message window is
        // destroyed, PostMessage simply fails harmlessly.
        let target = hwnd.0 as isize;
        let spawn = std::thread::Builder::new()
            .name("anim-clock".into())
            .spawn(move || anim_clock_thread(target, id, period_ms));
        if spawn.is_err() {
            // No threads: fall back to a plain timer.
            // Safety: main thread only.
            unsafe {
                let _ = SetTimer(Some(hwnd), id, period_ms, None);
            }
        }
    }

    /// Set the handler invoked (on the main thread) for each mouse
    /// event forwarded by the low-level mouse hook.
    pub fn set_mouse_handler(&self, handler: impl Fn(MouseEvent) + 'static) {
        // Safety: main thread only; wnd_proc runs on the main thread
        // during message dispatch.
        unsafe {
            MOUSE_HANDLER.set(Box::new(handler));
        }
    }
}

/// Body of the high-resolution clock thread: post `WM_APP_ANIM_FRAME`
/// to the message window every `period_ms`, as accurately as the OS
/// allows. `target` is the message window's HWND address (HWND itself
/// is not Send).
fn anim_clock_thread(target: isize, id: usize, period_ms: u32) {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::System::Threading::{
        CreateWaitableTimerExW, SetWaitableTimer, WaitForSingleObject,
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
    };
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

    let hwnd = HWND(target as *mut _);
    let timer = (|| -> windows::core::Result<_> {
        let t = unsafe {
            CreateWaitableTimerExW(
                None,
                windows::core::PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                // TIMER_ALL_ACCESS
                0x001F_0003,
            )?
        };
        // Negative due time = relative, in 100ns units: fire almost
        // immediately, then every period_ms.
        let due: i64 = -10_000;
        unsafe { SetWaitableTimer(t, &due, period_ms as i32, None, None, false)? };
        Ok(t)
    })();
    let Ok(timer) = timer else {
        // No high-resolution timers (pre Win10 1803): a steady sleep
        // loop still keeps a far more regular cadence than coalesced
        // WM_TIMER.
        loop {
            std::thread::sleep(std::time::Duration::from_millis(period_ms as u64));
            unsafe {
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_APP_ANIM_FRAME,
                    WPARAM(id),
                    LPARAM(0),
                );
            }
        }
    };
    loop {
        let r = unsafe { WaitForSingleObject(timer, u32::MAX) };
        if r != WAIT_OBJECT_0 {
            // Handle closed / error: stop ticking.
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(timer) };
            return;
        }
        unsafe {
            let _ = PostMessageW(
                Some(hwnd),
                WM_APP_ANIM_FRAME,
                WPARAM(id),
                LPARAM(0),
            );
        }
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        unsafe {
            let _ = KEY_HANDLER.take();
            let _ = MOUSE_HANDLER.take();
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_APP_KEY {
        // Safety: reads the handler on the same (main) thread that set
        // it, outside any mutation window.
        unsafe {
            if let Some(handler) = KEY_HANDLER.get() {
                let ev = input::decode_message(wparam.0, lparam.0);
                // Never let a panic cross the FFI boundary.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(ev)));
            }
        }
        return LRESULT(0);
    }
    if msg == WM_APP_MOUSE {
        // Safety: reads the handler on the same (main) thread that set
        // it, outside any mutation window.
        unsafe {
            if let Some(handler) = MOUSE_HANDLER.get() {
                let ev = input::decode_mouse_message(wparam.0, lparam.0);
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(ev)));
            }
        }
        return LRESULT(0);
    }
    if msg == WM_APP_ANIM_FRAME {
        // Safety: reads the handler on the same (main) thread that set
        // it, outside any mutation window.
        unsafe {
            if let Some(handler) = TIMER_HANDLER.get(wparam.0) {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(handler));
            }
        }
        return LRESULT(0);
    }
    if msg == windows::Win32::UI::WindowsAndMessaging::WM_TIMER {
        unsafe {
            if let Some(handler) = TIMER_HANDLER.get(wparam.0) {
                let _ =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(handler));
                return LRESULT(0);
            }
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Convenience: build a PCWSTR from a static utf16 literal is done via
/// windows::core::w; this helper exists for dynamic names later.
#[allow(dead_code)]
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[allow(dead_code)]
pub fn pcwstr(buf: &[u16]) -> PCWSTR {
    PCWSTR(buf.as_ptr())
}

#[allow(dead_code)]
pub fn window_user_data(hwnd: HWND) -> isize {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) }
}

#[allow(dead_code)]
pub fn set_window_user_data(hwnd: HWND, value: isize) {
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, value);
    }
}
