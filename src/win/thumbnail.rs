//! Overview host window: an opaque backdrop covering one monitor's work
//! area, carrying DWM live thumbnails of the (unmoved) real windows.
//!
//! The real windows stay exactly at their settled tile geometry. The
//! host sits on top of them (topmost, opaque) and renders:
//!   - an opaque dark backdrop (the window class brush), so the space
//!     between window thumbnails is a clean dimmed surface — niri dims
//!     the overview background the same way;
//!   - one DWM thumbnail per participant window, animated to the scaled
//!     overview rects.
//!
//! Because the host is opaque and above every source window, no real
//! window can ever leak through — this replaces the previous approach
//! of parking windows off-screen behind a colorkey-transparent backdrop,
//! which left a full-size duplicate visible whenever Windows denied our
//! z-order raise. Not moving the windows also avoids apps' minimum-size
//! clamping, per-frame content reflow, and geometry churn racing the
//! layout.
//!
//! - Host: `WS_POPUP` + `WS_EX_LAYERED` (LWA_ALPHA 255 = fully opaque),
//!   `WS_EX_TOPMOST` so it stays above the source windows, plus
//!   `WS_EX_NOACTIVATE` + `WS_EX_TOOLWINDOW` so clicking it never steals
//!   keyboard focus. It covers the work area only, leaving the native
//!   taskbar/sidebar visible.
//! - Thumbnails: `DwmRegisterThumbnail` per participant, destination
//!   rects updated every animation frame (DWM composites them; no
//!   per-frame app work).
//! - Input: button presses inside the host area are intercepted in the
//!   low-level mouse hook (see `input::mouse::set_overview_regions`),
//!   which hit-tests them against the current thumbnail rects.
//! - Z order: the host is topmost while open. `raise_bars` runs after
//!   each host raise so the desktop bars stay above it, and the focus
//!   ring (also topmost) is raised last.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
        DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION,
        DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE, DwmRegisterThumbnail,
        DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowW, GW_HWNDPREV, GWL_EXSTYLE,
        GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowThreadProcessId, HWND_TOPMOST,
        LWA_ALPHA, RegisterClassW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOOWNERZORDER, SWP_NOSIZE, SetLayeredWindowAttributes, SetWindowPos, ShowWindow,
        WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

use crate::layout::geometry::TileRect;

