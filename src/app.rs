//! Top-level application shell: owns the Win32 message loop that drives
//! window tracking, input handling and (later) animations.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PostQuitMessage, PostThreadMessageW, TranslateMessage,
        WM_QUIT,
};

use crate::config::{self, Action, Config};
use crate::input::{self, KeyEvent, MouseKind};
use crate::layout::geometry::{self, LayoutParams};
use crate::layout::{DirH, DirV, Edge, Layout, SizeChange};
use crate::transport::Transport;
use crate::win::events::{EventHooks, WinEvent};
use crate::win::monitor::{self, Monitor};
use crate::win::msg_window::{
        ANIM_TIMER_MS, CONFIG_TIMER_MS, MessageWindow, TIMER_ANIM, TIMER_CONFIG,
};
use crate::win::placement;
use crate::win::window::{WindowInfo, WindowRegistry};

/// Fatal, top-level error.
#[derive(Debug)]
pub struct AppError(String);

impl fmt::Display for AppError {
        fn fmt(
                &self,
                f: &mut fmt::Formatter<'_>,
        ) -> fmt::Result {
                write!(f, "{}", self.0)
        }
}

impl std::error::Error for AppError {}

impl From<String> for AppError {
        fn from(s: String) -> Self {
                AppError(s)
        }
}

/// A floating window: freed from the tiling grid, placed freely.
/// Niri remembers the floating position per window while it floats;
/// toggling back re-tiles it.
#[derive(Debug, Clone)]
struct FloatState {
        /// Which monitor / workspace the float belongs to.
        device: String,
        workspace_idx: usize,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
}

/// An in-flight horizontal shift of one workspace's view inside an
/// open overview: when the overview's internal focus moves (niri's
/// hardcoded Left/Right binds) the workspace view scrolls to keep
/// the newly focused column visible. Rebuilding the thumbnail
/// finals at the new view position would make every thumbnail JUMP
/// sideways instantly; instead the finals are rebuilt at the new
/// position and this animated offset (starting at the pre-scroll
/// delta, gliding to 0) is applied on top, reproducing niri's
/// view-offset animation.
#[derive(Debug, Clone)]
struct ViewShift {
        /// The workspace whose view scrolled.
        ws_idx: usize,
        /// Animated x offset in UNSCALED monitor px (scaled by the
        /// current zoom when applied).
        offset: crate::anim::Val,
}

/// An open (or opening/closing) overview on one monitor — niri's
/// `toggle-overview`. Like the slide, everything derives from TWO
/// animated values instead of per-window springs:
/// - `progress`: 0 = closed, 1 = fully open. The zoom is niri's
///   `compute_overview_zoom`: `zoom = 1 - progress * (1 - 0.35)` —
///   at progress 0 the derived rects are EXACTLY the settled tiles,
///   so opening starts pixel-continuous from the current view.
/// - `camera`: the workspace render index on the vertical strip,
///   identical to the slide camera. In the overview it scrolls
///   freely (wheel / focus-workspace) while `active_workspace_idx`
///   stays put; closing retargets it to the workspace we close into.
///
/// The canvas is the FULL monitor scaled by `zoom` and centered
/// (niri's `workspaces_render_geo`: workspace k sits at
/// `dy = (k - camera) * stride`, stride = scaled full height + a 10%
/// gap — `workspace_size_with_gap`).
///
/// These rects are THUMBNAIL rects, not real-window rects: while the
/// overview is open the real windows stay at their settled tile sizes
/// and positions. An opaque host (see `win::thumbnail`) covers the work
/// area on top of them and draws DWM live thumbnails at the scaled
/// rects, with a desktop thumbnail as the backdrop. This sidesteps app
/// minimum sizes (real windows refused to shrink → overlaps), per-frame
/// app reflow (flicker), and the close handover (the host simply drops,
/// revealing the windows exactly where they always were).
#[derive(Debug, Clone)]
struct Overview {
        /// Open/close progress (0 = closed, 1 = fully open).
        progress: crate::anim::Val,
        /// Camera position (workspace index on the strip).
        camera: crate::anim::Val,
        /// Settled (zoom-1) tile rects per participating (non-empty)
        /// workspace, exactly like the slide's finals. Refreshed by
        /// `refresh_overview` whenever the layout changes while open.
        finals: Vec<(usize, Vec<crate::layout::geometry::TileRect>)>,
        /// The monitor's WORK-AREA rect (left, top, w, h) — the overview
        /// canvas. The native taskbar/sidebar stays outside it.
        full: (f64, f64, f64, f64),
        /// Zoom at progress = 1 (niri's overview.zoom, default 0.35).
        zoom_target: f64,
        /// In-flight view scroll of one workspace (see [`ViewShift`]).
        view_shift: Option<ViewShift>,
        /// `Some(from_progress)` when the camera and progress springs
        /// were retargeted together with identical params (overview
        /// close/toggle-close, niri's `activate_workspace_with_anim_config`).
        /// Rendering then applies niri's `workspace_render_idx` correction
        /// so the zoom+slide composite is monotonic.
        sync_from_progress: Option<f64>,
}

impl Overview {
        /// Current zoom. Clamped to `[zoom_target, 1.0]`: a spring can
        /// overshoot its target, and a `progress` below 0 (on close) would
        /// give a zoom > 1, i.e. thumbnails LARGER than the real window —
        /// the visible "windows briefly maximize before shrinking back"
        /// glitch. The upper clamp at 1.0 removes it while keeping the
        /// motion continuous (the spring settles exactly at the bound).
        fn zoom(&self) -> f64 {
                (1.0 - self.progress.value() * (1.0 - self.zoom_target))
                        .clamp(self.zoom_target, 1.0)
        }

        /// Whether the overview is (animating) open or closing.
        fn opening(&self) -> bool {
                self.progress.target() > 0.5
        }

        /// All participant rects at an explicit zoom/camera (zoom 1 with
        /// the camera at the active workspace yields the settled tiles,
        /// which is what the close handover applies).
        fn rects_at(
                &self,
                zoom: f64,
                cam: f64,
        ) -> Vec<crate::layout::geometry::TileRect> {
                let (fx, fy, fw, fh) = self.full;
                let ws_h = fh * zoom;
                let stride = ws_h + fh * 0.1 * zoom;
                // Center the scaled canvas in the monitor (niri's
                // static_offset); at zoom 1 the offsets are 0.
                let off_x = fx + (fw - fw * zoom) / 2.0;
                let off_y = fy + (fh - ws_h) / 2.0;
                let mut out = Vec::new();
                for (k, rects) in &self.finals {
                        let dy = (*k as f64 - cam) * stride;
                        // The in-flight view shift (overview focus moves): the
                        // finals are at the NEW view position, the shift starts
                        // at the pre-scroll delta and glides to 0.
                        let shift = match &self.view_shift {
                                Some(s) if s.ws_idx == *k => s.offset.value() * zoom,
                                _ => 0.0,
                        };
                        for r in rects {
                                out.push(crate::layout::geometry::TileRect {
                                        id: r.id,
                                        x: (off_x + (r.x as f64 - fx) * zoom + shift).round()
                                                as i32,
                                        y: (off_y + dy + (r.y as f64 - fy) * zoom).round() as i32,
                                        w: ((r.w as f64) * zoom).round().max(1.0) as i32,
                                        h: ((r.h as f64) * zoom).round().max(1.0) as i32,
                                });
                        }
                }
                out
        }

        /// Camera position for rendering. While a synchronized close is
        /// in flight, apply niri's `workspace_render_idx` correction:
        ///
        /// ```text
        /// render_idx = to + (cam - to) * (from_zoom / cur_zoom)
        /// ```
        ///
        /// (substituted from niri's first_ws_y derivation; stride scales
        /// linearly with zoom, so the height ratio is the zoom ratio).
        /// Continuous at both ends — cam == to collapses it to `to`, and
        /// cur_zoom glides through the progress spring's exact endpoint —
        /// so it stays valid even when the distance-dependent spring
        /// durations let the camera outlive the zoom.
        fn render_cam(&self) -> f64 {
                let cam = self.camera.value();
                match self.sync_from_progress {
                        Some(from_progress) => {
                                let from_zoom = (1.0 - from_progress * (1.0 - self.zoom_target))
                                        .max(0.0001);
                                let cur_zoom = self.zoom();
                                let to = self.camera.target();
                                to + (cam - to) * (from_zoom / cur_zoom)
                        },
                        None => cam,
                }
        }

        /// Participant rects at the current animated zoom/camera.
        fn current_rects(&self) -> Vec<crate::layout::geometry::TileRect> {
                self.rects_at(self.zoom(), self.render_cam())
        }

        /// Both springs at rest?
        fn settled(&self) -> bool {
                self.progress.finished()
                        && self.camera.finished()
                        && self.view_shift.as_ref().is_none_or(|s| s.offset.finished())
        }
}

/// Current view position (column-space x at the content-area's left
/// edge) of `id`'s workspace on `device` — used to measure how far
/// an overview focus move scrolled the view (see `ViewShift`).
fn view_position(
        layout: &Layout,
        device: &str,
        id: isize,
        params: &LayoutParams,
        view_width: f64,
) -> Option<(usize, f64)> {
        let ml = layout.monitor(device)?;
        let idx = ml.workspace_of(id)?;
        let ws = &ml.workspaces[idx];
        let widths = geometry::column_widths(ws, params, view_width);
        let xs = geometry::column_xs(&widths, params.gaps);
        Some((idx, geometry::view_pos(ws, &xs)))
}

