//! Global keyboard input: low-level hook + bind matching.
//!
//! A `WH_KEYBOARD_LL` hook sees keystrokes system-wide, before any app.
//! The hook callback runs on a dedicated thread installed by Windows, so
//! it must do minimal work and forward everything to the main thread via
//! `PostMessage` to our hidden window (`WM_APP_KEY`).
//!
//! Combo syntax matches niri: `Mod+Shift+H`, `Mod+Left`, `Mod+Q`.
//! Key names are XKB-ish (`H`, `Left`, `Page_Up`, `Return`, `space`),
//! plus the niri mouse-scroll pseudo-keys (`WheelScrollDown`, ...).

pub mod mouse;

pub use mouse::{MouseEvent, MouseKind};

use std::sync::atomic::Ordering;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MapVirtualKeyW, MAPVK_VSC_TO_VK_EX, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU,
    VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, WH_KEYBOARD_LL,
};

use crate::config::{Action, Bind, ModKey};
use crate::win::msg_window::WM_APP_KEY;

/// Pseudo virtual-key codes for mouse-wheel scroll directions (niri's
/// WheelScroll* keys; bindable like any key).
pub const VK_WHEEL_DOWN: u32 = 0xE1;
pub const VK_WHEEL_UP: u32 = 0xE2;
pub const VK_WHEEL_LEFT: u32 = 0xE3;
pub const VK_WHEEL_RIGHT: u32 = 0xE4;

/// A key event forwarded from the hook thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// Windows virtual-key code.
    pub vk: u32,
    /// True on press, false on release.
    pub pressed: bool,
    /// Shift held.
    pub shift: bool,
    /// Ctrl held.
    pub ctrl: bool,
    /// Mod key (per config) held.
    pub mod_held: bool,
}

impl KeyEvent {
    /// Lowercase niri-style key name for a VK code.
    pub fn key_name(vk: u32) -> String {
        // Names niri users expect; extended keys arrive as their base VK
        // after MapVirtualKey remap in the hook.
        match vk {
            0x08 => "Backspace".into(),
            0x09 => "Tab".into(),
            0x0D => "Return".into(),
            0x13 => "Pause".into(),
            0x14 => "Caps_Lock".into(),
            0x1B => "Escape".into(),
            0x20 => "space".into(),
            0x21 => "Page_Up".into(),
            0x22 => "Page_Down".into(),
            0x23 => "End".into(),
            0x24 => "Home".into(),
            0x25 => "Left".into(),
            0x26 => "Up".into(),
            0x27 => "Right".into(),
            0x28 => "Down".into(),
            0x2C => "Print".into(),
            0x2D => "Insert".into(),
            0x2E => "Delete".into(),
            0x5B => "Super_L".into(),
            0x5C => "Super_R".into(),
            // OEM punctuation keys (niri/XKB-ish names): these are
            // bindable ("Slash", "Minus", ...) — without the mapping
            // they'd come out as "VK_BF" and never match a combo.
            0xBA => "Semicolon".into(),
            0xBB => "Equal".into(),
            0xBC => "Comma".into(),
            0xBD => "Minus".into(),
            0xBE => "Period".into(),
            0xBF => "Slash".into(),
            0xC0 => "Grave".into(),
            0xDB => "BracketLeft".into(),
            0xDC => "Backslash".into(),
            0xDD => "BracketRight".into(),
            0xDE => "Quote".into(),
            0x60..=0x69 => format!("KP_{}", vk - 0x60),
            0x6A => "KP_Multiply".into(),
            0x6B => "KP_Add".into(),
            0x6D => "KP_Subtract".into(),
            0x6E => "KP_Decimal".into(),
            0x6F => "KP_Divide".into(),
            0x70..=0x87 => format!("F{}", vk - 0x6F),
            0xA0 => "Shift_L".into(),
            0xA1 => "Shift_R".into(),
            0xA2 => "Control_L".into(),
            0xA3 => "Control_R".into(),
            0xA4 => "Alt_L".into(),
            0xA5 => "Alt_R".into(),
            VK_WHEEL_DOWN => "WheelScrollDown".into(),
            VK_WHEEL_UP => "WheelScrollUp".into(),
            VK_WHEEL_LEFT => "WheelScrollLeft".into(),
            VK_WHEEL_RIGHT => "WheelScrollRight".into(),
            _ => {
                // Letters/digits: their ASCII name.
                let c = vk as u8;
                if c.is_ascii_alphanumeric() {
                    (c as char).to_string()
                } else {
                    format!("VK_{vk:02X}")
                }
            }
        }
    }
}

