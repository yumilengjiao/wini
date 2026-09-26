//! Global mouse input: low-level hook for wheel binds and hover focus.
//!
//! A `WH_MOUSE_LL` hook sees mouse events system-wide. Wheel events are
//! turned into niri-style `WheelScroll*` pseudo-keys, so combos like
//! `Mod+WheelScrollDown` can be bound in the config like any key
//! (niri's default: wheel moves column focus, which scrolls the view).
//! Mouse moves feed the optional focus-follows-mouse mode. Everything
//! is forwarded to the main thread via `PostMessage` (`WM_APP_MOUSE`).
//!
//! The hook never swallows mouse moves; wheel events are swallowed only
//! when they match a bind (so plain scrolling keeps reaching apps).

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MSLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL,
};

use crate::config::ModKey;
use crate::input::{matches_a_bind, VK_WHEEL_DOWN, VK_WHEEL_LEFT, VK_WHEEL_RIGHT, VK_WHEEL_UP};
use crate::win::msg_window::WM_APP_MOUSE;

/// WM_MOUSEMOVE / WM_MOUSEWHEEL / WM_MOUSEHWHEEL.
const WM_MOUSEMOVE: usize = 0x0200;
const WM_LBUTTONDOWN: usize = 0x0201;
const WM_RBUTTONDOWN: usize = 0x0204;
const WM_MOUSEWHEEL: usize = 0x020A;
const WM_MOUSEHWHEEL: usize = 0x020E;

/// What the event is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Move,
    /// Vertical wheel (up/down).
    WheelV,
    /// Horizontal wheel (left/right).
    WheelH,
    /// Left button pressed.
    LButtonDown,
    /// Right button pressed.
    RButtonDown,
}

/// A mouse event forwarded from the hook thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub kind: MouseKind,
    /// Screen coordinates.
    pub x: i32,
    pub y: i32,
    /// Wheel delta in notch units (WHEEL_DELTA=120), 0 for moves.
    pub notches: i32,
    pub shift: bool,
    pub ctrl: bool,
    pub mod_held: bool,
}

impl MouseEvent {
    /// The niri-style pseudo key this event acts as (wheel only).
    pub fn wheel_vk(&self) -> Option<u32> {
        Some(match (self.kind, self.notches) {
            (MouseKind::WheelV, n) if n < 0 => VK_WHEEL_DOWN,
            (MouseKind::WheelV, n) if n > 0 => VK_WHEEL_UP,
            (MouseKind::WheelH, n) if n > 0 => VK_WHEEL_RIGHT,
            (MouseKind::WheelH, n) if n < 0 => VK_WHEEL_LEFT,
            _ => return None,
        })
    }
}

/// Live mouse-hook state shared between the hook thread and main.
struct MouseHookState {
    mod_key: ModKey,
    /// The hidden message window (raw bits so the static stays Send).
    target: isize,
}

static MOUSE_HOOK_STATE: Mutex<Option<MouseHookState>> = Mutex::new(None);
static MOUSE_HOOK_PTR: AtomicUsize = AtomicUsize::new(0);

/// Last forwarded move position, packed as `(x << 32) | y` — used to
/// throttle the move flood (high-rate mice produce ~1000 events/s).
static LAST_MOVE_POS: AtomicI64 = AtomicI64::new(i64::MIN);

/// Screen rects in which mouse BUTTONS are intercepted (monitors
/// with an open overview). The overview backdrop is a colorkey
/// layered window — fully transparent to hit-testing — so button
/// presses inside it must be captured here at the hook instead of by
/// the host window. Intercepted presses are forwarded to the main
/// thread and SWALLOWED (the desktop and the parked real windows
/// below must not see them).
static OVERVIEW_RECTS: Mutex<Vec<(i32, i32, i32, i32)>> = Mutex::new(Vec::new());

/// Replace the button-interception regions (called on overview
/// open/close/drop).
pub fn set_overview_regions(rects: Vec<(i32, i32, i32, i32)>) {
    *OVERVIEW_RECTS.lock().unwrap() = rects;
}