// Rate limiter for the "backdrop covered" diagnostic
// (raise_overview_hosts): one log line per 500 ms.
thread_local! {
    static BACKDROP_WARN_AT: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Mutable state shared between the message loop and event handlers.
/// (The config field is consumed by the input module in the next
/// commits.)
#[allow(dead_code)]
struct AppState {
        /// All top-level application windows we currently track.
        windows: WindowRegistry,
        /// Current monitor topology.
        monitors: Vec<Monitor>,
        /// The structural layout (columns/workspaces) of tracked windows.
        layout: Layout,
        /// Layout tuning parameters (config-driven).
        params: LayoutParams,
        /// Full configuration (binds, mod key).
        config: Config,
        /// Mtime of the config file as last loaded (hot reload polling).
        config_mtime: Option<std::time::SystemTime>,
        /// The window the OS currently considers foreground.
        focused: Option<HWND>,
        /// While the user drags/resizes this window, tiling is paused so we
        /// don't fight the user's mouse.
        interacting_window: Option<HWND>,
        /// Management suspended (do-screen-transition): the screen is
        /// covered, animations are frozen and no geometry is applied
        /// until the transition ends.
        suspended: bool,
        /// Window rect when the current interaction started, used to tell
        /// real drags apart from clicks and accidental nudges.
        interact_start_rect: Option<(f64, f64, f64, f64)>,
        /// Windows we stripped decorations from (windowed fullscreen);
        /// used to restore them on exit/removal.
        borderless: std::collections::HashSet<isize>,
        /// Floating windows (out of the tiling grid).
        floating: std::collections::HashMap<isize, FloatState>,
        /// Windows WE hid via `set_shown(hwnd, false)` (inactive
        /// workspaces, invisible floats). Their `EVENT_OBJECT_HIDE` is
        /// self-inflicted and must not unmanage them — otherwise switching
        /// workspaces would silently drop every window from the layout.
        hidden_by_us: std::collections::HashSet<isize>,
        /// Open (or animating) overviews, keyed by monitor device. See
        /// [`Overview`] for the zoom + camera model.
        overviews: std::collections::HashMap<String, Overview>,
        /// Backdrop + thumbnail host windows, keyed like `overviews`
        /// (kept separate so [`Overview`] stays pure, testable data).
        overview_hosts: std::collections::HashMap<String, crate::win::thumbnail::OverviewHost>,
        /// Each window's geometry as it was when we started managing it;
        /// restored on exit so the desktop is left as we found it.
        original_rects: std::collections::HashMap<isize, windows::Win32::Foundation::RECT>,
        /// Workspace indicator overlay (also relays WM_DISPLAYCHANGE).
        overlay: Option<crate::win::overlay::OverlayWindow>,
        /// Focus ring: outlines the focused window (niri's focus-ring).
        focus_border: Option<crate::win::focus_border::FocusBorder>,
        /// System tray icon with the quick-actions menu (config,
        /// autostart, quit). None if the tray could not be created.
        tray: Option<crate::win::tray::Tray>,
        /// Unified animated transport: the single per-frame geometry
        /// pipeline for tiled windows (scroll + workspace switch + per-tile
        /// move/resize). Replaces the old per-window animator and the
        /// frozen workspace-switch slide.
        transport: crate::transport::Transport,
        /// Windows currently shown by the transport (tiled). Diffed each
        /// frame so `set_shown` only fires on an actual visibility change.
        shown: std::collections::HashSet<isize>,
        /// Whether the transport was animating on the previous tick, so the
        /// frame that lands exactly on settle is still applied once before
        /// we stop pushing geometry at rest.
        was_animating: bool,
}

impl AppState {
        /// React to a decoded WinEvent; keeps the registry and layout in
        /// sync with reality. Geometry application arrives in a later
        /// module; for now the structure is maintained and logged.
        fn handle_event(
                &mut self,
                event: WinEvent,
        ) {
                match event {
                        WinEvent::Shown(hwnd)
                        | WinEvent::Uncloaked(hwnd)
                        | WinEvent::MinimizeEnded(hwnd) => {
                                if !self.windows.contains(hwnd)
                                        && crate::win::window::is_manageable(hwnd)
                                {
                                        let info = crate::win::window::snapshot(hwnd);
                                        log::info!(
                                                "window opened: [{}] \"{}\" ({})",
                                                info.exe,
                                                info.title,
                                                info.class
                                        );
                                        self.windows.insert(info.clone());
                                        self.original_rects.insert(hwnd.0 as isize, info.rect);
                                        self.layout_add_window(&info);
                                        self.reflow();
                                }
                        },
                        WinEvent::Hidden(hwnd)
                        | WinEvent::Cloaked(hwnd)
                        | WinEvent::MinimizeStarted(hwnd) => {
                                let id = hwnd.0 as isize;
                                if self.hidden_by_us.contains(&id) {
                                        // We hid this window ourselves (inactive workspace /
                                        // hidden float). Keep managing it.
                                        log::debug!("ignoring self-inflicted hide of {id}");
                                        return;
                                }
                                if let Some(info) = self.windows.remove(hwnd) {
                                        log::info!("window hidden: \"{}\"", info.title);
                                        let id = hwnd.0 as isize;
                                        let device = self.layout.remove_window(id);
                                        self.transport.remove_window(id);
                                        self.restore_borders(id);
                                        self.floating.remove(&id);
                                        self.original_rects.remove(&id);
                                        if let Some(d) = device {
                                                self.reclaim_workspaces(&d);
                                        }
                                        self.reflow();
                                }
                        },
                        WinEvent::Destroyed(hwnd) => {
                                let dead_id = hwnd.0 as isize;
                                self.hidden_by_us.remove(&dead_id);
                                if let Some(info) = self.windows.remove(hwnd) {
                                        log::info!("window closed: \"{}\"", info.title);
                                        let id = dead_id;
                                        let device = self.layout.remove_window(id);
                                        self.transport.remove_window(id);
                                        self.restore_borders(id);
                                        self.floating.remove(&id);
                                        self.original_rects.remove(&id);
                                        if let Some(d) = device {
                                                self.reclaim_workspaces(&d);
                                        }
                                        self.reflow();
                                }
                        },
                        WinEvent::MoveSizeStart(hwnd) => {
                                if self.windows.contains(hwnd) {
                                        log::debug!(
                                                "user interaction started on window {id}",
                                                id = hwnd.0 as isize
                                        );
                                        self.interacting_window = Some(hwnd);
                                        self.interact_start_rect =
                                                crate::win::api::window_rect(hwnd);
                                        // The ring would lag behind the dragged window;
                                        // hide it until the drop resolves.
                                        self.update_focus_border();
                                }
                        },
                        WinEvent::MoveSizeEnd(hwnd) => {
                                if self.interacting_window == Some(hwnd) {
                                        log::debug!(
                                                "user interaction ended on window {id}",
                                                id = hwnd.0 as isize
                                        );
                                        self.interacting_window = None;
                                        let id = hwnd.0 as isize;
                                        if let Some(fs) = self.floating.get_mut(&id) {
                                                // The user moved/resized a floating window: adopt
                                                // the new rect instead of snapping it back.
                                                if let Some((x, y, w, h)) =
                                                        crate::win::api::window_rect(hwnd)
                                                {
                                                        fs.x = x;
                                                        fs.y = y;
                                                        fs.w = w;
                                                        fs.h = h;
                                                }
                                        } else {
                                                // Tiled window: interpret the drag as a niri-style
                                                // drag-and-drop reorder (or a cross-monitor move).
                                                self.handle_tiled_drag_end(hwnd);
                                        }
                                }
                        },
                        WinEvent::Foreground(hwnd) => {
                                log::debug!("foreground event -> {hwnd:?}");
                                if self.focused != Some(hwnd) {
                                        self.focused = Some(hwnd);
                                        let id = hwnd.0 as isize;
                                        if self.layout.focus_window(id) {
                                                self.update_focus_view(id);
                                                self.reflow();
                                        } else {
                                                // Foreground moved to a window we don't track:
                                                // the ring must not stay on the old one.
                                                self.update_focus_border();
                                        }
                                }
                                // The newly-foreground window raises itself while
                                // processing WM_ACTIVATE (after our sync raises):
                                // re-raise the overview backdrops (they must cover
                                // the real windows) and the bars (yasb/zebar) on top
                                // of everything (niri: layer-shell top).
                                self.raise_overview_hosts();
                                placement::raise_bars(&self.monitors);
                        },
                }
        }

        /// Place a newly tracked window into the layout, applying any
        /// matching window-rule (open-floating / open-on-workspace /
        /// open-maximized / open-fullscreen). Falls back to the active
        /// workspace of the monitor the window currently lives on.
        fn layout_add_window(
                &mut self,
                info: &WindowInfo,
        ) {
                let id = info.id();
                let hwnd = info.hwnd;
                let device = monitor::monitor_of_window(hwnd, &self.monitors)
                        .map(|m| m.device.clone())
                        .unwrap_or_default();
                let rule = self
                        .config
                        .match_rule(&info.exe, &info.title, &info.class)
                        .cloned();

                // open-floating: keep the window at its current position,
                // outside the tiling grid.
                if rule.as_ref().is_some_and(|r| r.open_floating)
                        && let Some((x, y, w, h)) = crate::win::api::window_rect(hwnd)
                {
                        let ws_idx = self
                                .layout
                                .monitor(&device)
                                .map(|m| m.active_workspace_idx)
                                .unwrap_or(0);
                        self.floating.insert(
                                id,
                                FloatState {
                                        device,
                                        workspace_idx: ws_idx,
                                        x,
                                        y,
                                        w,
                                        h,
                                },
                        );
                        log::debug!("window-rule: {id} opens floating");
                        return;
                }

                match rule.as_ref().and_then(|r| r.open_workspace) {
                        Some(n) => {
                                let idx = n.saturating_sub(1) as usize;
                                if let Some(ml) = self.layout.monitor_mut(&device) {
                                        ml.add_window_to_workspace(id, idx);
                                } else {
                                        self.layout.add_window(&device, id);
                                }
                                log::debug!("window-rule: {id} opens on workspace {}", idx + 1);
                        },
                        None => self.layout.add_window(&device, id),
                }

                // open-maximized / open-fullscreen: flag the new column/window.
                if let Some(r) = rule
                        && let Some(ml) = self.layout.monitor_mut(&device)
                        && let Some((ci, _)) = ml.active_workspace().find(id)
                {
                        if r.open_maximized {
                                let col = &mut ml.active_workspace_mut().columns[ci];
                                col.is_maximized = true;
                                col.is_full_width = true;
                        }
                        if r.open_fullscreen {
                                ml.active_workspace_mut().fullscreen_id = Some(id);
                        }
                }
                // Honor the window's enforced minimum size: apps like Windows
                // Terminal clamp SetWindowPos to their minimum track size, so
                // tiles narrower than that would visually overlap neighbors.
                let (min_w, min_h) = info.min_size;
                if min_w > 0.0 || min_h > 0.0 {
                        for ml in self.layout.monitors.iter_mut() {
                                for ws in ml.workspaces.iter_mut() {
                                        if ws.set_min_size(id, min_w, min_h) {
                                                log::debug!(
                                                        "window {id} enforces min size {min_w}x{min_h}"
                                                );
                                        }
                                }
                        }
                }
                log::debug!(
                        "layout: window {id} -> monitor {device}, column {}",
                        self.layout
                                .monitor(&device)
                                .map(|m| m.active_workspace().columns.len())
                                .unwrap_or(0)
                );
        }

        /// Scroll the workspace owning `id` so the newly focused column is
        /// visible (instantly for now; animations come later).
        fn update_focus_view(
                &mut self,
                id: crate::layout::WindowId,
        ) {
                let params = self.params.clone();
                for m in &mut self.layout.monitors {
                        if let Some(ws_idx) = m.workspace_of(id) {
                                let Some(mon) =
                                        self.monitors.iter().find(|mon| mon.device == m.device)
                                else {
                                        continue;
                                };
                                let view_width = (mon.work.right - mon.work.left) as f64;
                                let ws = &mut m.workspaces[ws_idx];
                                geometry::refresh_view_offset(ws, &params, view_width, None);
                                return;
                        }
                }
        }

        /// Undo everything we did to real windows: restore decorations,
        /// original geometry and visibility. Called on the way out so the
        /// desktop is left as we found it.
        fn restore_all(&mut self) {
                let ids: Vec<isize> = self.original_rects.keys().copied().collect();
                log::info!("exiting: restoring geometry of {} window(s)", ids.len());
                for id in ids {
                        let hwnd = HWND(id as *mut _);
                        if !crate::win::api::is_alive(hwnd) {
                                self.original_rects.remove(&id);
                                continue;
                        }
                        self.restore_borders(id);
                        if let Some(rc) = self.original_rects.get(&id) {
                                unsafe {
                                        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                        hwnd,
                        None,
                        rc.left,
                        rc.top,
                        rc.right - rc.left,
                        rc.bottom - rc.top,
                        windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                            | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
                    );
                                }
                        }
                        // Windows on hidden workspaces must come back.
                        placement::set_shown(hwnd, true);
                }
        }

        /// Close the overview on every monitor, back to the currently
        /// active workspace (niri's Esc / Return in overview). Returns
        /// true if any overview was open (the caller consumes the key).
        fn exit_overviews(&mut self) -> bool {
                let devices: Vec<String> = self.overviews.keys().cloned().collect();
                if devices.is_empty() {
                        return false;
                }
                for device in devices {
                        self.close_overview_to(&device, None);
                }
                true
        }

        /// Open the overview on one monitor (niri's toggle-overview,
        /// per-monitor): every non-empty workspace participates, all its
        /// windows become visible at once and the zoom animates 1 ->
        /// zoom around the active workspace. The camera starts parked on
        /// the active workspace, so the first frame equals the current
        /// view.
        ///
        /// The real windows never move: they stay at their settled tiles
        /// behind a freshly created backdrop host (see `win::thumbnail`),
        /// and DWM live thumbnails of them are drawn at the animated
        /// rects. Participants on inactive workspaces are shown (hidden
        /// windows have blank thumbnails) — invisible behind the
        /// backdrop — and hidden again on close.
        fn open_overview(
                &mut self,
                device: &str,
        ) {
                if self.interacting_window.is_some() || self.suspended {
                        return;
                }
                let Some(mon) = self.monitors.iter().find(|m| m.device == device) else {
                        return;
                };
                let Some(ml) = self.layout.monitor(device) else {
                        return;
                };

                // The transport's workspace-switch camera may still be
                // animating; the overview reads settled geometry, so nothing
                // special is needed — opening simply parks the real windows and
                // draws thumbnails at the current (settled) tiles.

                let params = self.params.clone();
                // Use the Windows work area as the overview canvas. The
                // native taskbar/sidebar remains outside the host and is never
                // covered by the overview animation.
                let full = (
                        mon.work.left as f64,
                        mon.work.top as f64,
                        (mon.work.right - mon.work.left) as f64,
                        (mon.work.bottom - mon.work.top) as f64,
                );
                let area = (
                        mon.work.left as f64,
                        mon.work.top as f64,
                        (mon.work.right - mon.work.left) as f64,
                        (mon.work.bottom - mon.work.top) as f64,
                );
                let mut finals = Vec::new();
                for (k, ws) in ml.workspaces.iter().enumerate() {
                        if ws.is_empty() {
                                continue;
                        }
                        finals.push((k, geometry::compute_workspace_geometry(ws, &params, area)));
                }

                let anim = self.config.animations.overview_open_close_params();
                let mut progress = crate::anim::Val::to(0.0, anim);
                progress.retarget(1.0, anim);
                let camera = crate::anim::Val::to(ml.active_workspace_idx as f64, anim);

                // Niri clamps the configured zoom to a sane range.
                let zoom_target = self.config.overview.zoom.clamp(0.0001, 0.75);
                let ov = Overview {
                        progress,
                        camera,
                        finals,
                        full,
                        zoom_target,
                        view_shift: None,
                        sync_from_progress: None,
                };
                // Floating windows would sit full-size over the transparent
                // backdrop. Hide them BEFORE creating the host: OverviewHost
                // is shown by its constructor, and a colorkey backdrop cannot
                // conceal a float while it is being hidden.
                let float_ids: Vec<isize> = self
                        .floating
                        .iter()
                        .filter(|(_, fs)| fs.device == device)
                        .map(|(id, _)| *id)
                        .collect();
                for &id in &float_ids {
                        let hwnd = HWND(id as *mut _);
                        // Register BEFORE hiding (hook race).
                        self.hidden_by_us.insert(id);
                        if crate::win::api::is_alive(hwnd) {
                                placement::set_shown(hwnd, false);
                        }
                        self.transport.remove_window(id);
                }

                // The opaque host covers only the work area, leaving the
                // native taskbar/sidebar visible. Source HWNDs stay at their
                // settled geometry and cannot leak through the host.
                let Some(mut host) = crate::win::thumbnail::OverviewHost::new((
                        mon.work.left,
                        mon.work.top,
                        mon.work.right,
                        mon.work.bottom,
                )) else {
                        // The float hide above is only an implementation
                        // detail of a successful overview. Undo it if the
                        // host cannot be created.
                        for id in &float_ids {
                                self.hidden_by_us.remove(id);
                                let hwnd = HWND(*id as *mut _);
                                if crate::win::api::is_alive(hwnd) {
                                        placement::set_shown(hwnd, true);
                                }
                        }
                        log::warn!("overview[{device}]: no backdrop host; not opening");
                        return;
                };
                for (_, rs) in &ov.finals {
                        for r in rs {
                                let hwnd = HWND(r.id as *mut _);
                                self.hidden_by_us.remove(&r.id);
                                if crate::win::api::is_alive(hwnd) {
                                        placement::set_shown(hwnd, true);
                                }
                        }
                }
                let sources: Vec<isize> = ov
                        .finals
                        .iter()
                        .flat_map(|(_, rs)| rs.iter().map(|r| r.id))
                        .collect();
                host.sync_sources(&sources);
                host.update_rects(&ov.current_rects());
                // The host is opaque and topmost from the first frame, which
                // covers the real windows completely (no see-through
                // duplicate). At the opening frame the zoom is ~1 so the
                // thumbnails coincide with the real tiles — the dark backdrop
                // only appears in the gaps and grows smoothly as the zoom
                // pulls back, so there is no pop.
                host.raise();
                // Source windows remain at their settled geometry. Drop any
                // stale layout animation; otherwise an old resize could still
                // land while the host is opening and make the first frames
                // look like a maximize-then-shrink transition.
                for r in ov.finals.iter().flat_map(|(_, rs)| rs) {
                        self.transport.remove_window(r.id);
                }
                // All participants can now be visible: the opaque host hides
                // the real HWNDs while the DWM thumbnails are animated.
                for (_, rs) in &ov.finals {
                        for r in rs {
                                let hwnd = HWND(r.id as *mut _);
                                self.hidden_by_us.remove(&r.id);
                                if crate::win::api::is_alive(hwnd) {
                                        placement::set_shown(hwnd, true);
                                }
                        }
                }
                // A just-shown source may need one property refresh before DWM
                // paints its first live thumbnail.
                host.update_rects(&ov.current_rects());
                placement::raise_bars(&self.monitors);
                log::debug!(
                        "overview[{device}]: opening ({} workspaces)",
                        ov.finals.len()
                );
                self.overviews.insert(device.to_string(), ov);
                self.overview_hosts.insert(device.to_string(), host);
                self.sync_overview_regions();
                self.update_focus_border();
        }

        /// Toggle the overview on one monitor: open when closed, close
        /// when open (toggling twice mid-animation reverses smoothly — the
        /// progress spring carries its velocity).
        fn toggle_overview(
                &mut self,
                device: &str,
        ) {
                let Some(ov) = self.overviews.get_mut(device) else {
                        self.open_overview(device);
                        return;
                };
                let anim = self.config.animations.overview_open_close_params();
                if ov.opening() {
                        ov.progress.retarget(0.0, anim);
                        // Snap any in-flight thumbnail view-shift so the close
                        // starts from the settled view (otherwise the shift
                        // finishes DURING the close, which looks like the
                        // windows sliding sideways as the overview zooms in).
                        ov.view_shift = None;
                        // Close back into the active workspace. The camera must
                        // run with the SAME params as the zoom (niri passes the
                        // overview open/close config to activate_workspace) so
                        // both springs start and end together — otherwise the
                        // opaque backdrop lingers waiting for a longer camera
                        // slide after the zoom already finished.
                        if let Some(active) = self
                                .layout
                                .monitor(device)
                                .map(|ml| ml.active_workspace_idx)
                        {
                                ov.camera.retarget(active as f64, anim);
                                ov.sync_from_progress = Some(ov.progress.from());
                        }
                } else {
                        // Reopening mid-close: keep the camera wherever it is; the
                        // zoom/camera pair is no longer synchronized.
                        ov.progress.retarget(1.0, anim);
                        ov.sync_from_progress = None;
                }
        }

        /// Close the overview, sliding the camera to `ws_idx` (None = the
        /// active workspace). The zoom-in and the camera slide run with
        /// identical animation params (niri passes the overview open/close
        /// config to activate_workspace_with_anim_config) so both springs
        /// start and end together; the backdrop's `settled()` then fires
        /// exactly when the motion stops instead of dead-waiting on a
        /// longer camera slide.
        fn close_overview_to(
                &mut self,
                device: &str,
                ws_idx: Option<usize>,
        ) {
                let Some(ov) = self.overviews.get_mut(device) else {
                        return;
                };
                let anim = self.config.animations.overview_open_close_params();
                let target = ws_idx
                        .or_else(|| {
                                self.layout
                                        .monitor(device)
                                        .map(|ml| ml.active_workspace_idx)
                        })
                        .map(|i| i as f64)
                        .unwrap_or_else(|| ov.camera.target());
                ov.progress.retarget(0.0, anim);
                ov.camera.retarget(target, anim);
                ov.view_shift = None;
                ov.sync_from_progress = Some(ov.progress.from());
        }

        /// Scroll the overview camera one workspace up/down (niri's bare
        /// wheel and focus-workspace-up/down in overview). Clamped to the
        /// workspaces that exist.
        fn overview_scroll(
                &mut self,
                device: &str,
                down: bool,
        ) {
                let Some(ov) = self.overviews.get_mut(device) else {
                        return;
                };
                let Some(ml) = self.layout.monitor(device) else {
                        return;
                };
                let max = ml.workspaces.len().saturating_sub(1) as f64;
                let cur = ov.camera.target();
                let to = if down {
                        (cur + 1.0).min(max)
                } else {
                        (cur - 1.0).max(0.0)
                };
                if (to - cur).abs() > f64::EPSILON {
                        let switch = self.config.animations.workspace_switch_params();
                        ov.camera.retarget(to, switch);
                        ov.sync_from_progress = None;
                }
        }

        /// Focus a workspace while the overview is open: activate it (so
        /// focus follows and the overview closes into it), scroll the
        /// camera to center it, and move `self.focused` to that workspace's
        /// focused window. Clamped to the workspaces that exist.
        fn overview_activate(
                &mut self,
                device: &str,
                idx: usize,
        ) {
                let max = self
                        .layout
                        .monitor(device)
                        .map(|ml| ml.workspaces.len().saturating_sub(1))
                        .unwrap_or(0);
                let idx = idx.min(max);
                if let Some(ml) = self.layout.monitor_mut(device)
                        && ml.active_workspace_idx != idx
                {
                        ml.previous_workspace_idx = Some(ml.active_workspace_idx);
                        ml.active_workspace_idx = idx;
                }
                // Move focus to the newly active workspace's focused window
                // (the ring follows; OS foreground stays put until close).
                let new_focus = self
                        .layout
                        .monitor(device)
                        .and_then(|ml| ml.workspaces.get(idx))
                        .and_then(|ws| ws.focused_id());
                self.focused = new_focus.map(|id| HWND(id as *mut _));
                if let Some(ov) = self.overviews.get_mut(device) {
                        let switch = self.config.animations.workspace_switch_params();
                        ov.camera.retarget(idx as f64, switch);
                        ov.sync_from_progress = None;
                }
                self.refresh_overview(device);
                self.update_focus_border();
        }

        /// Move the overview's internal selection (the keyboard nav niri
        /// hardcodes while the overview is open: Left/Right — plus our
        /// first/last/index aliases — act on the ACTIVE workspace). The
        /// layout focus, `self.focused` and the ring move; the thumbnail
        /// finals are rebuilt if the view scrolled. The OS foreground is
        /// left strictly alone (see `sync_focus_to_os` for why).
        fn overview_focus(
                &mut self,
                device: &str,
                action: Action,
        ) {
                // View position BEFORE the focus move: `view_pos` is relative
                // to the ACTIVE column, so measuring after the move would mix
                // the new column with the old offset (the v1 of this code did
                // exactly that and the shift came out near zero — the
                // "slides with a hitch, no animation" bug).
                let vp_before = self.active_view_pos(device);
                let changed = {
                        let Some(ml) = self.layout.monitor_mut(device) else {
                                return;
                        };
                        let ws = ml.active_workspace_mut();
                        match action {
                                Action::FocusColumnLeft => ws.focus_column(DirH::Left),
                                Action::FocusColumnRight => ws.focus_column(DirH::Right),
                                Action::FocusColumnFirst => ws.focus_column_edge(Edge::First),
                                Action::FocusColumnLast => ws.focus_column_edge(Edge::Last),
                                Action::FocusColumnIndex(n) => {
                                        ws.focus_column_index(n.saturating_sub(1) as usize)
                                },
                                _ => false,
                        }
                };
                if !changed {
                        return;
                }
                let Some(id) = self
                        .layout
                        .monitor(device)
                        .and_then(|ml| ml.active_workspace().focused_id())
                else {
                        return;
                };
                self.focused = Some(HWND(id as *mut _));
                // Refresh the workspace view (it is stored relative to the
                // active column, so the focus move changed its meaning) and
                // record the scroll as an animated shift so the thumbnails
                // glide sideways instead of jumping (niri's view-offset
                // animation inside the overview).
                self.overview_focus_view(device, id, vp_before);
                self.refresh_overview(device);
                self.update_focus_border();
        }

        /// View position (column-space x at the view's left edge) of
        /// `device`'s ACTIVE workspace — `None` if it has no columns.
        fn active_view_pos(
                &self,
                device: &str,
        ) -> Option<f64> {
                let mon = self.monitors.iter().find(|m| m.device == device)?;
                let view_width = (mon.work.right - mon.work.left) as f64;
                let ws = self.layout.monitor(device)?.active_workspace();
                let widths = geometry::column_widths(ws, &self.params, view_width);
                let xs = geometry::column_xs(&widths, self.params.gaps);
                Some(geometry::view_pos(ws, &xs))
        }

        /// `update_focus_view` for an open overview: refreshes the view
        /// offset of `id`'s workspace and, given the view position
        /// measured BEFORE the focus move (`vp_before`), records the
        /// scroll as an animated [`ViewShift`] on the device's overview
        /// so the thumbnails glide sideways instead of jumping. A second
        /// focus move while a shift is still in flight continues from its
        /// CURRENT offset, so rapid Alt+H/L repeats stay continuous.
        fn overview_focus_view(
                &mut self,
                device: &str,
                id: isize,
                vp_before: Option<f64>,
        ) {
                let Some(mon) = self.monitors.iter().find(|m| m.device == device) else {
                        return;
                };
                let view_width = (mon.work.right - mon.work.left) as f64;
                if let Some(ml) = self.layout.monitor_mut(device)
                        && let Some(idx) = ml.workspace_of(id)
                {
                        let ws = &mut ml.workspaces[idx];
                        geometry::refresh_view_offset(ws, &self.params, view_width, None);
                }
                let Some(vp_before) = vp_before else { return };
                let Some((ws_idx, vp_after)) =
                        view_position(&self.layout, device, id, &self.params, view_width)
                else {
                        return;
                };
                let prev = self
                        .overviews
                        .get(device)
                        .and_then(|ov| ov.view_shift.as_ref().filter(|s| s.ws_idx == ws_idx))
                        .map(|s| s.offset.value())
                        .unwrap_or(0.0);
                // The shift must CANCEL the scroll for the first frame:
                // rects_at renders x = base(vp_after) + offset*zoom, and the
                // pre-move frame was base(vp_before), so the offset starts
                // at (vp_after - vp_before) — how far the view scrolled — and
                // glides to 0. (vp_before - vp_after was inverted: focusing
                // right made every thumbnail jump ~2 screens left and glide
                // back — the "hitch + wrong direction" bug.)
                let start = prev + vp_after - vp_before;
                let Some(ov) = self.overviews.get_mut(device) else {
                        return;
                };
                if start.abs() >= 0.5 {
                        let params = self.config.animations.view_offset_params();
                        // Val::to parks at `start`; the retarget animates to 0.
                        let mut offset = crate::anim::Val::to(start, params);
                        offset.retarget(0.0, params);
                        ov.view_shift = Some(ViewShift { ws_idx, offset });
                } else {
                        ov.view_shift = None;
                }
        }

        /// A click at screen coords while an overview is open (captured
        /// by the low-level mouse hook — the colorkey backdrop is
        /// transparent to hit-testing, so the host never sees clicks).
        /// Hit-testing happens against the CURRENT thumbnail rects, so a
        /// click mid-animation selects where the user sees the window. A
        /// hit activates its workspace, focuses the window and closes
        /// the overview into it (niri's toggle-overview-to-workspace).
        /// Clicks on the empty backdrop do nothing (the press was
        /// already swallowed by the hook).
        fn overview_click_at(
                &mut self,
                sx: i32,
                sy: i32,
        ) {
                let Some((device, r)) = self.overviews.iter().find_map(|(d, ov)| {
                        let (fx, fy, fw, fh) = ov.full;
                        let inside = sx >= fx as i32
                                && sx < (fx + fw) as i32
                                && sy >= fy as i32
                                && sy < (fy + fh) as i32;
                        if !inside {
                                return None;
                        }
                        ov.current_rects()
                                .into_iter()
                                .find(|r| {
                                        sx >= r.x && sx < r.x + r.w && sy >= r.y && sy < r.y + r.h
                                })
                                .map(|r| (d.clone(), r))
                }) else {
                        return;
                };
                let id = r.id;
                let hwnd = HWND(id as *mut _);
                let Some(ws_idx) = self
                        .layout
                        .monitor(&device)
                        .and_then(|ml| ml.workspace_of(id))
                else {
                        return;
                };
                log::debug!(
                        "overview[{device}]: selecting window {id} (ws {})",
                        ws_idx + 1
                );
                // Switch the workspace first, then measure the view position
                // BEFORE moving the focus within it (view_pos is relative to
                // the active column).
                if let Some(ml) = self.layout.monitor_mut(&device) {
                        ml.active_workspace_idx = ws_idx;
                }
                let vp_before = self.active_view_pos(&device);
                if let Some(ml) = self.layout.monitor_mut(&device) {
                        ml.active_workspace_mut().focus_window(id);
                }
                self.focused = Some(hwnd);
                // Refresh the view at the clicked window and rebuild the
                // finals (at the new view position) BEFORE closing, so the
                // close animation starts from a continuous frame instead of
                // jumping to the new view at settle.
                self.overview_focus_view(&device, id, vp_before);
                self.refresh_overview(&device);
                self.close_overview_to(&device, Some(ws_idx));
        }

        /// Push the current open-overview work-area rects to the mouse
        /// hook, which intercepts+swallows button presses inside them.
        fn sync_overview_regions(&self) {
                let rects: Vec<(i32, i32, i32, i32)> = self
                        .overviews
                        .keys()
                        .filter_map(|d| self.monitors.iter().find(|m| &m.device == d))
                        .map(|m| (m.work.left, m.work.top, m.work.right, m.work.bottom))
                        .collect();
                crate::input::mouse::set_overview_regions(rects);
        }

        /// Re-raise every open overview backdrop above the real windows
        /// (focus changes raise the focused window) — bars are re-raised
        /// separately afterwards (they must stay above the backdrops).
        /// Also runs the "backdrop covered" diagnostic: if a MANAGED
        /// window sits directly above a freshly-raised backdrop, it is
        /// visibly covering the overview (the intermittent Bug 1) — log
        /// it (rate-limited) so the culprit and timing can be identified
        /// from the log.
        fn raise_overview_hosts(&self) {
                for (device, host) in &self.overview_hosts {
                        host.raise();
                        let Some(above) = host.window_above() else {
                                continue;
                        };
                        let is_participant = self.overviews.get(device).is_some_and(|ov| {
                                ov.finals
                                        .iter()
                                        .any(|(_, rs)| rs.iter().any(|r| r.id == above))
                        });
                        if is_participant {
                                BACKDROP_WARN_AT.with(|t| {
                    let now = std::time::Instant::now();
                    if t.get().is_none_or(|last| now.duration_since(last).as_millis() > 500) {
                        t.set(Some(now));
                        log::warn!(
                            "overview[{device}]: window {above} (class {}) is ABOVE the \
                             backdrop after raise",
                            crate::win::api::window_class(HWND(above as *mut _))
                        );
                    }
                });
                        }
                }
        }

        /// Rebuild an overview's participant set from the current layout
        /// (windows opened/closed while it is open) and apply one frame.
        /// Called from reflow(), which skips overview monitors' tiled
        /// windows entirely. The real windows follow the settled tiles
        /// behind the backdrop — instantly (it is invisible back there,
        /// and the close handover stays pixel-exact) — while the
        /// thumbnails show them scaled at the current zoom/camera.
        fn refresh_overview(
                &mut self,
                device: &str,
        ) {
                let Some(mon) = self.monitors.iter().find(|m| m.device == device) else {
                        return;
                };
                let Some(ml) = self.layout.monitor(device) else {
                        return;
                };
                let params = self.params.clone();
                let area = (
                        mon.work.left as f64,
                        mon.work.top as f64,
                        (mon.work.right - mon.work.left) as f64,
                        (mon.work.bottom - mon.work.top) as f64,
                );
                let mut finals = Vec::new();
                for (k, ws) in ml.workspaces.iter().enumerate() {
                        if ws.is_empty() {
                                continue;
                        }
                        finals.push((k, geometry::compute_workspace_geometry(ws, &params, area)));
                }
                let Some(ov) = self.overviews.get_mut(device) else {
                        return;
                };
                ov.finals = finals;
                // Real windows stay at their settled geometry, shown so DWM
                // can thumbnail them.
                let settled: Vec<geometry::TileRect> = ov
                        .finals
                        .iter()
                        .flat_map(|(_, rs)| rs.iter().cloned())
                        .collect();
                for r in &settled {
                        let hwnd = HWND(r.id as *mut _);
                        self.hidden_by_us.remove(&r.id);
                        if crate::win::api::is_alive(hwnd) {
                                placement::set_shown(hwnd, true);
                        }
                }
                // Keep the transport tracking the overview's current active
                // workspace / view, SNAPPED (not animating): the frame is
                // skipped for overview monitors, but this guarantees that when
                // the overview closes the transport already holds the exact
                // settled positions — no "slide back" from a stale pre-overview
                // spring the instant the zoom-in completes.
                let params = self.params.clone();
                self.transport.sync(&self.layout, &self.monitors, &params);
                self.transport.snap(device);
                // Thumbnails: reconcile the source set, then one frame at
                // the current zoom/camera.
                let sources: Vec<isize> = settled.iter().map(|r| r.id).collect();
                if let Some(host) = self.overview_hosts.get_mut(device) {
                        host.sync_sources(&sources);
                        host.update_rects(&ov.current_rects());
                        host.raise();
                }
                placement::raise_bars(&self.monitors);
        }

        /// Niri's do-screen-transition: suspend management and cover the
        /// focused window's monitor (fallback: primary) for ~1s — used by
        /// screenshot workflows so the WM (overlays, in-flight animations)
        /// neither interferes with the capture nor appears in it.
        fn begin_screen_transition(&mut self) {
                if self.overlay.is_none() {
                        log::warn!("do-screen-transition: no overlay window; ignoring");
                        return;
                }
                let mon = self
                        .focused
                        .and_then(|h| monitor::monitor_of_window(h, &self.monitors).cloned())
                        .or_else(|| monitor::primary(&self.monitors).cloned());
                let Some(m) = mon else {
                        log::warn!("do-screen-transition: no monitor to cover; ignoring");
                        return;
                };
                self.suspend_management();
                if let Some(ov) = self.overlay.as_ref() {
                        ov.show_transition(&m);
                }
        }

        /// Freeze all management: no reflows, no animation frames, no
        /// focus changes. In-flight transport springs are left where they
        /// are (the screen is covered by the transition cover anyway);
        /// `resume_management`'s reflow re-syncs everything.
        fn suspend_management(&mut self) {
                if self.suspended {
                        return;
                }
                self.suspended = true;
                // Open overviews drop their backdrops/thumbnails (the screen
                // is covered for the transition anyway), park the active
                // workspace's participants back at their settled tiles
                // (synchronously) and hide the rest. `resume_management`'s
                // reflow re-applies everything.
                let overviews = std::mem::take(&mut self.overviews);
                self.overview_hosts.clear();
                self.sync_overview_regions();
                for (device, ov) in overviews {
                        let active_idx = self
                                .layout
                                .monitor(&device)
                                .map(|ml| ml.active_workspace_idx);
                        let mut back: Vec<crate::layout::geometry::TileRect> = Vec::new();
                        for (k, rs) in &ov.finals {
                                if Some(*k) == active_idx {
                                        back.extend(rs.iter().cloned());
                                        continue;
                                }
                                for r in rs {
                                        let hwnd = HWND(r.id as *mut _);
                                        // Register BEFORE hiding (hook race).
                                        self.hidden_by_us.insert(r.id);
                                        if crate::win::api::is_alive(hwnd) {
                                                placement::set_shown(hwnd, false);
                                        }
                                }
                        }
                        placement::apply_geometry_sync(&back);
                        log::debug!("overview[{device}]: snapped closed on suspend");
                }
                if let Some(fb) = self.focus_border.as_ref() {
                        fb.hide();
                }
                log::info!("management suspended (screen transition)");
        }

        /// End a suspension: everything picked up where it left off.
        fn resume_management(&mut self) {
                if !self.suspended {
                        return;
                }
                self.suspended = false;
                log::info!("management resumed");
                self.reflow();
        }

        /// Handle a mouse event forwarded by the low-level hook:
        /// - wheel events act as niri-style `WheelScroll*` key binds
        ///   (default `Mod+Wheel` moves column focus, scrolling the view),
        /// - moves drive the optional focus-follows-mouse mode.
        fn handle_mouse_event(
                &mut self,
                ev: input::MouseEvent,
        ) {
                // Wheel binds and focus-follows-mouse are paused during a
                // screen transition (nothing should steal focus or move).
                if self.suspended {
                        return;
                }
                match ev.kind {
                        MouseKind::WheelV | MouseKind::WheelH => {
                                // In an open overview, a bare vertical wheel scrolls the
                                // workspace strip (niri maps it to focus-workspace-up/
                                // down under the mouse); wheel combos and horizontal
                                // wheels fall through to the configured binds.
                                if ev.kind == MouseKind::WheelV
                                        && !ev.mod_held
                                        && !ev.ctrl
                                        && !ev.shift
                                        && let Some(device) =
                                                monitor::monitor_at_cursor(&self.monitors)
                                                        .map(|m| m.device.clone())
                                        && self.overviews.contains_key(&device)
                                {
                                        self.overview_scroll(&device, ev.notches < 0);
                                        return;
                                }
                                let Some(vk) = ev.wheel_vk() else { return };
                                let key_ev = KeyEvent {
                                        vk,
                                        pressed: true,
                                        shift: ev.shift,
                                        ctrl: ev.ctrl,
                                        mod_held: ev.mod_held,
                                };
                                if let Some(action) = input::action_for(&self.config.binds, &key_ev)
                                {
                                        log::debug!("wheel action: {action:?}");
                                        self.dispatch(action);
                                }
                        },
                        MouseKind::Move => {
                                if self.config.focus_follows_mouse {
                                        self.focus_follows_mouse(ev.x, ev.y);
                                }
                        },
                        MouseKind::LButtonDown | MouseKind::RButtonDown => {
                                // Only fires inside an overview region (the hook
                                // intercepts and swallows those presses).
                                self.overview_click_at(ev.x, ev.y);
                        },
                }
        }

        /// Niri's focus-follows-mouse: hovering a managed window focuses
        /// it (layout focus + OS foreground). Skipped while the user is
        /// dragging/resizing, and only applies to windows that are actually
        /// visible (active workspace tiles and visible floats).
        fn focus_follows_mouse(
                &mut self,
                x: i32,
                y: i32,
        ) {
                if self.interacting_window.is_some() {
                        return;
                }
                let Some(hwnd) = crate::win::api::root_window_at(x, y) else {
                        return;
                };
                if !self.windows.contains(hwnd) || self.focused == Some(hwnd) {
                        return;
                }
                let id = hwnd.0 as isize;
                // Hovering a scaled-down overview window must not focus it:
                // focusing activates its workspace and fights the overview.
                if self.overviews.values().any(|ov| {
                        ov.finals
                                .iter()
                                .any(|(_, rs)| rs.iter().any(|r| r.id == id))
                }) {
                        return;
                }
                let visible_tiled = self.layout.monitors.iter().any(|m| {
                        m.workspace_of(id)
                                .is_some_and(|ws| ws == m.active_workspace_idx)
                });
                let visible_float = self
                        .floating
                        .get(&id)
                        .and_then(|fs| {
                                self.layout
                                        .monitor(&fs.device)
                                        .map(|ml| ml.active_workspace_idx == fs.workspace_idx)
                        })
                        .unwrap_or(false);
                if !visible_tiled && !visible_float {
                        return;
                }
                if self.layout.focus_window(id) {
                        self.focused = Some(hwnd);
                        self.update_focus_view(id);
                        self.reflow();
                        // Real focus follows too (Windows couples focus and
                        // foreground; failing is harmless, e.g. foreground lock).
                        crate::win::api::force_set_foreground(hwnd);
                        placement::raise_bars(&self.monitors);
                }
        }

        /// niri dynamic-workspace cleanup: drop empty non-active workspaces
        /// and keep one trailing empty. Skipped while a workspace switch is
        /// still animating or an overview owns the monitor (both hold
        /// workspace indices that reindexing would invalidate); the next
        /// removal cleans up instead.
        fn reclaim_workspaces(
                &mut self,
                device: &str,
        ) {
                if self.transport.switching(device) || self.overviews.contains_key(device) {
                        return;
                }
                if let Some(ml) = self.layout.monitor_mut(device) {
                        ml.normalize_workspaces();
                }
        }

        /// Handle a monitor-directional action. `kind`: 0 = focus-monitor,
        /// 1 = move-column-to-monitor, 2 = move-window-to-monitor.
        fn dispatch_monitor(
                &mut self,
                device: &str,
                dir: crate::win::monitor::Dir,
                kind: u8,
        ) {
                let Some(current) = self.monitors.iter().find(|m| m.device == device) else {
                        return;
                };
                let Some(target) =
                        crate::win::monitor::monitor_in_direction(&self.monitors, current, dir)
                else {
                        return;
                };
                let target_device = target.device.clone();

                let moved_focus = match kind {
                        1 => self
                                .layout
                                .move_focused_column_to_monitor(device, &target_device)
                                .and_then(|ids| ids.first().copied()),
                        2 => self
                                .layout
                                .move_focused_window_to_monitor(device, &target_device),
                        _ => None,
                };

                // After a move, focus follows the moved window to the target
                // monitor; for a plain focus-monitor, adopt whatever the target
                // monitor currently focuses.
                let new_focus = match kind {
                        0 => self.layout.focused_id(&target_device),
                        _ => moved_focus,
                };
                if kind != 0 && moved_focus.is_none() {
                        // Nothing to move (empty column / no focus): no-op.
                        return;
                }
                // A cross-monitor move may have emptied a workspace on the
                // source monitor; clean it up (no slide runs for cross-monitor
                // moves, so reindexing is safe).
                if kind != 0 {
                        self.reclaim_workspaces(device);
                }
                self.focused = new_focus.map(|id| HWND(id as *mut _));
                if let Some(id) = new_focus {
                        self.update_focus_view(id);
                }
                self.reflow();
                self.sync_focus_to_os();
        }

        /// Execute a bound action. The navigation subset works on the
        /// focused window's workspace.
        fn dispatch(
                &mut self,
                action: Action,
        ) {
                use Action::*;
                // While management is suspended (screen transition), the
                // screen is covered and windows must not move: only quit and
                // restarting the transition make sense.
                if self.suspended {
                        match action {
                                Quit => unsafe { PostQuitMessage(0) },
                                DoScreenTransition => self.begin_screen_transition(),
                                _ => {},
                        }
                        return;
                }
                if matches!(action, DoScreenTransition) {
                        self.begin_screen_transition();
                        return;
                }
                // Resolve the device for this action: the monitor holding the
                // focused window. When nothing is focused (e.g. we're on an
                // empty workspace), fall back to the monitor under the cursor
                // — workspace switches, spawn and quit must keep working on
                // an empty workspace (niri semantics: you can always switch
                // away from an empty one).
                let focused_id = self.focused.map(|h| h.0 as isize).or_else(|| {
                        let cursor_mon = monitor::monitor_at_cursor(&self.monitors)?;
                        self.layout
                                .monitor(&cursor_mon.device)
                                .and_then(|m| m.active_workspace().focused_id())
                });

                let device = focused_id
                        .and_then(|id| {
                                self.layout
                                        .monitors
                                        .iter()
                                        .find_map(|m| m.workspace_of(id).map(|_| m.device.clone()))
                        })
                        .or_else(|| {
                                monitor::monitor_at_cursor(&self.monitors).map(|m| m.device.clone())
                        });
                let Some(device) = device else { return };

                // The overview owns several actions while open: they act on the
                // per-monitor overview state (camera scroll) instead of the
                // layout, and the layout borrow below must not run.
                match action {
                        Action::ToggleOverview => {
                                self.toggle_overview(&device);
                                return;
                        },
                        Action::FocusWindowDown if self.overviews.contains_key(&device) => {
                                let cur = self
                                        .layout
                                        .monitor(&device)
                                        .map(|ml| ml.active_workspace_idx)
                                        .unwrap_or(0);
                                self.overview_activate(&device, cur + 1);
                                return;
                        },
                        Action::FocusWindowUp if self.overviews.contains_key(&device) => {
                                let cur = self
                                        .layout
                                        .monitor(&device)
                                        .map(|ml| ml.active_workspace_idx)
                                        .unwrap_or(0);
                                self.overview_activate(&device, cur.saturating_sub(1));
                                return;
                        },
                        // Column focus in the overview moves the INTERNAL
                        // selection only (niri's hardcoded overview binds:
                        // Left/Right = FocusColumnLeft/Right on the ACTIVE
                        // workspace). Crucially this path never reaches
                        // sync_focus_to_os: force_set_foreground makes the target
                        // window raise itself above the backdrop asynchronously
                        // (from WM_ACTIVATE), which no amount of re-raising can
                        // reliably outrun — the root cause of the "big window
                        // covers the small ones" bug. The OS foreground catches
                        // up once when the overview closes.
                        Action::FocusColumnLeft
                        | Action::FocusColumnRight
                        | Action::FocusColumnFirst
                        | Action::FocusColumnLast
                        | Action::FocusColumnIndex(_)
                                if self.overviews.contains_key(&device) =>
                        {
                                self.overview_focus(&device, action);
                                return;
                        },
                        Action::FocusWorkspace(n) | Action::WorkspaceSwitch(n)
                                if self.overviews.contains_key(&device) =>
                        {
                                self.overview_activate(&device, n.saturating_sub(1) as usize);
                                return;
                        },
                        Action::FocusWorkspaceDown if self.overviews.contains_key(&device) => {
                                let cur = self
                                        .layout
                                        .monitor(&device)
                                        .map(|ml| ml.active_workspace_idx)
                                        .unwrap_or(0);
                                self.overview_activate(&device, cur + 1);
                                return;
                        },
                        Action::FocusWorkspaceUp if self.overviews.contains_key(&device) => {
                                let cur = self
                                        .layout
                                        .monitor(&device)
                                        .map(|ml| ml.active_workspace_idx)
                                        .unwrap_or(0);
                                self.overview_activate(&device, cur.saturating_sub(1));
                                return;
                        },
                        Action::FocusWorkspacePrevious if self.overviews.contains_key(&device) => {
                                if let Some(prev) = self
                                        .layout
                                        .monitor(&device)
                                        .and_then(|ml| ml.previous_workspace_idx)
                                {
                                        self.overview_activate(&device, prev);
                                }
                                return;
                        },
                        _ => {},
                }

                // Monitor navigation crosses the per-monitor layout, so it is
                // handled before the single-monitor layout borrow below.
                use crate::win::monitor::Dir;
                let monitor_dir = match action {
                        Action::FocusMonitorLeft => Some((Dir::Left, 0u8)),
                        Action::FocusMonitorRight => Some((Dir::Right, 0)),
                        Action::FocusMonitorUp => Some((Dir::Up, 0)),
                        Action::FocusMonitorDown => Some((Dir::Down, 0)),
                        Action::MoveColumnToMonitorLeft => Some((Dir::Left, 1)),
                        Action::MoveColumnToMonitorRight => Some((Dir::Right, 1)),
                        Action::MoveColumnToMonitorUp => Some((Dir::Up, 1)),
                        Action::MoveColumnToMonitorDown => Some((Dir::Down, 1)),
                        Action::MoveWindowToMonitorLeft => Some((Dir::Left, 2)),
                        Action::MoveWindowToMonitorRight => Some((Dir::Right, 2)),
                        Action::MoveWindowToMonitorUp => Some((Dir::Up, 2)),
                        Action::MoveWindowToMonitorDown => Some((Dir::Down, 2)),
                        _ => None,
                };
                if let Some((dir, kind)) = monitor_dir {
                        self.dispatch_monitor(&device, dir, kind);
                        return;
                }

                // Floating windows are not in the tiling: handle the small
                // action subset that applies to them directly.
                if let Some(id) = focused_id
                        && self.floating.contains_key(&id)
                {
                        if matches!(action, Action::ToggleWindowFloating) {
                                let fs = self.floating.get(&id).cloned().unwrap();
                                self.unfloat_window(id, fs);
                                return;
                        }
                        // These apply to the workspace, not the float itself.
                        if matches!(action, Action::CloseWindow) {
                                self.close_window(id);
                                return;
                        }
                        // Map a few tiling actions to float move/resize.
                        let mut touched = false;
                        if let Some(fs) = self.floating.get_mut(&id) {
                                const STEP: f64 = 50.0;
                                match action {
                                        Action::MoveColumnLeft => {
                                                fs.x -= STEP;
                                                touched = true;
                                        },
                                        Action::MoveColumnRight => {
                                                fs.x += STEP;
                                                touched = true;
                                        },
                                        Action::MoveWindowUp => {
                                                fs.y -= STEP;
                                                touched = true;
                                        },
                                        Action::MoveWindowDown => {
                                                fs.y += STEP;
                                                touched = true;
                                        },
                                        Action::SetColumnWidth(spec) => {
                                                if let Some(change) = SizeChange::parse(&spec) {
                                                        match change {
                                                                SizeChange::Delta(d) => {
                                                                        fs.w = (fs.w + d).max(100.0)
                                                                },
                                                                SizeChange::Fixed(f) => {
                                                                        fs.w = f.max(100.0)
                                                                },
                                                                SizeChange::Proportion(p) => {
                                                                        fs.w = (fs.w * p
                                                                                .clamp(0.05, 20.0))
                                                                        .max(100.0)
                                                                },
                                                                SizeChange::ProportionDelta(dp) => {
                                                                        fs.w = (fs.w * (1.0 + dp))
                                                                                .max(100.0)
                                                                },
                                                        }
                                                        touched = true;
                                                }
                                        },
                                        Action::SetWindowHeight(spec) => {
                                                if let Some(change) = SizeChange::parse(&spec) {
                                                        match change {
                                                                SizeChange::Delta(d) => {
                                                                        fs.h = (fs.h + d).max(100.0)
                                                                },
                                                                SizeChange::Fixed(f) => {
                                                                        fs.h = f.max(100.0)
                                                                },
                                                                SizeChange::Proportion(p) => {
                                                                        fs.h = (fs.h * p
                                                                                .clamp(0.05, 20.0))
                                                                        .max(100.0)
                                                                },
                                                                SizeChange::ProportionDelta(dp) => {
                                                                        fs.h = (fs.h * (1.0 + dp))
                                                                                .max(100.0)
                                                                },
                                                        }
                                                        touched = true;
                                                }
                                        },
                                        _ => {},
                                }
                                // Keep at least 100 px of the float on its monitor.
                                if touched
                                        && let Some(m) =
                                                self.monitors.iter().find(|m| m.device == fs.device)
                                {
                                        fs.x = fs.x.clamp(
                                                m.work.left as f64 - fs.w + 100.0,
                                                m.work.right as f64 - 100.0,
                                        );
                                        fs.y = fs.y.clamp(
                                                m.work.top as f64 - fs.h + 100.0,
                                                m.work.bottom as f64 - 100.0,
                                        );
                                }
                        }
                        if touched {
                                self.reflow();
                        }
                        // Other tiling actions fall through to the tiling below.
                        return;
                }
                // Tiling -> float happens before the layout lookup too (the
                // window leaves the layout immediately).
                if let Some(id) = focused_id
                        && matches!(action, Action::ToggleWindowFloating)
                {
                        self.float_window(id);
                        return;
                }

                let mut changed = false;
                // center-column sets the view offset directly; skip the
                // post-action refresh_view_offset that would undo it.
                let mut skip_view_refresh = false;
                // Windowed-fullscreen bookkeeping, applied after the layout
                // borrow ends (see below).
                let mut fullscreen_prev: Option<isize> = None;
                let mut fullscreen_now: Option<isize> = None;
                // Workspace we switched to (for the indicator overlay), if any.
                let mut ws_switch: Option<usize> = None;
                // Workspace we switched FROM (drives the slide direction).
                let mut ws_prev: Option<usize> = None;
                // Preset column widths for the bare set-column-width (cloned
                // out before the layout borrow below), plus the geometry
                // context needed to resolve them like niri's toggle_width.
                let presets = self.params.preset_column_widths.clone();
                let layout_params = self.params.clone();
                let view_width = self
                        .monitors
                        .iter()
                        .find(|m| m.device == device)
                        .map(|m| (m.work.right - m.work.left) as f64)
                        .unwrap_or(1920.0);
                {
                        let monitor_layout = self.layout.monitor_mut(&device).unwrap();
                        let ws = monitor_layout.active_workspace_mut();
                        match action {
                                FocusColumnLeft => changed = ws.focus_column(DirH::Left),
                                FocusColumnRight => changed = ws.focus_column(DirH::Right),
                                FocusWindowDown => changed = ws.focus_tile(DirV::Down),
                                FocusWindowUp => changed = ws.focus_tile(DirV::Up),
                                MoveColumnLeft => changed = ws.move_column(DirH::Left),
                                MoveColumnRight => changed = ws.move_column(DirH::Right),
                                MoveWindowDown => changed = ws.move_tile(DirV::Down),
                                MoveWindowUp => changed = ws.move_tile(DirV::Up),
                                MoveWindowToColumnLeft => changed = ws.move_tile_across(DirH::Left),
                                MoveWindowToColumnRight => {
                                        changed = ws.move_tile_across(DirH::Right)
                                },
                                ConsumeOrExpelWindowLeft => {
                                        changed = ws.consume_or_expel(DirH::Left)
                                },
                                ConsumeOrExpelWindowRight => {
                                        changed = ws.consume_or_expel(DirH::Right)
                                },
                                ConsumeWindowIntoColumn => {
                                        changed = ws.consume_window_into_column()
                                },
                                ExpelWindowFromColumn => changed = ws.expel_window_from_column(),
                                FocusColumnFirst => changed = ws.focus_column_edge(Edge::First),
                                FocusColumnLast => changed = ws.focus_column_edge(Edge::Last),
                                FocusColumnIndex(n) => {
                                        changed =
                                                ws.focus_column_index(n.saturating_sub(1) as usize)
                                },
                                SetColumnWidth(spec) => {
                                        match SizeChange::parse(&spec) {
                                                Some(change) => {
                                                        changed = ws.set_column_width(
                                                                &change,
                                                                &layout_params,
                                                                view_width,
                                                        )
                                                },
                                                // Bare `set-column-width;` cycles the presets
                                                // (niri's preset-column-widths).
                                                None => {
                                                        changed = ws.cycle_column_width(
                                                                &presets,
                                                                &layout_params,
                                                                view_width,
                                                        )
                                                },
                                        }
                                },
                                SwitchPresetColumnWidth => {
                                        changed = ws.cycle_column_width(
                                                &presets,
                                                &layout_params,
                                                view_width,
                                        );
                                },
                                SwitchPresetColumnWidthBack => {
                                        changed = ws.cycle_column_width_dir(
                                                &presets,
                                                &layout_params,
                                                view_width,
                                                false,
                                        );
                                },
                                MoveColumnToFirst => changed = ws.move_column_to_edge(Edge::First),
                                MoveColumnToLast => changed = ws.move_column_to_edge(Edge::Last),
                                SetWindowHeight(spec) => {
                                        if let Some(change) = SizeChange::parse(&spec) {
                                                changed = ws.set_window_height(&change);
                                        }
                                },
                                ToggleFullWidth => changed = ws.toggle_full_width(),
                                MaximizeColumn => changed = ws.toggle_maximized(),
                                CenterColumn => {
                                        changed =
                                                ws.center_active_column(&layout_params, view_width);
                                        skip_view_refresh = true;
                                },
                                ToggleWindowedFullscreen => {
                                        fullscreen_prev = ws.fullscreen_id;
                                        changed = ws.toggle_fullscreen();
                                        fullscreen_now = ws.fullscreen_id;
                                },
                                FocusWorkspace(n) | WorkspaceSwitch(n) => {
                                        let idx = n.saturating_sub(1) as usize;
                                        log::debug!(
                                                "ws switch: target idx={idx} active={} (device {device})",
                                                monitor_layout.active_workspace_idx
                                        );
                                        let prev = monitor_layout.active_workspace_idx;
                                        changed = monitor_layout.switch_workspace(idx);
                                        if changed {
                                                ws_switch = Some(idx);
                                                ws_prev = Some(prev);
                                        }
                                },
                                FocusWorkspaceDown | FocusWorkspaceUp => {
                                        let prev = monitor_layout.active_workspace_idx;
                                        let last =
                                                monitor_layout.workspaces.len().saturating_sub(1);
                                        let idx = if matches!(action, FocusWorkspaceDown) {
                                                (prev + 1).min(last)
                                        } else {
                                                prev.saturating_sub(1)
                                        };
                                        changed = monitor_layout.switch_workspace(idx);
                                        if changed {
                                                ws_switch = Some(idx);
                                                ws_prev = Some(prev);
                                        }
                                },
                                FocusWorkspacePrevious => {
                                        let prev = monitor_layout.active_workspace_idx;
                                        if let Some(target) = monitor_layout.previous_workspace_idx
                                                && target != prev
                                        {
                                                changed = monitor_layout.switch_workspace(target);
                                                if changed {
                                                        ws_switch = Some(target);
                                                        ws_prev = Some(prev);
                                                }
                                        }
                                },
                                MoveWindowToWorkspace(n) => {
                                        let idx = n.saturating_sub(1) as usize;
                                        let prev = monitor_layout.active_workspace_idx;
                                        changed = monitor_layout
                                                .move_focused_window_to_workspace(idx, true)
                                                .is_some();
                                        if changed {
                                                ws_switch = Some(idx);
                                                ws_prev = Some(prev);
                                        }
                                },
                                MoveWindowToWorkspaceDown | MoveWindowToWorkspaceUp => {
                                        let prev = monitor_layout.active_workspace_idx;
                                        let idx = if matches!(action, MoveWindowToWorkspaceDown) {
                                                prev + 1
                                        } else {
                                                prev.saturating_sub(1)
                                        };
                                        // Down past the end grows a new workspace; up at
                                        // the top is a no-op.
                                        if idx != prev
                                                || matches!(action, MoveWindowToWorkspaceDown)
                                        {
                                                changed = monitor_layout
                                                        .move_focused_window_to_workspace(idx, true)
                                                        .is_some();
                                        }
                                        if changed {
                                                ws_switch = Some(idx);
                                                ws_prev = Some(prev);
                                        }
                                },
                                MoveColumnToWorkspace(n) => {
                                        let idx = n.saturating_sub(1) as usize;
                                        let prev = monitor_layout.active_workspace_idx;
                                        changed = monitor_layout
                                                .move_focused_column_to_workspace(idx, true)
                                                .is_some();
                                        if changed {
                                                ws_switch = Some(idx);
                                                ws_prev = Some(prev);
                                        }
                                },
                                MoveColumnToWorkspaceDown | MoveColumnToWorkspaceUp => {
                                        let prev = monitor_layout.active_workspace_idx;
                                        let idx = if matches!(action, MoveColumnToWorkspaceDown) {
                                                prev + 1
                                        } else {
                                                prev.saturating_sub(1)
                                        };
                                        if idx != prev
                                                || matches!(action, MoveColumnToWorkspaceDown)
                                        {
                                                changed = monitor_layout
                                                        .move_focused_column_to_workspace(idx, true)
                                                        .is_some();
                                        }
                                        if changed {
                                                ws_switch = Some(idx);
                                                ws_prev = Some(prev);
                                        }
                                },
                                Spawn(cmd) => {
                                        self.spawn(&cmd);
                                },
                                Quit => {
                                        unsafe { PostQuitMessage(0) };
                                },
                                CloseWindow => {
                                        if let Some(id) = focused_id {
                                                self.close_window(id);
                                        }
                                },
                                _ => {},
                        }
                }
                if changed {
                        // Apply decoration changes for windowed fullscreen.
                        if let Some(prev_id) = fullscreen_prev {
                                self.restore_borders(prev_id);
                        }
                        if let Some(cur_id) = fullscreen_now {
                                let hwnd = HWND(cur_id as *mut _);
                                if crate::win::api::is_alive(hwnd) {
                                        placement::set_borderless(hwnd, true);
                                        placement::raise(hwnd);
                                        self.borderless.insert(cur_id);
                                }
                        }
                        if let Some(idx) = ws_switch {
                                // Focus follows the workspace switch (niri semantics):
                                // adopt the new workspace's focused window, if any.
                                // Otherwise `self.focused` would still point at the old
                                // workspace's window — hidden by reflow — and
                                // sync_focus_to_os would push OS focus onto it.
                                let new_id = self
                                        .layout
                                        .monitor(&device)
                                        .and_then(|ml| ml.workspaces.get(idx))
                                        .and_then(|ws| ws.focused_id());
                                self.focused = new_id.map(|nid| HWND(nid as *mut _));
                                if let Some(nid) = new_id {
                                        // Refresh the view offset of the workspace we
                                        // switched TO, not the one we came from.
                                        self.update_focus_view(nid);
                                }
                                // The transport's workspace-switch camera animates
                                // the vertical slide; a plain reflow retargets it.
                                let _ = ws_prev;
                                self.reflow();
                        } else {
                                if let Some(id) = focused_id
                                        && !skip_view_refresh
                                {
                                        self.update_focus_view(id);
                                }
                                self.reflow();
                        }
                        self.sync_focus_to_os();
                }
                if let Some(idx) = ws_switch {
                        self.show_workspace_overlay(&device, idx);
                }
        }

        /// Poll the config file for changes (TIMER_CONFIG). On a change,
        /// reload and apply everything that can be applied live.
        fn maybe_reload_config(&mut self) {
                let Ok(meta) = std::fs::metadata(config::config_path()) else {
                        return;
                };
                let Ok(mtime) = meta.modified() else {
                        return;
                };
                if Some(mtime) == self.config_mtime {
                        return;
                }
                self.config_mtime = Some(mtime);
                match config::try_load() {
                        Ok(cfg) => {
                                log::info!("config changed on disk; reloading");
                                self.apply_config(cfg);
                        },
                        Err(err) => {
                                log::warn!(
                                        "config reload failed ({err}); keeping the previous config"
                                );
                        },
                }
        }

        /// Apply a (possibly new) configuration live: layout params, focus
        /// ring, animation tuning, binds and the Mod key.
        fn apply_config(
                &mut self,
                cfg: Config,
        ) {
                let mod_changed = cfg.mod_key != self.config.mod_key;
                self.transport.set_params(
                        cfg.animations.movement_params(),
                        cfg.animations.resize_params(),
                        cfg.animations.view_offset_params(),
                        cfg.animations.workspace_switch_params(),
                );
                self.params = cfg.layout.clone();
                if mod_changed {
                        input::set_mod_key(cfg.mod_key.clone());
                        log::info!("mod key changed to {:?}", cfg.mod_key);
                }
                self.config = cfg;
                input::update_binds(&self.config.binds);
                self.reflow();
        }

        /// Place the focus ring around the currently focused window (its
        /// live on-screen rect, so it follows animations). Hidden when
        /// disabled, during interactions, or when the focused window is
        /// not visible (inactive workspace / untracked).
        fn update_focus_border(&self) {
                let Some(fb) = &self.focus_border else { return };
                let ring = &self.config.focus_ring;
                if !ring.enabled || self.interacting_window.is_some() {
                        fb.hide();
                        return;
                }
                // The focused window: OS foreground, else the layout focus of
                // the monitor under the cursor.
                let candidate = self.focused.or_else(|| {
                        let device = monitor::monitor_at_cursor(&self.monitors)
                                .map(|m| m.device.clone())
                                .or_else(|| self.monitors.first().map(|m| m.device.clone()))?;
                        let id = self.layout.focused_id(&device)?;
                        Some(HWND(id as *mut _))
                });
                let Some(hwnd) = candidate else {
                        fb.hide();
                        return;
                };
                let id = hwnd.0 as isize;
                // In an overview the focused window is scaled down; the ring
                // must outline the scaled rect (the real HWND rect also lags
                // the zoom animation by a frame).
                if let Some(r) = self
                        .overviews
                        .values()
                        .find_map(|ov| ov.current_rects().into_iter().find(|r| r.id == id))
                {
                        // Config stores 0xRRGGBB; COLORREF wants 0x00BBGGRR.
                        let rgb = ring.active_color;
                        let bgr = ((rgb & 0xFF) << 16) | (rgb & 0x00_FF_00) | ((rgb >> 16) & 0xFF);
                        fb.update(
                                r.x,
                                r.y,
                                r.w,
                                r.h,
                                ring.width,
                                ring.radius,
                                windows::Win32::Foundation::COLORREF(bgr),
                        );
                        return;
                }
                let visible_float = self
                        .floating
                        .get(&id)
                        .and_then(|fs| {
                                self.layout
                                        .monitor(&fs.device)
                                        .map(|ml| ml.active_workspace_idx == fs.workspace_idx)
                        })
                        .unwrap_or(false);
                let visible_tiled = self.layout.monitors.iter().any(|m| {
                        m.workspace_of(id)
                                .is_some_and(|ws| ws == m.active_workspace_idx)
                });
                if !crate::win::api::is_alive(hwnd) || (!visible_float && !visible_tiled) {
                        fb.hide();
                        return;
                }
                // Hug the *visible* window edge: `frame_rect` (DWM extended
                // frame bounds) skips the invisible resize borders that
                // `window_rect` includes — the ring would otherwise float
                // ~10px off the window on three sides.
                //
                // While the window animates, the HWND's rects are stale: moves
                // go out with SWP_ASYNCWINDOWPOS, so right after a tick the
                // window is still at the previous frame's position — reading
                // it makes the ring trail the window. Use the transport's
                // in-flight rect (what we just sent) plus the visible-edge
                // insets measured from the live rects (insets don't change
                // mid-motion; both rects are stale by the same amount).
                let rect = if let Some((ax, ay, aw, ah)) =
                        self.transport.window_rect(&self.layout, &self.monitors, id)
                        && self.transport.animating()
                        && let (Some(win), Some(frame)) = (
                                crate::win::api::window_rect(hwnd),
                                crate::win::api::frame_rect(hwnd),
                        ) {
                        let in_l = frame.0 - win.0;
                        let in_t = frame.1 - win.1;
                        let in_r = (win.0 + win.2) - (frame.0 + frame.2);
                        let in_b = (win.1 + win.3) - (frame.1 + frame.3);
                        Some((
                                ax as f64 + in_l,
                                ay as f64 + in_t,
                                aw as f64 - in_l - in_r,
                                ah as f64 - in_t - in_b,
                        ))
                } else {
                        crate::win::api::frame_rect(hwnd)
                };
                if let Some((x, y, w, h)) = rect {
                        // Config stores 0xRRGGBB; COLORREF wants 0x00BBGGRR.
                        let rgb = ring.active_color;
                        let bgr = ((rgb & 0xFF) << 16) | (rgb & 0x00_FF_00) | ((rgb >> 16) & 0xFF);
                        fb.update(
                                x as i32,
                                y as i32,
                                w as i32,
                                h as i32,
                                ring.width,
                                ring.radius,
                                windows::Win32::Foundation::COLORREF(bgr),
                        );
                } else {
                        fb.hide();
                }
        }

        /// Flash the "Workspace N" indicator on a monitor.
        fn show_workspace_overlay(
                &self,
                device: &str,
                idx: usize,
        ) {
                let Some(mon) = self.monitors.iter().find(|m| m.device == device) else {
                        return;
                };
                if let Some(ov) = &self.overlay {
                        ov.show_workspace(mon, idx + 1);
                }
        }

        /// Display topology changed: re-enumerate monitors, rehome windows
        /// from gone monitors, adopt new ones, re-tile.
        fn on_display_change(&mut self) {
                log::info!("display topology changed; re-enumerating monitors");
                // The bar cache was computed against the old topology.
                placement::invalidate_bar_cache();
                let new_monitors = monitor::enumerate();
                let new_devices: Vec<String> =
                        new_monitors.iter().map(|m| m.device.clone()).collect();

                let gone: Vec<String> = self
                        .monitors
                        .iter()
                        .map(|m| m.device.clone())
                        .filter(|d| !new_devices.contains(d))
                        .collect();
                for device in gone {
                        self.transport.remove_monitor(&device);
                        if let Some(ml) = self.layout.remove_monitor(&device) {
                                let ids: Vec<isize> = ml
                                        .workspaces
                                        .iter()
                                        .flat_map(|ws| ws.window_ids())
                                        .collect();
                                if ids.is_empty() {
                                        continue;
                                }
                                match self.layout.monitors.first().map(|m| m.device.clone()) {
                                        Some(target) => {
                                                log::info!(
                                                        "rehoming {} window(s) from {device} to {target}",
                                                        ids.len()
                                                );
                                                for id in ids {
                                                        self.layout.add_window(&target, id);
                                                }
                                        },
                                        None => log::warn!(
                                                "no monitor left; {} window(s) left in place",
                                                ids.len()
                                        ),
                                }
                        }
                }
                for m in &new_monitors {
                        self.layout.add_monitor(&m.device);
                }
                // Overviews reference monitor geometry that just changed;
                // drop them: park every participant back at its settled tile
                // (the reflow below re-applies normal tiling, incl. hiding
                // the non-active workspaces).
                let dropped = std::mem::take(&mut self.overviews);
                self.overview_hosts.clear();
                self.sync_overview_regions();
                for (_, ov) in dropped {
                        let all: Vec<crate::layout::geometry::TileRect> = ov
                                .finals
                                .iter()
                                .flat_map(|(_, rs)| rs.iter().cloned())
                                .collect();
                        placement::apply_geometry_sync(&all);
                }
                self.monitors = new_monitors;
                self.reflow();
        }

        /// Restore decorations on a window we borderlessed. Safe to call
        /// for windows that no longer exist.
        fn restore_borders(
                &mut self,
                id: isize,
        ) {
                if self.borderless.remove(&id) {
                        let hwnd = HWND(id as *mut _);
                        if crate::win::api::is_alive(hwnd) {
                                placement::set_borderless(hwnd, false);
                        }
                }
        }

        /// Close a window politely (WM_CLOSE, lets apps prompt/save).
        fn close_window(
                &mut self,
                id: isize,
        ) {
                let hwnd = windows::Win32::Foundation::HWND(id as *mut _);
                unsafe {
                        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                                Some(hwnd),
                                windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                                WPARAM(0),
                                LPARAM(0),
                        );
                }
        }

        /// A drag of a *tiled* window ended at the current cursor position:
        /// reorder niri-style (drag-and-drop). Dropping in the gap between
        /// columns creates a standalone column there (keeping its width);
        /// dropping onto a column's span inserts the window into that
        /// column's tile stack at the height the cursor points at; dropping
        /// on another monitor moves the window to that output.
        fn handle_tiled_drag_end(
                &mut self,
                hwnd: HWND,
        ) {
                let id = hwnd.0 as isize;

                // (Clicks during an open overview never reach real windows:
                // the backdrop host covers the monitor and handles them via
                // `overview_click`.)

                // Tell real drags apart from clicks / tiny nudges: only a
                // meaningful position change triggers a reorder.
                let start = self.interact_start_rect.take();
                if let (Some(s), Some(e)) = (start, crate::win::api::window_rect(hwnd))
                        && (s.0 - e.0).abs() < 10.0
                        && (s.1 - e.1).abs() < 10.0
                {
                        self.reflow();
                        return;
                }

                let Some((cx, cy)) = crate::win::api::cursor_pos() else {
                        self.reflow();
                        return;
                };

                // Where the window came from.
                let Some(source_device) = self
                        .layout
                        .monitors
                        .iter()
                        .find_map(|m| m.workspace_of(id).map(|_| m.device.clone()))
                else {
                        self.reflow();
                        return;
                };

                // Cross-monitor drag: move to the drop monitor's active
                // workspace as a new column (niri moves windows between
                // outputs this way).
                let drop_device = monitor::monitor_at_cursor(&self.monitors)
                        .map(|m| m.device.clone())
                        .unwrap_or_else(|| source_device.clone());
                if drop_device != source_device {
                        log::info!("drag: window {id} moved to monitor {drop_device}");
                        self.layout.remove_window(id);
                        self.layout.add_window(&drop_device, id);
                        self.reflow();
                        return;
                }

                // Capture the on-screen geometry *before* removing the window:
                // reflow was paused during the drag, so these rects match what
                // the user sees. The dragged window's own rects are skipped, so
                // a column that only held it is not a drop target.
                let params = self.params.clone();
                let Some(mon) = self.monitors.iter().find(|m| m.device == source_device) else {
                        self.reflow();
                        return;
                };
                let area = (
                        mon.work.left as f64,
                        mon.work.top as f64,
                        (mon.work.right - mon.work.left) as f64,
                        (mon.work.bottom - mon.work.top) as f64,
                );
                let Some(ml) = self.layout.monitor(&source_device) else {
                        self.reflow();
                        return;
                };
                let fullscreen_drag = ml.active_workspace().fullscreen_id == Some(id);
                // Windowed-fullscreen windows are not part of the visible tile
                // grid; dragging one just snaps it back.
                if fullscreen_drag {
                        self.reflow();
                        return;
                }
                let ws = ml.active_workspace();
                let rects = geometry::compute_workspace_geometry(ws, &params, area);
                let mut col_rects: Vec<Vec<geometry::TileRect>> =
                        vec![Vec::new(); ws.columns.len()];
                for r in rects {
                        if r.id == id {
                                continue;
                        }
                        if let Some((ci, _)) = ws.find(r.id) {
                                col_rects[ci].push(r);
                        }
                }
                // Keep the column width for a standalone re-insertion.
                let old_width = ws.find(id).and_then(|(ci, _)| ws.columns[ci].width);

                // Decide the drop from the cursor position:
                // - inside a column's span (plus half a gap): into that column,
                //   at the tile slot the cursor points at (remembered via the
                //   anchor tile it lands on, so index shifts after the removal
                //   don't matter);
                // - otherwise: a standalone column right after the last column
                //   fully left of the cursor.
                let gap = params.gaps;
                let mut into: Option<(isize, bool)> = None; // (anchor tile, insert after it?)
                let mut left_of: Option<isize> = None; // anchor of the last column left of the cursor
                for rects in &col_rects {
                        let Some(first) = rects.first() else { continue };
                        let (x, w) = (first.x as f64, first.w as f64);
                        let left = x - gap / 2.0;
                        let right = x + w + gap / 2.0;
                        if cx >= left && cx < right {
                                let mut slot = rects.len();
                                for (i, r) in rects.iter().enumerate() {
                                        if cy < r.y as f64 + r.h as f64 / 2.0 {
                                                slot = i;
                                                break;
                                        }
                                }
                                let (anchor, after) = if slot < rects.len() {
                                        (rects[slot].id, false)
                                } else {
                                        (rects[rects.len() - 1].id, true)
                                };
                                into = Some((anchor, after));
                                break;
                        }
                        if cx >= right {
                                left_of = Some(first.id);
                        }
                }

                // Apply: remove, then re-insert at the resolved position.
                let Some(ml) = self.layout.monitor_mut(&source_device) else {
                        self.reflow();
                        return;
                };
                ml.active_workspace_mut().remove_window(id);
                let ws = ml.active_workspace_mut();
                match into {
                        Some((anchor, after)) => {
                                if let Some((ci, ti)) = ws.find(anchor) {
                                        let tile_idx = if after { ti + 1 } else { ti };
                                        ws.add_window_to_column(id, ci, tile_idx);
                                } else {
                                        ws.insert_column_at(ws.columns.len(), id);
                                }
                        },
                        None => {
                                let idx = left_of
                                        .and_then(|anchor| ws.find(anchor).map(|(ci, _)| ci + 1))
                                        .unwrap_or(0);
                                ws.insert_column_at(idx, id);
                                // A standalone drop keeps the dragged column's width.
                                if let Some(w) = old_width
                                        && let Some(col) = ws.columns.get_mut(idx)
                                {
                                        col.width = Some(w);
                                }
                        },
                }
                log::debug!("drag: window {id} reordered on {source_device}");
                self.update_focus_view(id);
                self.reflow();
                self.sync_focus_to_os();
        }

        /// Niri toggle-window-floating (tiling -> float): the window leaves
        /// the layout at its current position and is placed freely.
        fn float_window(
                &mut self,
                id: isize,
        ) {
                let Some(device) = self.layout.remove_window(id) else {
                        return;
                };
                let ws_idx = self
                        .layout
                        .monitor(&device)
                        .map(|m| m.active_workspace_idx)
                        .unwrap_or(0);
                let hwnd = HWND(id as *mut _);
                let rect = crate::win::api::window_rect(hwnd).unwrap_or_else(|| {
                        // Fallback: centered 60% of the monitor's work area.
                        let mon = self
                                .monitors
                                .iter()
                                .find(|m| m.device == device)
                                .or_else(|| self.monitors.first());
                        match mon {
                                Some(m) => {
                                        let w = (m.work.right - m.work.left) as f64 * 0.6;
                                        let h = (m.work.bottom - m.work.top) as f64 * 0.6;
                                        let x = m.work.left as f64
                                                + ((m.work.right - m.work.left) as f64 - w) / 2.0;
                                        let y = m.work.top as f64
                                                + ((m.work.bottom - m.work.top) as f64 - h) / 2.0;
                                        (x, y, w, h)
                                },
                                None => (100.0, 100.0, 800.0, 600.0),
                        }
                });
                self.floating.insert(
                        id,
                        FloatState {
                                device: device.clone(),
                                workspace_idx: ws_idx,
                                x: rect.0,
                                y: rect.1,
                                w: rect.2,
                                h: rect.3,
                        },
                );
                // Close the gap in the tiling (reflow positions the float too).
                self.reflow();
        }

        /// Niri toggle-window-floating (float -> tiling): back into the
        /// layout at the focused position.
        fn unfloat_window(
                &mut self,
                id: isize,
                fs: FloatState,
        ) {
                self.floating.remove(&id);
                self.layout.add_window(&fs.device, id);
                self.reflow();
                self.sync_focus_to_os();
        }

        /// Run a command (niri spawn). The first token is the executable,
        /// the rest is passed as parameters.
        fn spawn(
                &self,
                cmd: &str,
        ) {
                use windows::Win32::UI::Shell::ShellExecuteW;
                use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
                use windows::core::HSTRING;
                let (app, args) = match cmd.split_once(' ') {
                        Some((a, rest)) => (a, Some(rest.to_string())),
                        None => (cmd, None),
                };
                let app = HSTRING::from(app);
                let args = args.map(HSTRING::from);
                let params = match &args {
                        Some(a) => windows::core::PCWSTR(a.as_ptr()),
                        None => windows::core::PCWSTR::null(),
                };
                let r = unsafe { ShellExecuteW(None, None, &app, params, None, SW_SHOWNORMAL) };
                // ShellExecuteW returns a small value (<= 32) on failure.
                if r.0 as usize <= 32 {
                        log::warn!("spawn {cmd:?} failed (code {})", r.0 as isize);
                }
        }

        /// After a layout-driven focus change, tell the OS: raise the
        /// focused window.
        fn sync_focus_to_os(&mut self) {
                let Some(device) = self
                        .focused
                        .and_then(|h| {
                                let id = h.0 as isize;
                                self.layout
                                        .monitors
                                        .iter()
                                        .find_map(|m| m.workspace_of(id).map(|_| m.device.clone()))
                        })
                        .or_else(|| {
                                monitor::monitor_at_cursor(&self.monitors).map(|m| m.device.clone())
                        })
                else {
                        return;
                };
                let Some(focused_id) = self.layout.focused_id(&device) else {
                        return;
                };
                // While an overview is open on this device, the focused
                // window lives BEHIND its backdrop: force_set_foreground
                // would make it raise itself above the backdrop
                // asynchronously (WM_ACTIVATE processing), which we cannot
                // reliably outrun by re-raising (the v2 z-order race, Bug 1).
                // Skip the OS sync entirely; the close path (tick_overviews)
                // syncs once the backdrop is destroyed and raising is safe
                // again.
                if self.overviews.contains_key(&device) {
                        log::debug!("sync_focus_to_os: deferred (overview open on {device})");
                        return;
                }
                let hwnd = windows::Win32::Foundation::HWND(focused_id as *mut _);
                let ok = crate::win::api::force_set_foreground(hwnd);
                log::debug!("sync_focus_to_os: force_set_foreground({focused_id}) -> {ok}");
                // SetForegroundWindow/BringWindowToTop put the window above
                // the desktop bar (yasb/zebar) and above an open overview's
                // backdrop; bring both back up (niri: the layer-shell bar
                // renders above tiled windows; the overview covers them).
                if ok {
                        self.raise_overview_hosts();
                        placement::raise_bars(&self.monitors);
                }
        }

        /// Retarget the transport springs from the current layout and push
        /// one frame. Paused while the user is dragging/resizing a window,
        /// and while management is suspended (screen transition).
        fn reflow(&mut self) {
                if self.interacting_window.is_some() || self.suspended {
                        return;
                }
                let params = self.params.clone();
                // The transport owns all tiled-window geometry: retarget its
                // per-tile / view-offset / workspace-switch springs to the
                // layout's settled targets. Monitors with an open overview are
                // handled by `refresh_overview` instead (thumbnails, not real
                // windows), so their tiles are excluded from the applied frame.
                self.transport.sync(&self.layout, &self.monitors, &params);

                // Floating windows: placed freely on their workspace, above
                // the tiles. Not part of the transport (they never tile);
                // positioned directly here.
                let floats: Vec<(isize, f64, f64, f64, f64, bool)> = self
                        .floating
                        .iter()
                        .map(|(id, fs)| {
                                let visible = self
                    .layout
                    .monitor(&fs.device)
                    .map(|ml| ml.active_workspace_idx == fs.workspace_idx)
                    .unwrap_or(false)
                    // Floats are hidden while an overview is open on
                    // their monitor (they would sit full-size over
                    // the scaled workspaces).
                    && !self.overviews.contains_key(&fs.device);
                                (*id, fs.x, fs.y, fs.w, fs.h, visible)
                        })
                        .collect();
                for (id, x, y, w, h, visible) in floats {
                        let hwnd = HWND(id as *mut _);
                        if !visible {
                                // Register BEFORE hiding: if the winevent hook fires
                                // re-entrantly, hidden_by_us must already contain the
                                // id so we don't unmanage our own hide.
                                self.hidden_by_us.insert(id);
                                if crate::win::api::is_alive(hwnd) {
                                        placement::set_shown(hwnd, false);
                                }
                        } else {
                                if crate::win::api::is_alive(hwnd) {
                                        placement::set_shown(hwnd, true);
                                        placement::raise(hwnd);
                                        placement::apply_geometry(&[geometry::TileRect {
                                                id,
                                                x: x.round() as i32,
                                                y: y.round() as i32,
                                                w: w.round().max(1.0) as i32,
                                                h: h.round().max(1.0) as i32,
                                        }]);
                                }
                                self.hidden_by_us.remove(&id);
                        }
                }

                // Monitors with an open overview own their tiled geometry:
                // rebuild their participant set (windows may have opened or
                // closed since) and park one frame at the current camera/zoom.
                let overview_devices: Vec<String> = self.overviews.keys().cloned().collect();
                for device in overview_devices {
                        self.refresh_overview(&device);
                }

                // Sample + apply the transport frame (also kicks the first
                // frame so first paint isn't delayed).
                self.apply_transport_frame();
                self.update_focus_border();
        }

        /// Sample the transport, apply moves/visibility to real windows,
        /// and handle the windowed-fullscreen override (cover the whole
        /// monitor + stay on top). Visibility is diffed against `shown` so
        /// `set_shown` only fires when a window actually appears/disappears
        /// (during a workspace switch, workspaces enter/leave the sweep).
        fn apply_transport_frame(&mut self) {
                let skip: std::collections::HashSet<String> =
                        self.overviews.keys().cloned().collect();
                let frame = self.transport.frame(&self.layout, &self.monitors, &skip);

                // Fullscreen ids per monitor (active workspace only).
                let mut fs_ids: std::collections::HashSet<isize> = std::collections::HashSet::new();
                let mut fs_rect: std::collections::HashMap<isize, (i32, i32, i32, i32)> =
                        std::collections::HashMap::new();
                for mon in &self.monitors {
                        if let Some(ml) = self.layout.monitor(&mon.device)
                                && let Some(id) = ml.active_workspace().fullscreen_id
                        {
                                fs_ids.insert(id);
                                fs_rect.insert(
                                        id,
                                        (
                                                mon.full.left,
                                                mon.full.top,
                                                mon.full.right - mon.full.left,
                                                mon.full.bottom - mon.full.top,
                                        ),
                                );
                        }
                }

                // Visibility. `frame.hide` is authoritative for windows on
                // non-rendered workspaces (of monitors this frame owns), and
                // `frame.show` for visible ones. `hidden_by_us` is the source
                // of truth for "we hid this" so the calls are idempotent (no
                // per-frame churn) and the first reflow hides inactive windows
                // even though `shown` starts empty.
                for &id in &frame.hide {
                        if !self.hidden_by_us.contains(&id) {
                                self.hidden_by_us.insert(id);
                                let hwnd = HWND(id as *mut _);
                                if crate::win::api::is_alive(hwnd) {
                                        placement::set_shown(hwnd, false);
                                }
                        }
                        self.shown.remove(&id);
                }
                for &id in &frame.show {
                        if !self.shown.contains(&id) {
                                self.hidden_by_us.remove(&id);
                                let hwnd = HWND(id as *mut _);
                                if crate::win::api::is_alive(hwnd) {
                                        placement::set_shown(hwnd, true);
                                }
                                self.shown.insert(id);
                        }
                }

                // Apply moves, overriding fullscreen windows to cover the
                // whole monitor.
                let mut moves: Vec<geometry::TileRect> = Vec::with_capacity(frame.moves.len());
                for r in frame.moves {
                        if let Some(&(x, y, w, h)) = fs_rect.get(&r.id) {
                                moves.push(geometry::TileRect {
                                        id: r.id,
                                        x,
                                        y,
                                        w,
                                        h,
                                });
                        } else {
                                moves.push(r);
                        }
                }
                placement::apply_geometry(&moves);

                // Fullscreen windows must cover their tile siblings, and the
                // bars stay above them (niri: layer-shell top).
                if !fs_ids.is_empty() {
                        for id in &fs_ids {
                                let hwnd = HWND(*id as *mut _);
                                if crate::win::api::is_alive(hwnd) {
                                        placement::raise(hwnd);
                                }
                        }
                        placement::raise_bars(&self.monitors);
                }
        }

        /// Advance all animations one frame and push geometry to windows.
        /// Called from the timer tick on the main thread.
        fn tick_animations(&mut self) {
                self.handle_tray_events();
                if !self.suspended && self.interacting_window.is_none() {
                        let anim = self.transport.animating();
                        // Apply while moving, plus the single frame that lands
                        // exactly on settle (was_animating), then go quiet so we
                        // don't SetWindowPos at rest.
                        if anim || self.was_animating {
                                self.apply_transport_frame();
                                // The sliding windows may cross the bar zone during
                                // a workspace switch; keep the bars on top.
                                if anim {
                                        placement::raise_bars(&self.monitors);
                                }
                        }
                        self.was_animating = anim;
                }
                self.tick_overviews();
                // The ring follows the focused window's live rect, so it must
                // move with every animation frame.
                self.update_focus_border();
        }

        /// Drain tray-menu events and perform the requested actions.
        /// Called from the animation timer (menu events arrive on a
        /// global channel; polling it every frame is cheap and keeps
        /// everything on the main thread).
        fn handle_tray_events(&mut self) {
                let Some(tray) = self.tray.as_mut() else {
                        return;
                };
                let actions = tray.poll_events();
                for action in actions {
                        log::info!("tray action: {action:?}");
                        match action {
                                crate::win::tray::TrayAction::OpenConfig => {
                                        self.open_config_file();
                                },
                                crate::win::tray::TrayAction::ReloadConfig => {
                                        // Force a reload even if the mtime poll already
                                        // saw today's change (or the file was touched back
                                        // to an old mtime).
                                        self.config_mtime = None;
                                        self.maybe_reload_config();
                                },
                                crate::win::tray::TrayAction::ToggleAutostart => {
                                        let now = !crate::win::tray::autostart_enabled();
                                        if crate::win::tray::set_autostart(now) {
                                                if let Some(tray) = self.tray.as_ref() {
                                                        tray.set_autostart_checked(now);
                                                }
                                                log::info!(
                                                        "autostart {}",
                                                        if now { "on" } else { "off" }
                                                );
                                        } else {
                                                log::warn!("failed to toggle autostart");
                                        }
                                },
                                crate::win::tray::TrayAction::Quit => unsafe { PostQuitMessage(0) },
                        }
                }
        }

        /// Open the config file with the system default editor.
        fn open_config_file(&self) {
                use windows::Win32::UI::Shell::ShellExecuteW;
                use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
                let path = config::config_path();
                if !path.exists() {
                        // Give the editor something to open and the user something
                        // to edit: seed the file with the example config.
                        if let Some(parent) = path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&path, include_str!("../config.example.kdl"));
                }
                let path = windows::core::HSTRING::from(path.as_os_str());
                let r = unsafe { ShellExecuteW(None, None, &path, None, None, SW_SHOWNORMAL) };
                // ShellExecuteW returns a small value (<= 32) on failure.
                if r.0 as usize <= 32 {
                        log::warn!("failed to open config file (code {})", r.0 as usize);
                }
        }

        /// Advance every overview one frame: while zoom or camera are in
        /// flight, update the thumbnail destination rects (DWM composites
        /// them — the real windows never move) and keep the backdrop
        /// above any window that raised itself. A fully closed and
        /// settled overview hands over: hide the non-active participants
        /// first (so nothing foreign flashes when the backdrop drops),
        /// then destroy the host — revealing real windows that have been
        /// at their settled tiles all along — and reflow (floats come
        /// back and any layout change lands). A fully open and settled
        /// overview needs no per-frame work — the state stays for
        /// scrolling and selection.
        fn tick_overviews(&mut self) {
                if self.overviews.is_empty() {
                        return;
                }
                let devices: Vec<String> = self.overviews.keys().cloned().collect();
                for device in devices {
                        let Some(ov) = self.overviews.get(&device) else {
                                continue;
                        };
                        // While the user drags one of the real windows (a float
                        // on another monitor can still be dragged), stop
                        // applying overview rects for a moment — fighting the
                        // drag would make the window rubber-band back every
                        // frame.
                        if self.interacting_window.is_some() {
                                continue;
                        }
                        if !ov.settled() {
                                let zoom = ov.zoom();
                                let cam = ov.render_cam();
                                log::debug!("overview[{device}]: zoom={zoom:.3} cam={cam:.2}");
                                let rects = ov.rects_at(zoom, cam);
                                if let Some(host) = self.overview_hosts.get_mut(&device) {
                                        host.update_rects(&rects);
                                }
                                // No per-frame re-raise: the host is opaque and
                                // topmost and the real windows don't move during the
                                // open/close zoom, so nothing can climb above it. DWM
                                // composites the thumbnails; the frame stays cheap.
                                continue;
                        }
                        if ov.opening() {
                                // Open and settled: maintain z order as a defense in
                                // depth — windows can raise themselves for reasons we
                                // don't control. We never force_set_foreground while
                                // the overview is open (see sync_focus_to_os /
                                // overview_focus), so this only closes whatever race
                                // remains, within one frame.
                                self.raise_overview_hosts();
                                placement::raise_bars(&self.monitors);
                                continue;
                        }
                        let ov = self.overviews.remove(&device).expect("checked above");
                        log::debug!("overview[{device}]: closed");
                        // Bring the real windows back BEFORE dropping the
                        // backdrop: hide the non-active participants (invisible —
                        // they are off-screen) and park the active ones at their
                        // settled tiles SYNCHRONOUSLY. At zoom 1 the thumbnails
                        // cover those tiles pixel-exactly (the inset converged to
                        // 0), so the host drop below is seamless.
                        let active_idx = self
                                .layout
                                .monitor(&device)
                                .map(|ml| ml.active_workspace_idx);
                        let mut back: Vec<crate::layout::geometry::TileRect> = Vec::new();
                        for (k, rs) in &ov.finals {
                                if Some(*k) == active_idx {
                                        back.extend(rs.iter().cloned());
                                        continue;
                                }
                                for r in rs {
                                        let hwnd = HWND(r.id as *mut _);
                                        // Register BEFORE hiding (hook race).
                                        self.hidden_by_us.insert(r.id);
                                        if crate::win::api::is_alive(hwnd) {
                                                placement::set_shown(hwnd, false);
                                        }
                                }
                        }
                        placement::apply_geometry_sync(&back);
                        // Dropping the host destroys the backdrop + thumbnails;
                        // the real windows are revealed exactly where the
                        // thumbnails ended (zoom 1 == settled tiles).
                        self.overview_hosts.remove(&device);
                        self.sync_overview_regions();
                        // The overview no longer owns this monitor's geometry.
                        // Snap the transport to the settled targets first so the
                        // real windows are revealed exactly where the thumbnails
                        // ended, instead of animating back from their stale
                        // pre-overview spring positions (the "move back on
                        // zoom-in complete" glitch).
                        let params = self.params.clone();
                        self.transport.sync(&self.layout, &self.monitors, &params);
                        self.transport.snap(&device);
                        self.reflow();
                        // Focus only moved internally while the overview was open
                        // (overview_focus / sync deferral); now that the backdrop
                        // is gone, push it to the OS — a foreground change can no
                        // longer pop anything above a destroyed backdrop.
                        self.sync_focus_to_os();
                }
        }
}

