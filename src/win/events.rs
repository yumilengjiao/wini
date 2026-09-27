//! WinEvent hooks: live notifications about window lifecycle changes.
//!
//! Windows notifies us through out-of-context WinEvent hooks. Hooks must be
//! installed on a thread that pumps messages (our main thread), and the
//! callbacks arrive during `GetMessage` — so everything here is effectively
//! single-threaded together with the message loop.
//!
//! The events we care about:
//! - `EVENT_OBJECT_DESTROY` — window handle gone (closed)
//! - `EVENT_OBJECT_SHOW` / `EVENT_OBJECT_HIDE` — visibility toggled
//!   (also fired on restore/minimize)
//! - `EVENT_OBJECT_CLOAKED` / `EVENT_OBJECT_UNCLOAKED` — UWP suspending or
//!   moving to another virtual desktop
//! - `EVENT_SYSTEM_MINIMIZESTART` / `EVENT_SYSTEM_MINIMIZEEND`
//! - `EVENT_SYSTEM_FOREGROUND` — global focus changes (user clicked
//!   somewhere, taskbar, etc.)

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
        EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_SHOW,
        EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND,
        EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZESTART,
        OBJID_WINDOW, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

/// A decoded, pre-filtered lifecycle event for one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinEvent {
        /// A window appeared / became visible.
        Shown(HWND),
        /// A window disappeared / lost visibility.
        Hidden(HWND),
        /// The window handle was destroyed.
        Destroyed(HWND),
        /// A UWP window was cloaked (suspended / moved off-desktop).
        Cloaked(HWND),
        /// A previously cloaked window became visible again.
        Uncloaked(HWND),
        /// A window was minimized.
        MinimizeStarted(HWND),
        /// A window was restored from minimization.
        MinimizeEnded(HWND),
        /// Global foreground (focus) switched to this window.
        Foreground(HWND),
        /// The user started dragging or resizing this window.
        MoveSizeStart(HWND),
        /// The user finished dragging / resizing this window.
        MoveSizeEnd(HWND),
}

/// The hooks currently installed, auto-unhooked on drop.
#[derive(Debug, Default)]
pub struct EventHooks {
        handles: Vec<HWINEVENTHOOK>,
}

impl EventHooks {
        /// Install all lifecycle hooks. `handler` is invoked from the message
        /// loop thread whenever a relevant event fires; it must be fast and
        /// must not pump messages itself.
        pub fn install(handler: impl FnMut(WinEvent) + 'static) -> Self {
                // WINEVENTPROC cannot capture state, so park the handler in a
                // thread-local slot. Everything (install, callback, dispatch) runs
                // on the main thread.
                HANDLER.with(|slot| {
                        *slot.borrow_mut() = Some(Box::new(handler));
                });

                let mut hooks = Self::default();
                // (min, max) event ranges we subscribe to. Narrow ranges keep the
                // amount of delivered events low.
                for (min, max) in [
                        (EVENT_OBJECT_DESTROY, EVENT_OBJECT_DESTROY),
                        (EVENT_OBJECT_SHOW, EVENT_OBJECT_HIDE),
                        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
                        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
                        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
                        (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
                ] {
                        unsafe {
                                let handle = SetWinEventHook(
                                        min,
                                        max,
                                        None,
                                        Some(win_event_proc),
                                        0,
                                        0,
                                        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                                );
                                if handle.is_invalid() {
                                        log::error!("SetWinEventHook({min:#x}..{max:#x}) failed");
                                } else {
                                        hooks.handles.push(handle);
                                }
                        }
                }
                log::info!("installed {} winevent hooks", hooks.handles.len());
                hooks
        }
}

impl Drop for EventHooks {
        fn drop(&mut self) {
                for handle in self.handles.drain(..) {
                        unsafe {
                                let _ = UnhookWinEvent(handle);
                        }
                }
                log::info!("winevent hooks removed");
        }
}

type WinEventHandler = Box<dyn FnMut(WinEvent)>;

thread_local! {
    static HANDLER: std::cell::RefCell<Option<WinEventHandler>> =
        const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn win_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        id_object: i32,
        _id_child: i32,
        _id_event_thread: u32,
        _time: u32,
) {
        // Only whole-window events; ignore sub-objects (scrollbars, menus...).
        if id_object != OBJID_WINDOW.0 || hwnd.0.is_null() {
                return;
        }
        let Some(decoded) = decode(event, hwnd) else {
                return;
        };
        // Never let a panic cross the FFI boundary.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                HANDLER.with(|slot| {
                        if let Some(handler) = slot.borrow_mut().as_mut() {
                                handler(decoded);
                        }
                });
        }));
        if result.is_err() {
                log::error!("winevent handler panicked on {decoded:?}");
        }
}

fn decode(
        event: u32,
        hwnd: HWND,
) -> Option<WinEvent> {
        use windows::Win32::UI::WindowsAndMessaging as m;
        let ev = match event {
                m::EVENT_OBJECT_SHOW => WinEvent::Shown(hwnd),
                m::EVENT_OBJECT_HIDE => WinEvent::Hidden(hwnd),
                m::EVENT_OBJECT_DESTROY => WinEvent::Destroyed(hwnd),
                m::EVENT_OBJECT_CLOAKED => WinEvent::Cloaked(hwnd),
                m::EVENT_OBJECT_UNCLOAKED => WinEvent::Uncloaked(hwnd),
                m::EVENT_SYSTEM_MINIMIZESTART => WinEvent::MinimizeStarted(hwnd),
                m::EVENT_SYSTEM_MINIMIZEEND => WinEvent::MinimizeEnded(hwnd),
                m::EVENT_SYSTEM_FOREGROUND => WinEvent::Foreground(hwnd),
                m::EVENT_SYSTEM_MOVESIZESTART => WinEvent::MoveSizeStart(hwnd),
                m::EVENT_SYSTEM_MOVESIZEEND => WinEvent::MoveSizeEnd(hwnd),
                _ => return None,
        };
        Some(ev)
}