/// Install the low-level mouse hook. `target` receives `WM_APP_MOUSE`
/// messages; see [`encode`]/[`crate::input::decode_mouse_message`].
pub fn install(mod_key: ModKey, target: HWND) -> Result<(), String> {
    *MOUSE_HOOK_STATE.lock().unwrap() = Some(MouseHookState {
        mod_key,
        target: target.0 as isize,
    });
    unsafe {
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0)
            .map_err(|e| format!("SetWindowsHookExW(WH_MOUSE_LL): {e}"))?;
        MOUSE_HOOK_PTR.store(hook.0 as usize, Ordering::SeqCst);
    }
    log::info!("mouse hook installed");
    Ok(())
}

/// Change the Mod key without reinstalling the hook (config hot
/// reload).
pub fn set_mod_key(mod_key: ModKey) {
    if let Some(s) = MOUSE_HOOK_STATE.lock().unwrap().as_mut() {
        s.mod_key = mod_key;
    }
}

/// Remove the hook (called on shutdown).
pub fn uninstall() {
    let handle = MOUSE_HOOK_PTR.swap(0, Ordering::SeqCst);
    if handle != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(windows::Win32::UI::WindowsAndMessaging::HHOOK(
                handle as *mut _,
            ));
        }
        log::info!("mouse hook removed");
    }
    *MOUSE_HOOK_STATE.lock().unwrap() = None;
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return call_next(lparam, code, wparam);
    }
    let msg = wparam.0;
    let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };

    let (kind, notches) = match msg {
        WM_MOUSEMOVE => (MouseKind::Move, 0),
        WM_MOUSEWHEEL => (MouseKind::WheelV, ((ms.mouseData >> 16) as i16) as i32 / 120),
        WM_MOUSEHWHEEL => (MouseKind::WheelH, ((ms.mouseData >> 16) as i16) as i32 / 120),
        WM_LBUTTONDOWN => (MouseKind::LButtonDown, 0),
        WM_RBUTTONDOWN => (MouseKind::RButtonDown, 0),
        _ => return call_next(lparam, code, wparam),
    };

    // Overview backdrop regions: button presses are overview UI —
    // forward and swallow them (see OVERVIEW_RECTS).
    if matches!(kind, MouseKind::LButtonDown | MouseKind::RButtonDown) {
        let inside = OVERVIEW_RECTS
            .lock()
            .unwrap()
            .iter()
            .any(|&(l, t, r, b)| ms.pt.x >= l && ms.pt.x < r && ms.pt.y >= t && ms.pt.y < b);
        if !inside {
            return call_next(lparam, code, wparam);
        }
        let Some(target) = MOUSE_HOOK_STATE.lock().unwrap().as_ref().map(|s| s.target) else {
            return call_next(lparam, code, wparam);
        };
        let ev = MouseEvent {
            kind,
            x: ms.pt.x,
            y: ms.pt.y,
            notches: 0,
            shift: key_down(VK_SHIFT),
            ctrl: key_down(VK_CONTROL),
            mod_held: false,
        };
        forward(target, &ev, ms.pt.x, ms.pt.y);
        return LRESULT(1);
    }

    // Throttle moves: skip if we forwarded (nearly) this position
    // already. Moves are never swallowed.
    if kind == MouseKind::Move {
        let packed = ((ms.pt.x as i64) << 32) | (ms.pt.y as u32 as u64) as i64;
        let last = LAST_MOVE_POS.load(Ordering::Relaxed);
        let dx = ((packed >> 32) - (last >> 32)).abs();
        let dy = ((packed & 0xFFFF_FFFF) - (last & 0xFFFF_FFFF)).abs();
        if last != i64::MIN && dx <= 2 && dy <= 2 {
            return call_next(lparam, code, wparam);
        }
        LAST_MOVE_POS.store(packed, Ordering::Relaxed);
    }

    // Snapshot modifiers on the hook thread.
    let shift = key_down(VK_SHIFT);
    let ctrl = key_down(VK_CONTROL);
    let mod_vk = match MOUSE_HOOK_STATE.lock().unwrap().as_ref().map(|s| s.mod_key.clone()) {
        Some(ModKey::Super) => VK_LWIN,
        Some(ModKey::Ctrl) => VK_CONTROL,
        _ => VK_MENU,
    };
    let mod_held = key_down(mod_vk);

    let Some(target) = MOUSE_HOOK_STATE.lock().unwrap().as_ref().map(|s| s.target) else {
        return call_next(lparam, code, wparam);
    };

    // A wheel event that matches a bind is consumed system-wide;
    // everything else keeps flowing to the app under the cursor.
    if let MouseKind::WheelV | MouseKind::WheelH = kind {
        let ev = wheel_event(kind, notches, shift, ctrl, mod_held);
        if let Some(vk) = ev.wheel_vk()
            && matches_a_bind(vk, shift, ctrl, mod_held)
        {
            forward(target, &ev, ms.pt.x, ms.pt.y);
            return LRESULT(1);
        }
        return call_next(lparam, code, wparam);
    }

    let ev = MouseEvent {
        kind,
        x: ms.pt.x,
        y: ms.pt.y,
        notches,
        shift,
        ctrl,
        mod_held,
    };
    forward(target, &ev, ms.pt.x, ms.pt.y);
    call_next(lparam, code, wparam)
}