pub struct App {
        state: Rc<RefCell<AppState>>,
        /// Keeps the WinEvent hooks alive; dropping uninstalls them. Taken
        /// during shutdown so no tracking callbacks fire while we restore
        /// windows.
        _hooks: Option<EventHooks>,
        /// Hidden message-only window; receives marshaled key events.
        /// Kept alive for the process lifetime (field is deliberately
        /// unused after construction).
        #[allow(dead_code)]
        msg_window: MessageWindow,
}

impl App {
        /// Construct the application: adopt existing windows and install
        /// WinEvent hooks on this (main) thread.
        pub fn new() -> Result<Self, AppError> {
                let monitors = monitor::enumerate();
                for m in &monitors {
                        log::info!(
                                "monitor {}{}: {}x{} at ({}, {})",
                                m.device,
                                if m.is_primary { " (primary)" } else { "" },
                                m.width(),
                                m.height(),
                                m.origin().0,
                                m.origin().1
                        );
                }

                let mut windows = WindowRegistry::new();
                let count = windows.adopt_existing();
                log::info!("adopted {count} existing window(s) into the layout at startup");

                let cfg = config::load();
                let config_mtime = std::fs::metadata(config::config_path())
                        .and_then(|m| m.modified())
                        .ok();

                // Unified transport: per-tile move/resize, lockstep view
                // scroll and the workspace-switch camera each get their own
                // spring (niri tunes movement/resize separately; the view
                // offset and workspace switch have their own kinds too).
                let transport = Transport::new(
                        cfg.animations.movement_params(),
                        cfg.animations.resize_params(),
                        cfg.animations.view_offset_params(),
                        cfg.animations.workspace_switch_params(),
                );

                let overlay = crate::win::overlay::OverlayWindow::new();
                if overlay.is_none() {
                        log::warn!("failed to create the workspace overlay window");
                }
                let focus_border = crate::win::focus_border::FocusBorder::new();
                if focus_border.is_none() {
                        log::warn!("failed to create the focus ring window");
                }

                let state = Rc::new(RefCell::new(AppState {
                        windows,
                        monitors,
                        layout: Layout::new(),
                        params: cfg.layout.clone(),
                        config: cfg,
                        config_mtime,
                        focused: None,
                        interacting_window: None,
                        suspended: false,
                        interact_start_rect: None,
                        borderless: std::collections::HashSet::new(),
                        floating: std::collections::HashMap::new(),
                        hidden_by_us: std::collections::HashSet::new(),
                        overviews: std::collections::HashMap::new(),
                        overview_hosts: std::collections::HashMap::new(),
                        original_rects: std::collections::HashMap::new(),
                        overlay,
                        focus_border,
                        tray: {
                                let t = crate::win::tray::Tray::new();
                                if t.is_none() {
                                        log::warn!("failed to create the tray icon");
                                }
                                t
                        },
                        transport,
                        shown: std::collections::HashSet::new(),
                        was_animating: false,
                }));

                // Adopt existing windows through the same window-rule path as
                // newly opened ones (rules apply at startup too).
                {
                        let infos: Vec<WindowInfo> =
                                state.borrow().windows.iter().cloned().collect();
                        let mut s = state.borrow_mut();
                        for info in &infos {
                                s.original_rects.insert(info.id(), info.rect);
                                s.layout_add_window(info);
                        }
                }

                // Perform the initial tiling of everything we adopted.
                state.borrow_mut().reflow();

                // The hook handler shares state with the message loop via Rc.
                // Both live on the main thread, so no locking is needed.
                let handler_state = Rc::clone(&state);
                let hooks = EventHooks::install(move |event| {
                        handler_state.borrow_mut().handle_event(event);
                });

                let msg_window = MessageWindow::new().ok_or_else(|| {
                        AppError("failed to create the message window".to_string())
                })?;

                // Keyboard: install the LL hook targeting our message window,
                // and dispatch forwarded events to bound actions.
                {
                        let mod_key = state.borrow().config.mod_key.clone();
                        input::install(mod_key, msg_window.hwnd()).map_err(AppError::from)?;
                }
                // Mouse: wheel binds + optional focus-follows-mouse.
                {
                        let mouse_state = Rc::clone(&state);
                        msg_window.set_mouse_handler(move |ev| {
                                mouse_state.borrow_mut().handle_mouse_event(ev);
                        });
                }
                let key_state = Rc::clone(&state);
                msg_window.set_key_handler(move |ev| {
                        if !ev.pressed {
                                return;
                        }
                        // Escape exits overview anywhere (niri behavior); if it
                        // did, the press is consumed here.
                        {
                                let mut s = key_state.borrow_mut();
                                if ev.vk == 0x1B && s.exit_overviews() {
                                        return;
                                }
                        }
                        let mut s = key_state.borrow_mut();
                        let action = input::action_for(&s.config.binds, &ev);
                        if let Some(action) = action {
                                log::debug!("key action: {action:?}");
                                s.dispatch(action);
                        } else if ev.vk == 0x0D && s.exit_overviews() {
                                // Niri's hardcoded overview binds close on BOTH
                                // Escape and Return (input/mod.rs
                                // `hardcoded_overview_bind`); config binds take
                                // priority (checked above).
                                log::debug!("key action: Return -> close overview");
                        }
                });
                // Animation frames: tick the animator at ~60 Hz; config hot
                // reload polls the file once a second.
                {
                        let anim_state = Rc::clone(&state);
                        msg_window.start_hires_timer(TIMER_ANIM, ANIM_TIMER_MS, move || {
                                anim_state.borrow_mut().tick_animations();
                        });
                        let cfg_state = Rc::clone(&state);
                        msg_window.start_timer(TIMER_CONFIG, CONFIG_TIMER_MS, move || {
                                cfg_state.borrow_mut().maybe_reload_config();
                        });
                }
                // Display hotplug: WM_DISPLAYCHANGE arrives on the (top-level)
                // overlay window and is forwarded here.
                if let Some(ov) = state.borrow().overlay.as_ref() {
                        let dc_state = Rc::clone(&state);
                        ov.set_display_change_handler(move || {
                                dc_state.borrow_mut().on_display_change();
                        });
                        // Screen transition ended (cover hid itself): resume
                        // window management.
                        let tr_state = Rc::clone(&state);
                        ov.set_transition_end_handler(move || {
                                tr_state.borrow_mut().resume_management();
                        });
                }
                // Compile the combo table for hook-side swallowing.
                input::update_binds(&state.borrow().config.binds);

                // spawn-at-startup entries, in order. Spawned windows appear
                // after the hooks are live, so they are adopted and tiled like
                // any other window.
                for cmd in state.borrow().config.spawn_at_startup.clone() {
                        state.borrow().spawn(&cmd);
                }

                Ok(App {
                        state,
                        _hooks: Some(hooks),
                        msg_window,
                })
        }