/// Live keyboard-hook state shared between the hook thread and main.
struct HookState {
    /// The configured Mod key.
    mod_key: ModKey,
    /// The hidden window to post forwarded events to (stored as raw
    /// bits so the static Mutex stays Send).
    target: isize,
}

static HOOK_STATE: Mutex<Option<HookState>> = Mutex::new(None);

/// HHOOK is a raw pointer; store its address as usize so the static
/// stays Send+Sync.
static HOOK_HANDLE_PTR: AtomicUsize = AtomicUsize::new(0);

/// Install the low-level keyboard hook. `target` receives `WM_APP_KEY`
/// with `wparam = pressed(1/0)` and `lparam = KeyEvent bits`:
/// `[ vk:32 | shift:1 | ctrl:1 | mod:1 ]` (packed low 35 bits).
pub fn install(mod_key: ModKey, target: HWND) -> Result<(), String> {
    *HOOK_STATE.lock().unwrap() = Some(HookState {
        mod_key: mod_key.clone(),
        target: target.0 as isize,
    });

    unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)
            .map_err(|e| format!("SetWindowsHookExW(WH_KEYBOARD_LL): {e}"))?;
        HOOK_HANDLE_PTR.store(hook.0 as usize, Ordering::SeqCst);
    }
    log::info!("keyboard hook installed");
    // Mouse: wheel binds + optional focus-follows-mouse. Installed on
    // the same thread (LL hooks are dispatched by our message loop).
    mouse::install(mod_key.clone(), target)?;
    Ok(())
}

/// Change the Mod key without reinstalling the hooks (config hot
/// reload). The hook threads read it live from the shared state.
pub fn set_mod_key(mod_key: ModKey) {
    if let Some(s) = HOOK_STATE.lock().unwrap().as_mut() {
        s.mod_key = mod_key.clone();
    }
    mouse::set_mod_key(mod_key);
}

/// Remove the hook (called on shutdown).
pub fn uninstall() {
    let handle = HOOK_HANDLE_PTR.swap(0, Ordering::SeqCst);
    if handle != 0 {
        unsafe {
            let _ = UnhookWindowsHookEx(windows::Win32::UI::WindowsAndMessaging::HHOOK(
                handle as *mut _,
            ));
        }
        log::info!("keyboard hook removed");
    }
    mouse::uninstall();
    *HOOK_STATE.lock().unwrap() = None;
}

unsafe extern "system" fn hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(lparam, code, wparam) };
    }

    // LLKHF_INJECTED: events we (or other apps) synthesized.
    let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    let pressed = wparam.0 == 0x0100 /* WM_KEYDOWN */ || wparam.0 == 0x0104 /* WM_SYSKEYDOWN */;
    let released = wparam.0 == 0x0101 /* WM_KEYUP */ || wparam.0 == 0x0105 /* WM_SYSKEYUP */;
    if !pressed && !released {
        return unsafe { CallNextHookEx(lparam, code, wparam) };
    }

    // Snapshot modifiers with GetKeyState on the hook thread.
    let shift = key_down(VK_SHIFT);
    let ctrl = key_down(VK_CONTROL);
    let mod_vk = match HOOK_STATE.lock().unwrap().as_ref().map(|s| s.mod_key.clone()) {
        Some(ModKey::Super) => VK_LWIN,
        Some(ModKey::Ctrl) => VK_CONTROL,
        _ => VK_MENU,
    };
    let mod_held = key_down(mod_vk);

    // The key event itself changes modifier state; the hook fires before
    // the state updates, so incorporate the event key.
    let vk = kb.vkCode;
    let (shift, ctrl, mod_held) = match vk {
        0x10 | 0xA0 | 0xA1 => (pressed, ctrl, mod_held),
        0x11 | 0xA2 | 0xA3 => (shift, pressed, mod_held),
        0x12 | 0xA4 | 0xA5 | 0x5B | 0x5C => {
            if mod_vk.0 as u32 == vk {
                (shift, ctrl, pressed)
            } else {
                (shift, ctrl, mod_held)
            }
        }
        _ => (shift, ctrl, mod_held),
    };

    let target = HOOK_STATE.lock().unwrap().as_ref().map(|s| s.target);
    if let Some(target) = target {
        let packed = (vk as u64)
            | ((shift as u64) << 32)
            | ((ctrl as u64) << 33)
            | ((mod_held as u64) << 34);
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(HWND(target as *mut _)),
                WM_APP_KEY,
                WPARAM(pressed as usize),
                LPARAM(packed as isize),
            );
        }
        // If this key press matches a bind, swallow it system-wide.
        // (Matching happens on the main thread after delivery; the
        // swallow decision must be synchronous, so match here too.)
        if pressed && matches_a_bind(vk, shift, ctrl, mod_held) {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(lparam, code, wparam) }
}