fn wheel_event(kind: MouseKind, notches: i32, shift: bool, ctrl: bool, mod_held: bool) -> MouseEvent {
    MouseEvent {
        kind,
        x: 0,
        y: 0,
        notches,
        shift,
        ctrl,
        mod_held,
    }
}

/// Post the event to the message window.
fn forward(target: isize, ev: &MouseEvent, x: i32, y: i32) {
    let wparam = encode_wparam(ev);
    let lparam = (((x as u32 as u64) << 32) | y as u32 as u64) as isize;
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(HWND(target as *mut _)),
            WM_APP_MOUSE,
            WPARAM(wparam),
            LPARAM(lparam),
        );
    }
}

fn encode_wparam(ev: &MouseEvent) -> usize {
    let kind_bits = match ev.kind {
        MouseKind::Move => 0usize,
        MouseKind::WheelV => 1,
        MouseKind::WheelH => 2,
        MouseKind::LButtonDown => 3,
        MouseKind::RButtonDown => 4,
    };
    kind_bits
        | ((ev.shift as usize) << 3)
        | ((ev.ctrl as usize) << 4)
        | ((ev.mod_held as usize) << 5)
        | (((ev.notches as i8) as u8 as usize) << 16)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetKeyState(vk.0 as i32) as u16) & 0x8000 != 0 }
}

fn call_next(lparam: LPARAM, code: i32, wparam: WPARAM) -> LRESULT {
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::CallNextHookEx(None, code, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyEvent;

    fn decode(w: usize, l: isize) -> MouseEvent {
        crate::input::decode_mouse_message(w, l)
    }

    #[test]
    fn move_roundtrip() {
        // x = -100 (negative, multi-monitor), y = 200.
        let l = (((-100i32 as u32 as u64) << 32) | 200u32 as u64) as isize;
        let ev = decode(0, l);
        assert_eq!(ev.kind, MouseKind::Move);
        assert_eq!((ev.x, ev.y), (-100, 200));
        assert_eq!(ev.notches, 0);
        assert!(!ev.shift && !ev.ctrl && !ev.mod_held);
    }

    #[test]
    fn wheel_roundtrip() {
        // Vertical wheel, 2 notches up, Mod+Shift held.
        let ev = MouseEvent {
            kind: MouseKind::WheelV,
            x: 5,
            y: 6,
            notches: 2,
            shift: true,
            ctrl: false,
            mod_held: true,
        };
        let w = encode_wparam(&ev);
        let l = (((5i32 as u32 as u64) << 32) | 6u32 as u64) as isize;
        assert_eq!(decode(w, l), ev);
        assert_eq!(ev.wheel_vk(), Some(VK_WHEEL_UP));

        let down = MouseEvent { notches: -1, ..ev };
        assert_eq!(down.wheel_vk(), Some(VK_WHEEL_DOWN));

        let h = MouseEvent {
            kind: MouseKind::WheelH,
            notches: 1,
            ..ev
        };
        assert_eq!(h.wheel_vk(), Some(VK_WHEEL_RIGHT));
    }

    #[test]
    fn wheel_key_names_bind() {
        let binds = crate::config::Config::default().binds;
        let ev = KeyEvent {
            vk: VK_WHEEL_DOWN,
            pressed: true,
            shift: false,
            ctrl: false,
            mod_held: true,
        };
        assert_eq!(
            crate::input::action_for(&binds, &ev),
            Some(crate::config::Action::FocusColumnRight)
        );
    }
}
