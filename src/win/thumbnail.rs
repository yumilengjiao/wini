//! Overview host window: a transparent backdrop covering one monitor,
//! carrying DWM live thumbnails of the (unmoved) real windows.
//!
//! The real windows keep their settled tile sizes and park one
//! virtual-screen width to the right (off every monitor) while the
//! overview is open — the transparent backdrop shows the bare
//! desktop between the scaled DWM thumbnails of them. (Cross-process
//! DWMWA_CLOAK is access-denied and SW_HIDE blanks the thumbnails;
//! parking off-screen is the only approach that keeps live
//! thumbnails AND clears the real windows from the desktop.) This
//! avoids the three fatal problems of resizing real windows: apps
//! have minimum sizes (overlap), apps reflow their content at every
//! intermediate size (flicker), and real-window geometry churn races
//! the layout.
//!
//! - Backdrop: a `WS_POPUP` + `WS_EX_LAYERED` window painted with a
//!   transparency color key (`SetLayeredWindowAttributes`,
//!   `LWA_COLORKEY`) so the desktop wallpaper shows through; only the
//!   DWM thumbnails (opaque window content) are visible.
//!   `WS_EX_NOACTIVATE` + `WS_EX_TOOLWINDOW` so clicking it never
//!   steals keyboard focus from the focused window behind it.
//! - Thumbnails: `DwmRegisterThumbnail` per participant, destination
//!   rects updated every animation frame (DWM composites them; no
//!   per-frame app work).
//! - Input: the colorkey window is fully transparent to hit-testing,
//!   so button presses are intercepted one level lower, in the
//!   low-level mouse hook (see `input::mouse::set_overview_regions`),
//!   which hit-tests them against the current thumbnail rects.
//! - Z order: the host lives in the normal band, re-raised above the
//!   real windows (which raise themselves on focus changes) but below
//!   the desktop bars (`raise_bars` runs after every host raise) and
//!   below our topmost focus ring.

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
    DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
    DWM_THUMBNAIL_PROPERTIES,
};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowThreadProcessId, GWL_EXSTYLE, GW_HWNDPREV, LWA_COLORKEY, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOOWNERZORDER, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::layout::geometry::TileRect;

/// Transparency color key of the backdrop (COLORREF 0x00BBGGRR).
/// Magenta: symmetric in RGB/BGR and essentially never present in
/// real window content — every pixel painted in it composites fully
/// transparent, so the desktop shows through around the thumbnails.
const COLORKEY: u32 = 0x00FF_00FF;

/// One monitor's overview: backdrop window + thumbnail set.
pub struct OverviewHost {
    hwnd: HWND,
    /// Thumbnails by source window id (the DWM thumbnail handle
    /// is an `isize` in windows-rs). Dead/deregistered entries are
    /// pruned by `sync_sources`.
    thumbs: Vec<(isize, isize)>,
    /// Host window origin in screen coords (thumbnail destination
    /// rects are client-relative).
    origin: (i32, i32),
}

impl OverviewHost {
    /// Create the backdrop window covering `full` (left, top, right,
    /// bottom in screen px), shown immediately.
    pub fn new(full: (i32, i32, i32, i32)) -> Option<Self> {
        let class_name = w!("yumi_wini_ov_host");
        let (l, t, r, b) = full;
        unsafe {
            let hinstance = GetModuleHandleW(None).ok()?;
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                // Solid backdrop: the class brush paints it without
                // WM_PAINT flicker.
                hbrBackground: backdrop_brush(),
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            // WS_EX_LAYERED + LWA_COLORKEY: the colorkey-painted
            // client area composites fully transparent (desktop
            // visible), the DWM thumbnails stay opaque. No
            // WS_EX_TOPMOST (the focus ring must stay above).
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(
                    WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_LAYERED.0,
                ),
                class_name,
                w!(""),
                WINDOW_STYLE(WS_POPUP.0),
                l,
                t,
                r - l,
                b - t,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
            .ok()?;
            let _ = SetLayeredWindowAttributes(
                hwnd,
                windows::Win32::Foundation::COLORREF(COLORKEY),
                0,
                LWA_COLORKEY,
            );
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Some(OverviewHost {
                hwnd,
                thumbs: Vec::new(),
                origin: (l, t),
            })
        }
    }

    pub fn hwnd_addr(&self) -> isize {
        self.hwnd.0 as isize
    }

    /// The window directly ABOVE the backdrop in z order (None when
    /// the backdrop tops its band). Diagnostic: with the backdrop
    /// freshly raised, a MANAGED window here is exactly the
    /// "real window covers the overview" bug — the app logs it.
    pub fn window_above(&self) -> Option<isize> {
        unsafe {
            let above = GetWindow(self.hwnd, GW_HWNDPREV).ok()?;
            (above.0 as isize != 0).then_some(above.0 as isize)
        }
    }

    // (window_above used by the app's raise diagnostic)