        /// Run the Win32 message loop until a quit message arrives.
        ///
        /// WinEvent callbacks are dispatched by `GetMessageW`, so this loop is
        /// also what drives all window-tracking updates.
        pub fn run(&mut self) -> Result<(), AppError> {
                install_ctrl_c_quit();

                let mut msg = MSG::default();
                loop {
                        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                        if r.0 > 0 {
                                unsafe {
                                        let _ = TranslateMessage(&msg);
                                        DispatchMessageW(&msg);
                                }
                        } else if r.0 == 0 {
                                // WM_QUIT
                                break;
                        } else {
                                return Err(AppError(format!("GetMessageW failed: {}", r.0)));
                        }
                }

                let state = self.state.borrow();
                log::info!(
                        "message loop exited; was tracking {} window(s)",
                        state.windows.len()
                );
                drop(state);

                // --- graceful shutdown, in a deliberate order -------------
                //
                // 1. Input hooks first: no key/mouse event can mutate state
                //    (or get swallowed) while we tear down.
                input::uninstall();
                // 2. WinEvent hooks next: our own restore mutations must not
                //    trigger tracking callbacks that would fight the restore.
                if let Some(hooks) = self._hooks.take() {
                        drop(hooks);
                }
                // 3. Destroy our overlay windows before touching real ones —
                //    the workspace pill / focus ring / transition cover /
                //    overview backdrops would otherwise sit on top of the
                //    restored desktop. (Overview participants get re-shown by
                //    restore_all below.)
                {
                        let mut s = self.state.borrow_mut();
                        if let Some(ov) = s.overlay.take() {
                                drop(ov);
                        }
                        if let Some(fb) = s.focus_border.take() {
                                drop(fb);
                        }
                        s.overviews.clear();
                        s.overview_hosts.clear();
                }
                // 4. Leave the desktop as we found it: decorations, geometry
                //    and visibility of every window we ever managed.
                self.state.borrow_mut().restore_all();
                log::info!("shutdown complete; goodbye");
                Ok(())
        }
}