/// Opaque dark backdrop color (COLORREF 0x00BBGGRR): a dimmed surface
/// shown between the window thumbnails, matching niri's overview.
const BACKDROP: u32 = 0x0018_1818;

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
        /// Host client size (w, h) for the desktop backdrop thumbnail.
        size: (i32, i32),
        /// DWM thumbnail of the desktop shell (Progman): wallpaper + icons,
        /// NOT the app windows (they aren't Progman's content). Drawn to
        /// fill the host BEHIND the window thumbnails so the overview
        /// background is the real wallpaper — closing then dissolves
        /// wallpaper into wallpaper with no dark-backdrop pop. `None` if
        /// Progman can't be thumbnailed (falls back to the dark brush).
        desktop_thumb: Option<isize>,
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
                                // Opaque dark backdrop painted by the class brush.
                                hbrBackground: backdrop_brush(),
                                ..Default::default()
                        };
                        let _ = RegisterClassW(&wc);

                        // This host is opaque: transparent colorkey pixels let
                        // a source HWND show through when Windows denies a z-order
                        // raise, which appeared as a duplicate full-size window.
                        let hwnd = CreateWindowExW(
                                WINDOW_EX_STYLE(
                                        WS_EX_NOACTIVATE.0
                                                | WS_EX_TOOLWINDOW.0
                                                | WS_EX_LAYERED.0
                                                | WS_EX_TOPMOST.0,
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
                                windows::Win32::Foundation::COLORREF(0),
                                255,
                                LWA_ALPHA,
                        );
                        // Register the desktop-shell (Progman) thumbnail FIRST so
                        // it composites BEHIND the window thumbnails registered
                        // later: the overview background becomes the real
                        // wallpaper (+ icons), so closing dissolves wallpaper into
                        // wallpaper without the dark backdrop popping to wallpaper.
                        let desktop_thumb = FindWindowW(w!("Progman"), None)
                                .ok()
                                .filter(|h| !h.0.is_null())
                                .and_then(|progman| DwmRegisterThumbnail(hwnd, progman).ok());
                        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        let host = OverviewHost {
                                hwnd,
                                thumbs: Vec::new(),
                                origin: (l, t),
                                size: (r - l, b - t),
                                desktop_thumb,
                        };
                        host.update_desktop_rect();
                        Some(host)
                }
        }

        /// Fill the whole host with the desktop backdrop thumbnail
        /// (wallpaper and icons). Called once at creation; the source is the
        /// desktop shell, so it never shows the app windows.
        fn update_desktop_rect(&self) {
                let Some(thumb) = self.desktop_thumb else {
                        return;
                };
                let props = DWM_THUMBNAIL_PROPERTIES {
                        dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_OPACITY | DWM_TNP_VISIBLE,
                        rcDestination: RECT {
                                left: 0,
                                top: 0,
                                right: self.size.0,
                                bottom: self.size.1,
                        },
                        opacity: 255,
                        fVisible: windows::core::BOOL(1),
                        fSourceClientAreaOnly: windows::core::BOOL(0),
                        ..Default::default()
                };
                // Safety: handle owned by us.
                unsafe {
                        let _ = DwmUpdateThumbnailProperties(thumb, &props);
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
                                Some(HWND_TOPMOST),
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
                                        Some(HWND_TOPMOST),
                                        0,
                                        0,
                                        0,
                                        0,
                                        SWP_NOMOVE
                                                | SWP_NOSIZE
                                                | SWP_NOACTIVATE
                                                | SWP_NOOWNERZORDER,
                                );
                                let _ = AttachThreadInput(me, fg_thread, false);
                        }
                }
        }

        /// Reconcile the thumbnail set with `sources`: register missing
        /// (alive) windows, drop dead/deregistered ones. Call whenever
        /// the participant set may have changed (every reflow).
        pub fn sync_sources(
                &mut self,
                sources: &[isize],
        ) {
                self.thumbs.retain(|(id, thumb)| {
                        if sources.contains(id) {
                                true
                        } else {
                                // Safety: thumbnail handle is valid (we own it).
                                unsafe {
                                        let _ = DwmUnregisterThumbnail(*thumb);
                                }
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
                                Err(e) => log::warn!(
                                        "overview: thumbnail for window {id} failed: {e}"
                                ),
                        }
                }
        }

        /// Push one frame of destination rects (screen coords; converted
        /// to the host's client space here). The rects already carry the
        /// zoom-scaled layout gaps, so the close handover to the real
        /// windows at zoom 1 is pixel-exact (no separate inset motion,
        /// which used to fight the zoom and looked janky).
        pub fn update_rects(
                &mut self,
                rects: &[TileRect],
        ) {
                for r in rects {
                        let Some((_, thumb)) = self.thumbs.iter().find(|(id, _)| *id == r.id)
                        else {
                                continue;
                        };
                        let props = DWM_THUMBNAIL_PROPERTIES {
                                dwFlags: DWM_TNP_RECTDESTINATION
                                        | DWM_TNP_OPACITY
                                        | DWM_TNP_VISIBLE
                                        | DWM_TNP_SOURCECLIENTAREAONLY,
                                rcDestination: RECT {
                                        left: r.x - self.origin.0,
                                        top: r.y - self.origin.1,
                                        right: r.x - self.origin.0 + r.w,
                                        bottom: r.y - self.origin.1 + r.h,
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
                        unsafe {
                                let _ = DwmUnregisterThumbnail(thumb);
                        }
                }
                if let Some(thumb) = self.desktop_thumb.take() {
                        unsafe {
                                let _ = DwmUnregisterThumbnail(thumb);
                        }
                }
                // Safety: window created by this thread.
                unsafe {
                        let _ = DestroyWindow(self.hwnd);
                }
        }
}

/// Backdrop brush for the window class (leaked: one per process).
/// Paints the opaque dark overview backdrop.
fn backdrop_brush() -> HBRUSH {
        unsafe { CreateSolidBrush(windows::Win32::Foundation::COLORREF(BACKDROP)) }
}

extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
