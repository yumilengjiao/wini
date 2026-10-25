//! Window discovery is retryable: SHOW can precede the title/frame becoming
//! ready (browsers, Electron and UWP startup). Events give low latency; a cheap
//! periodic HWND scan recovers missed/early events without resnapshotting or
//! retiling already managed/manager-hidden windows.

use super::AppState;
use crate::win::{monitor, window};
use windows::Win32::Foundation::HWND;

impl AppState {
        pub(super) fn try_adopt_window(
                &mut self,
                hwnd: HWND,
        ) -> bool {
                if self.suspended
                        || self.interacting_window.is_some()
                        || self.windows.contains(hwnd)
                        || self.hidden_by_us.contains(&(hwnd.0 as isize))
                {
                        return false;
                }
                if let Some(reason) = window::rejection_reason(hwnd) {
                        log::trace!("window {hwnd:?} not ready/eligible: {reason}");
                        return false;
                }
                let info = window::snapshot(hwnd);
                let device =
                        monitor::monitor_of_window(hwnd, &self.monitors).map(|m| m.device.clone());
                let previous = device
                        .as_deref()
                        .and_then(|d| self.layout.monitor(d))
                        .map(|ml| (ml.active_workspace_idx, ml.active_workspace().focused_id()));
                let foreground =
                        unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }
                                == hwnd;
                let covered = device
                        .as_ref()
                        .is_some_and(|d| self.overviews.contains_key(d));
                log::info!(
                        "window adopted: {} [{}] \"{}\" ({})",
                        info.id(),
                        info.exe,
                        info.title,
                        info.class
                );
                self.original_rects.insert(info.id(), info.rect);
                self.windows.insert(info.clone());
                self.layout_add_window(&info);
                // Background discovery (including overview sources) is not an
                // instruction to replace the user's current workspace/tab.
                if (!foreground || covered)
                        && let Some(device) = device.as_deref()
                        && let Some((workspace, previous_focus)) = previous
                        && let Some(ml) = self.layout.monitor_mut(device)
                {
                        ml.active_workspace_idx = workspace;
                        if let Some(id) = previous_focus {
                                ml.active_workspace_mut().focus_window(id);
                        } else if covered
                                && self.focused.is_none()
                                && self.focused_device.as_deref() == Some(device)
                        {
                                self.focused = ml
                                        .active_workspace()
                                        .focused_id()
                                        .map(|id| HWND(id as *mut _));
                        }
                } else if foreground && !covered {
                        self.focused = Some(hwnd);
                        self.focused_device = device;
                        self.update_focus_view(info.id());
                }
                true
        }

        /// Called once a second, not on the 60Hz animation path. Snapshot and
        /// min-track-size queries run only for newly eligible HWNDs.
        pub(super) fn reconcile_new_windows(&mut self) {
                if self.suspended || self.interacting_window.is_some() {
                        return;
                }
                let mut changed = false;
                for hwnd in window::enumerate_top_level() {
                        changed |= self.try_adopt_window(hwnd);
                }
                if changed {
                        self.reflow();
                }
        }
}