// The hook-side combo table for synchronous swallow decisions.
static MATCHING_COMBOS: Mutex<Vec<ComboKeys>> = Mutex::new(Vec::new());

#[derive(Debug, Clone)]
struct ComboKeys {
    key: String,
    shift: bool,
    ctrl: bool,
    uses_mod: bool,
}

/// Update the hook-side combo table from config binds.
pub fn update_binds(binds: &[Bind]) {
    let mut combos = MATCHING_COMBOS.lock().unwrap();
    combos.clear();
    for b in binds {
        // Parse "Mod+Shift+H" style combos.
        let mut key = String::new();
        let mut shift = false;
        let mut ctrl = false;
        let mut uses_mod = false;
        for part in b.combo.split('+') {
            match part {
                "Mod" => uses_mod = true,
                "Shift" => shift = true,
                "Ctrl" | "Control" => ctrl = true,
                k => key = k.to_string(),
            }
        }
        if key.is_empty() {
            continue;
        }
        combos.push(ComboKeys {
            key,
            shift,
            ctrl,
            uses_mod,
        });
    }
    log::debug!("hook now matching {} combos", combos.len());
}

pub(crate) fn matches_a_bind(vk: u32, shift: bool, ctrl: bool, mod_held: bool) -> bool {
    let name = KeyEvent::key_name(vk);
    let combos = MATCHING_COMBOS.lock().unwrap();
    combos.iter().any(|c| {
        c.key == name
            && c.shift == shift
            && c.ctrl == ctrl
            && (!c.uses_mod || mod_held)
            // A combo without Mod must not fire while Mod is held
            // (that would be a different combo).
            && (c.uses_mod || !mod_held)
    })
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetKeyState(vk.0 as i32) as u16) & 0x8000 != 0 }
}

#[allow(non_snake_case)]
unsafe fn CallNextHookEx(lparam: LPARAM, code: i32, wparam: WPARAM) -> LRESULT {
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::CallNextHookEx(
            None,
            code,
            wparam,
            lparam,
        )
    }
}

/// Decode a `WM_APP_KEY` lparam/wparam pair back into a KeyEvent.
pub fn decode_message(wparam: usize, lparam: isize) -> KeyEvent {
    let packed = lparam as u64;
    KeyEvent {
        vk: (packed & 0xFFFF_FFFF) as u32,
        pressed: wparam == 1,
        shift: (packed >> 32) & 1 == 1,
        ctrl: (packed >> 33) & 1 == 1,
        mod_held: (packed >> 34) & 1 == 1,
    }
}

/// Decode a `WM_APP_MOUSE` wparam/lparam pair back into a MouseEvent.
/// (Encoding lives in `input::mouse`.)
pub fn decode_mouse_message(wparam: usize, lparam: isize) -> MouseEvent {
    let w = wparam;
    let kind = match w & 0b11 {
        0 => MouseKind::Move,
        1 => MouseKind::WheelV,
        _ => MouseKind::WheelH,
    };
    let packed = lparam as u64;
    MouseEvent {
        kind,
        x: (packed >> 32) as u32 as i32,
        y: packed as u32 as i32,
        notches: ((w >> 16) as u8) as i8 as i32,
        shift: w & 0b0100 != 0,
        ctrl: w & 0b1000 != 0,
        mod_held: w & 0b1_0000 != 0,
    }
}