    /// Bring the backdrop above the real windows (they raise
    /// themselves on focus changes; the bars are re-raised after this
    /// by the caller, and the ring is topmost).
    ///
    /// Windows' FOREGROUND Z-ORDER PROTECTION silently denies this
    /// raise whenever the foreground window is one of the non-topmost
    /// windows below us — the usual case, since the focused
    /// participant IS the OS foreground: SetWindowPos returns success
    /// but the z order doesn't change (verified live). So the fast
    /// path first checks whether the order is already correct (zero
    /// cost when it is), then verifies the plain raise actually took
    /// effect (only TOPMOST windows may sit above a raised host),
    /// and as a last resort retries attached to the foreground
    /// thread (AttachThreadInput lifts the restriction — the same
    /// technique as `force_set_foreground`).
    pub fn raise(&self) {
        unsafe {
            if z_order_ok(self.hwnd) {
                return;
            }
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
            if z_order_ok(self.hwnd) {
                return;
            }
            let fg = GetForegroundWindow();
            let fg_thread = if fg.0.is_null() {
                0
            } else {
                GetWindowThreadProcessId(fg, None)
            };
            let me = GetCurrentThreadId();
            if fg_thread != 0 && fg_thread != me {
                let _ = AttachThreadInput(me, fg_thread, true);
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOP),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
                let _ = AttachThreadInput(me, fg_thread, false);
            }
        }
    }

    /// Reconcile the thumbnail set with `sources`: register missing
    /// (alive) windows, drop dead/deregistered ones. Call whenever
    /// the participant set may have changed (every reflow).
    pub fn sync_sources(&mut self, sources: &[isize]) {
        self.thumbs
            .retain(|(id, thumb)| {
                if sources.contains(id) {
                    true
                } else {
                    // Safety: thumbnail handle is valid (we own it).
                    unsafe { let _ = DwmUnregisterThumbnail(*thumb); }
                    false
                }
            });
        for &id in sources {
            if self.thumbs.iter().any(|(i, _)| *i == id) {
                continue;
            }
            let hwnd = HWND(id as *mut core::ffi::c_void);
            if !crate::win::api::is_alive(hwnd) {
                continue;
            }
            // Safety: both HWNDs are valid; the host belongs to this
            // thread.
            match unsafe { DwmRegisterThumbnail(self.hwnd, hwnd) } {
                Ok(thumb) => self.thumbs.push((id, thumb)),
                Err(e) => log::warn!("overview: thumbnail for window {id} failed: {e}"),
            }
        }
    }

    /// Push one frame of destination rects (screen coords; converted
    /// to the host's client space here). Each rect is inset by
    /// `inset` pixels so neighboring thumbnails don't visually touch
    /// — with typical `layout { gaps 1..4 }` the scaled gap is
    /// sub-pixel and the overview would look like one solid mosaic.
    /// The caller converges the inset to 0 as the zoom approaches 1
    /// so the close handover to the real windows is pixel-exact.
    pub fn update_rects(&mut self, rects: &[TileRect], inset: i32) {
        for r in rects {
            let Some((_, thumb)) = self.thumbs.iter().find(|(id, _)| *id == r.id) else {
                continue;
            };
            let (dx, dy, dw, dh) = if r.w > 2 * inset && r.h > 2 * inset {
                (r.x + inset, r.y + inset, r.w - 2 * inset, r.h - 2 * inset)
            } else {
                (r.x, r.y, r.w, r.h)
            };
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION
                    | DWM_TNP_OPACITY
                    | DWM_TNP_VISIBLE
                    | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT {
                    left: dx - self.origin.0,
                    top: dy - self.origin.1,
                    right: dx - self.origin.0 + dw,
                    bottom: dy - self.origin.1 + dh,
                },
                opacity: 255,
                fVisible: windows::core::BOOL(1),
                fSourceClientAreaOnly: windows::core::BOOL(0),
                ..Default::default()
            };
            // Safety: thumbnail handle is valid (we own it).
            unsafe {
                let _ = DwmUpdateThumbnailProperties(*thumb, &props);
            }
        }
    }
}

/// Is `hwnd` correctly placed — i.e. nothing sits above it that
/// shouldn't? Allowed above: TOPMOST windows (focus ring, tray)
/// and bar-like windows (`WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` —
/// yasb/zebar's recipe; raise_bars legitimately puts them above the
/// backdrop). Anything else above — a managed real window — means
/// the raise was (or would be) denied by the foreground z-order
/// protection, or something raised itself above us since.
unsafe fn z_order_ok(hwnd: HWND) -> bool {
    unsafe {
        let above = match GetWindow(hwnd, GW_HWNDPREV) {
            Ok(h) => h,
            Err(_) => return true, // nothing above
        };
        if above.0.is_null() {
            return true;
        }
        let ex = GetWindowLongPtrW(above, GWL_EXSTYLE) as u32;
        ex & WS_EX_TOPMOST.0 != 0
            || ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0)
                == (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0)
    }
}

impl Drop for OverviewHost {
    fn drop(&mut self) {
        for (_, thumb) in self.thumbs.drain(..) {
            // Safety: handle owned by us, main thread.
            unsafe { let _ = DwmUnregisterThumbnail(thumb); }
        }
        // Safety: window created by this thread.
        unsafe { let _ = DestroyWindow(self.hwnd); }
    }
}

/// Backdrop brush for the window class (leaked: one per process).
/// Paints the transparency color key: with `LWA_COLORKEY` every such
/// pixel composites fully transparent.
fn backdrop_brush() -> HBRUSH {
    unsafe { CreateSolidBrush(windows::Win32::Foundation::COLORREF(COLORKEY)) }
}

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