/// Ctrl+C / window-close: post WM_QUIT to our own thread so the loop
/// unwinds and hooks get dropped cleanly.
///
/// The console ctrl handler runs on a fresh OS thread, so the main
/// thread's id must be captured at install time — calling
/// GetCurrentThreadId() inside the handler would target the handler
/// thread, which has no message loop, and the quit message would
/// vanish.
static MAIN_THREAD_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn install_ctrl_c_quit() {
        unsafe extern "system" fn handler(_ctrl_type: u32) -> windows::core::BOOL {
                let thread_id = MAIN_THREAD_ID.load(std::sync::atomic::Ordering::Relaxed);
                unsafe {
                        let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                windows::core::BOOL(1)
        }
        unsafe {
                MAIN_THREAD_ID.store(
                        windows::Win32::System::Threading::GetCurrentThreadId(),
                        std::sync::atomic::Ordering::Relaxed,
                );
                if SetConsoleCtrlHandler(Some(handler), true).is_err() {
                        log::warn!("failed to install Ctrl+C handler");
                }
        }
}

#[cfg(test)]
mod tests {
        use super::*;

        fn instant() -> crate::anim::AnimParams {
                crate::anim::AnimParams {
                        kind: crate::anim::AnimKind::instant(),
                        slowdown: 1.0,
                }
        }

        fn tile(
                id: isize,
                x: i32,
                y: i32,
                w: i32,
                h: i32,
        ) -> geometry::TileRect {
                geometry::TileRect { id, x, y, w, h }
        }

        /// Monitor: full 1000x800 at (0,0). Workspace 0's settled tile is
        /// (100, 60, 400, 700), workspace 1's is (150, 60, 400, 700).
        fn overview() -> Overview {
                Overview {
                        progress: crate::anim::Val::to(1.0, instant()),
                        camera: crate::anim::Val::to(0.0, instant()),
                        finals: vec![
                                (0, vec![tile(1, 100, 60, 400, 700)]),
                                (1, vec![tile(2, 150, 60, 400, 700)]),
                        ],
                        full: (0.0, 0.0, 1000.0, 800.0),
                        zoom_target: 0.35,
                        view_shift: None,
                        sync_from_progress: None,
                }
        }

        #[test]
        fn overview_zoom_follows_progress() {
                let mut ov = overview();
                // progress 1 -> the target zoom.
                assert!((ov.zoom() - 0.35).abs() < 1e-9);
                // progress 0 -> zoom 1 (closed == settled view).
                ov.progress = crate::anim::Val::to(0.0, instant());
                assert!((ov.zoom() - 1.0).abs() < 1e-9);
        }

        #[test]
        fn overview_rects_continuous_at_zoom_one() {
                // At zoom 1 with the camera on workspace k, workspace k's
                // rects are EXACTLY the settled tiles — the close handover to
                // reflow() is pixel-exact, and opening starts from the
                // current view.
                let mut ov = overview();
                ov.camera = crate::anim::Val::to(1.0, instant());
                let rects = ov.rects_at(1.0, 1.0);
                assert_eq!(rects.len(), 2);
                let r2 = rects.iter().find(|r| r.id == 2).unwrap();
                assert_eq!((r2.x, r2.y, r2.w, r2.h), (150, 60, 400, 700));
                // Workspace 0 sits one stride above: stride = 800 + 80 = 880.
                let r1 = rects.iter().find(|r| r.id == 1).unwrap();
                assert_eq!((r1.x, r1.y, r1.w, r1.h), (100, 60 - 880, 400, 700));
        }

        #[test]
        fn overview_scales_and_centers() {
                // niri's workspaces_render_geo: the canvas (full monitor) is
                // scaled by zoom and centered; workspace k sits at
                // (k - camera) * stride with stride = scaled height + 10% gap.
                let ov = overview();
                let rects = ov.rects_at(0.35, 0.0);
                let r1 = rects.iter().find(|r| r.id == 1).unwrap();
                // off_x = (1000 - 350) / 2 = 325; x = 325 + 100 * 0.35 = 360.
                assert_eq!(r1.x, 360);
                // off_y = (800 - 280) / 2 = 260; y = 260 + 60 * 0.35 = 281.
                assert_eq!(r1.y, 281);
                assert_eq!(r1.w, 140);
                assert_eq!(r1.h, 245);
                // Camera on workspace 0: workspace 1 is one stride below
                // (stride = 280 + 800 * 0.1 * 0.35 = 308).
                let r2 = rects.iter().find(|r| r.id == 2).unwrap();
                assert_eq!(r2.y, 281 + 308);
        }

        #[test]
        fn overview_camera_offsets_all_workspaces() {
                // Camera between workspaces interpolates the strip offset;
                // every participant moves in lockstep (one animated value).
                let ov = overview();
                let rects = ov.rects_at(0.35, 0.5);
                let r1 = rects.iter().find(|r| r.id == 1).unwrap();
                let r2 = rects.iter().find(|r| r.id == 2).unwrap();
                assert_eq!(r1.y, 281 - 154); // (0 - 0.5) * 308
                assert_eq!(r2.y, 281 + 154); // (1 - 0.5) * 308
                // x does not depend on the camera.
                assert_eq!(r1.x, 360);
        }

        #[test]
        fn overview_sync_correction_continuous_at_ends() {
                // niri's workspace_render_idx correction:
                //   render = to + (cam - to) * (from_zoom / cur_zoom).
                // At the start (cam == from, zoom == from_zoom) it must equal
                // `from`; at the end (cam == to) it must equal `to` —
                // regardless of the zoom ratio.
                // A 10s linear easing keeps values mid-flight for sampling.
                let slow = || crate::anim::AnimParams {
                        kind: crate::anim::AnimKind::Easing {
                                duration: std::time::Duration::from_secs(10),
                                curve: crate::anim::Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                let mut ov = overview();
                // Close from progress 1: camera 0 -> 1 (ws 2 selected).
                ov.camera = crate::anim::Val::to(0.0, slow());
                ov.camera.retarget(1.0, slow());
                ov.sync_from_progress = Some(1.0);

                // Start: progress 1 -> zoom 0.35 == from_zoom, cam ~0.
                let start = ov.render_cam();
                assert!(start.abs() < 1e-3, "start == from: {start}");

                // End: camera finished at 1.0 -> correction collapses to `to`.
                ov.progress = crate::anim::Val::to(0.0, instant());
                ov.camera = crate::anim::Val::to(1.0, instant());
                let end = ov.render_cam();
                assert!((end - 1.0).abs() < 1e-9, "end == to: {end}");

                // Mid-flight with the zoom finished (progress 0 -> zoom 1):
                // render = to + (cam - to) * from_zoom — the tail of the
                // camera slide is compressed, never reversed or jumped.
                ov.camera = crate::anim::Val::to(0.4, slow());
                ov.camera.retarget(1.0, slow());
                let cam_now = ov.camera.value();
                let mid = ov.render_cam();
                let expect = 1.0 + (cam_now - 1.0) * 0.35;
                assert!((mid - expect).abs() < 1e-3, "mid: {mid} vs {expect}");
                assert!(mid > cam_now && mid < 1.0, "monotonic-ish: {mid}");

                // No sync: the raw camera value passes through.
                ov.sync_from_progress = None;
                assert!((ov.render_cam() - ov.camera.value()).abs() < 1e-3);
        }
}