/// Build the combo string for a key event, e.g. "mod+shift+h".
pub fn combo_of(ev: &KeyEvent) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if ev.mod_held {
        parts.push("mod");
    }
    if ev.ctrl {
        parts.push("ctrl");
    }
    if ev.shift {
        parts.push("shift");
    }
    let key = KeyEvent::key_name(ev.vk).to_lowercase();
    parts.push(&key);
    parts.join("+")
}

/// Look up the first action bound to a key event's combo.
pub fn action_for(binds: &[Bind], ev: &KeyEvent) -> Option<Action> {
    let combo = combo_of(ev);
    binds
        .iter()
        .find(|b| b.combo.to_lowercase() == combo)
        .map(|b| b.action.clone())
        .filter(|a| !matches!(a, Action::Unknown(_)))
}

/// Map a virtual-key code with the extended flag to the niri-ish name
/// (used by the hook to normalize RWin etc.). Kept for completeness.
#[allow(dead_code)]
fn extended_vk(kb: &KBDLLHOOKSTRUCT) -> u32 {
    const LLKHF_EXTENDED: u32 = 0x00000001;
    if kb.flags.0 & LLKHF_EXTENDED != 0 {
        unsafe {
            let mapped = MapVirtualKeyW(kb.scanCode, MAPVK_VSC_TO_VK_EX);
            if mapped != 0 {
                return mapped;
            }
        }
    }
    kb.vkCode
}

#[allow(dead_code)]
fn bool_true(_: BOOL) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(vk: u32, pressed: bool, shift: bool, ctrl: bool, m: bool) -> KeyEvent {
        KeyEvent {
            vk,
            pressed,
            shift,
            ctrl,
            mod_held: m,
        }
    }

    #[test]
    fn key_names() {
        assert_eq!(KeyEvent::key_name(0x48), "H");
        assert_eq!(KeyEvent::key_name(0x25), "Left");
        assert_eq!(KeyEvent::key_name(0x70), "F1");
        assert_eq!(KeyEvent::key_name(0x20), "space");
        assert_eq!(KeyEvent::key_name(0x31), "1");
        // OEM punctuation must map to names the config can bind —
        // the default Mod+Shift+Slash (toggle-overview) depends on it.
        assert_eq!(KeyEvent::key_name(0xBF), "Slash");
        assert_eq!(KeyEvent::key_name(0xBD), "Minus");
        assert_eq!(KeyEvent::key_name(0xBB), "Equal");
        // ... and the resulting combos must hit the default binds.
        let binds = crate::config::Config::default().binds;
        let slash = ev(0xBF, true, true, false, true);
        assert_eq!(
            action_for(&binds, &slash),
            Some(crate::config::Action::ToggleOverview)
        );
    }

    #[test]
    fn combo_building() {
        assert_eq!(combo_of(&ev(0x48, true, false, false, true)), "mod+h");
        assert_eq!(
            combo_of(&ev(0x25, true, false, true, true)),
            "mod+ctrl+left"
        );
        assert_eq!(combo_of(&ev(0x51, true, false, false, false)), "q");
    }

    #[test]
    fn action_lookup() {
        let binds = crate::config::Config::default().binds;
        // Mod+H -> focus-column-left.
        let a = action_for(&binds, &ev(0x48, true, false, false, true));
        assert_eq!(a, Some(Action::FocusColumnLeft));
        // Plain H (no mod) is not bound.
        assert_eq!(action_for(&binds, &ev(0x48, true, false, false, false)), None);
        // Mod+Q closes.
        assert_eq!(
            action_for(&binds, &ev(0x51, true, false, false, true)),
            Some(Action::CloseWindow)
        );
    }

    #[test]
    fn message_roundtrip() {
        let e = ev(0x25, true, true, false, true);
        let packed = (e.vk as u64)
            | ((e.shift as u64) << 32)
            | ((e.ctrl as u64) << 33)
            | ((e.mod_held as u64) << 34);
        let d = decode_message(1, packed as isize);
        assert_eq!(d, e);
        let d2 = decode_message(0, packed as isize);
        assert!(!d2.pressed);
    }
}
