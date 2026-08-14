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

use crate::anim::Animator;
use crate::config::{self, Action, Config};
use crate::input::{self, KeyEvent, MouseKind};
use crate::layout::geometry::{self, LayoutParams};
use crate::layout::{DirH, DirV, Edge, Layout, SizeChange};
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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

/// An in-flight workspace-switch slide on one monitor (niri's
/// vertical canvas, camera model).
///
/// Niri does NOT animate each window separately: it animates ONE
/// value — the workspace render index (the "camera" position on the
/// vertical strip) — and every workspace's render offset falls out of
/// that single spring. That is what makes interruptions smooth: a
/// switch mid-slide retargets the camera from its CURRENT position
/// (with velocity carry-over), and all windows move in lockstep.
/// Animating per-window rects instead (what we did before) broke in
/// three visible ways: windows re-seeded mid-flight jumped, windows
/// of different workspaces overlapped in wrong Z order, and rapid
/// switching produced per-window spring state that diverged.
///
/// The camera is an index on the workspace strip: `0.0` = workspace 0
/// fills the view, `1.0` = workspace 1, etc. One strip unit is the
/// FULL monitor height plus a 10% gap (niri's
/// `workspace_size_with_gap`: view_size is the whole output — layer
/// bars float above it — plus `0.1 * height` gap). Tile geometry
/// stays work-area based as always; only the slide transport uses the
/// full-height strip, so windows slide clear of the bar zone instead
/// of stopping right at its edge (the "old workspace's bottom edge
/// visible under the bar" bug).
#[derive(Debug, Clone)]
struct Slide {
    /// Animated camera position (workspace index on the strip).
    camera: crate::anim::Val,
    /// Each participant's final on-work-area tile rects (target
    /// geometry; workspace idx -> rects). This doubles as the list
    /// of participants — everything between the previous and the
    /// target workspace, inclusive (niri sweeps intermediate
    /// workspaces across the view).
    finals: Vec<(usize, Vec<crate::layout::geometry::TileRect>)>,
    /// Strip stride: one workspace unit in pixels (full height × 1.1).
    /// The camera anchor is implicit: `finals` are work-area rects,
    /// so at camera position c workspace c's tiles sit at exactly
    /// their final (settled) positions (dy = 0) — the settle
    /// handover to reflow() is pixel-exact.
    stride: f64,
}

impl Slide {
    /// Current on-screen rects for every participant window at
    /// camera position `cam` (idx units on the strip).
    fn rects_at(&self, cam: f64) -> Vec<crate::layout::geometry::TileRect> {
        let mut out = Vec::new();
        for (k, rects) in &self.finals {
            let dy = ((*k as f64 - cam) * self.stride).round() as i32;
            for r in rects {
                out.push(crate::layout::geometry::TileRect {
                    id: r.id,
                    x: r.x,
                    y: r.y + dy,
                    w: r.w,
                    h: r.h,
                });
            }
        }
        out
    }

    /// Is the camera settled at its target?
    fn finished(&self) -> bool {
        self.camera.finished()
    }
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
    /// In-flight workspace-switch slides (niri's vertical canvas),
    /// keyed by monitor device. See [`Slide`] for the camera model.
    slides: std::collections::HashMap<String, Slide>,
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
    /// Window geometry animations.
    animator: Animator,
}

impl AppState {
    /// React to a decoded WinEvent; keeps the registry and layout in
    /// sync with reality. Geometry application arrives in a later
    /// module; for now the structure is maintained and logged.
    fn handle_event(&mut self, event: WinEvent) {
        match event {
            WinEvent::Shown(hwnd) | WinEvent::Uncloaked(hwnd) | WinEvent::MinimizeEnded(hwnd) => {
                if !self.windows.contains(hwnd) && crate::win::window::is_manageable(hwnd) {
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
            }
            WinEvent::Hidden(hwnd) | WinEvent::Cloaked(hwnd) | WinEvent::MinimizeStarted(hwnd) => {
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
                    self.layout.remove_window(id);
                    self.animator.remove(id);
                    self.restore_borders(id);
                    self.floating.remove(&id);
                    self.original_rects.remove(&id);
                    self.reflow();
                }
            }
            WinEvent::Destroyed(hwnd) => {
                let dead_id = hwnd.0 as isize;
                self.hidden_by_us.remove(&dead_id);
                if let Some(info) = self.windows.remove(hwnd) {
                    log::info!("window closed: \"{}\"", info.title);
                    let id = dead_id;
                    self.layout.remove_window(id);
                    self.animator.remove(id);
                    self.restore_borders(id);
                    self.floating.remove(&id);
                    self.original_rects.remove(&id);
                    self.reflow();
                }
            }
            WinEvent::MoveSizeStart(hwnd) => {
                if self.windows.contains(hwnd) {
                    log::debug!(
                        "user interaction started on window {id}",
                        id = hwnd.0 as isize
                    );
                    self.interacting_window = Some(hwnd);
                    self.interact_start_rect = crate::win::api::window_rect(hwnd);
                    // The ring would lag behind the dragged window;
                    // hide it until the drop resolves.
                    self.update_focus_border();
                }
            }
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
                        if let Some((x, y, w, h)) = crate::win::api::window_rect(hwnd) {
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
            }
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
                // processing WM_ACTIVATE (after our sync raise_bars):
                // re-raise the bars (yasb/zebar) on every foreground
                // change so they stay on top (niri: layer-shell top).
                placement::raise_bars(&self.monitors);
            }
        }
    }

    /// Place a newly tracked window into the layout, applying any
    /// matching window-rule (open-floating / open-on-workspace /
    /// open-maximized / open-fullscreen). Falls back to the active
    /// workspace of the monitor the window currently lives on.
    fn layout_add_window(&mut self, info: &WindowInfo) {
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
            }
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
                        log::debug!("window {id} enforces min size {min_w}x{min_h}");
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
    fn update_focus_view(&mut self, id: crate::layout::WindowId) {
        let params = self.params.clone();
        for m in &mut self.layout.monitors {
            if let Some(ws_idx) = m.workspace_of(id) {
                let Some(mon) = self.monitors.iter().find(|mon| mon.device == m.device) else {
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

    /// Exit overview mode on every monitor. Returns true if any was
    /// active (the caller consumes the key press in that case).
    fn exit_overviews(&mut self) -> bool {
        let mut any = false;
        for m in &mut self.layout.monitors {
            if m.active_workspace_mut().is_overview {
                m.active_workspace_mut().is_overview = false;
                any = true;
            }
        }
        if any {
            self.reflow();
        }
        any
    }

    /// A click (not a drag) on a window while overview is active:
    /// exit overview focused on the clicked window. Returns true if
    /// handled.
    fn click_in_overview(&mut self, hwnd: HWND) -> bool {
        let id = hwnd.0 as isize;
        let in_overview = self.layout.monitors.iter().any(|m| {
            m.active_workspace().is_overview && m.workspace_of(id) == Some(m.active_workspace_idx)
        });
        if !in_overview {
            return false;
        }
        self.exit_overviews();
        if self.layout.focus_window(id) {
            self.focused = Some(hwnd);
            self.update_focus_view(id);
        }
        self.reflow();
        self.sync_focus_to_os();
        true
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
    /// focus changes. In-flight animations snap to their targets so
    /// nothing is caught moving mid-pause.
    fn suspend_management(&mut self) {
        if self.suspended {
            return;
        }
        self.suspended = true;
        let finals = self.animator.finish_all();
        if !finals.is_empty() {
            let tiles: Vec<geometry::TileRect> = finals
                .into_iter()
                .map(|(id, x, y, w, h)| geometry::TileRect { id, x, y, w, h })
                .collect();
            placement::apply_geometry(&tiles);
        }
        // In-flight workspace slides snap to their endpoints too:
        // park every participant at its final tile, hide the
        // non-active workspaces, and drop the slide. Without this,
        // a slide freezes mid-transport (tick_slides is paused) and
        // the transition reveal would show a half-scrolled canvas.
        // `resume_management`'s reflow re-applies the layout after.
        let slides = std::mem::take(&mut self.slides);
        for (device, slide) in slides {
            let cam = slide.camera.target();
            placement::apply_geometry(&slide.rects_at(cam));
            let active_idx = self
                .layout
                .monitor(&device)
                .map(|ml| ml.active_workspace_idx);
            for (k, rs) in &slide.finals {
                if Some(*k) == active_idx {
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
            log::debug!(
                "slide[{device}]: snapped to endpoint on suspend (camera={cam:.3})"
            );
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
    fn handle_mouse_event(&mut self, ev: input::MouseEvent) {
        // Wheel binds and focus-follows-mouse are paused during a
        // screen transition (nothing should steal focus or move).
        if self.suspended {
            return;
        }
        match ev.kind {
            MouseKind::WheelV | MouseKind::WheelH => {
                let Some(vk) = ev.wheel_vk() else { return };
                let key_ev = KeyEvent {
                    vk,
                    pressed: true,
                    shift: ev.shift,
                    ctrl: ev.ctrl,
                    mod_held: ev.mod_held,
                };
                if let Some(action) = input::action_for(&self.config.binds, &key_ev) {
                    log::debug!("wheel action: {action:?}");
                    self.dispatch(action);
                }
            }
            MouseKind::Move => {
                if self.config.focus_follows_mouse {
                    self.focus_follows_mouse(ev.x, ev.y);
                }
            }
        }
    }

    /// Niri's focus-follows-mouse: hovering a managed window focuses
    /// it (layout focus + OS foreground). Skipped while the user is
    /// dragging/resizing, and only applies to windows that are actually
    /// visible (active workspace tiles and visible floats).
    fn focus_follows_mouse(&mut self, x: i32, y: i32) {
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

    /// Execute a bound action. The navigation subset works on the
    /// focused window's workspace.
    fn dispatch(&mut self, action: Action) {
        use Action::*;
        // While management is suspended (screen transition), the
        // screen is covered and windows must not move: only quit and
        // restarting the transition make sense.
        if self.suspended {
            match action {
                Quit => unsafe { PostQuitMessage(0) },
                DoScreenTransition => self.begin_screen_transition(),
                _ => {}
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
            match action {
                Action::ToggleOverview => {
                    if let Some(fs) = self.floating.get(&id) {
                        let device = fs.device.clone();
                        if let Some(ml) = self.layout.monitor_mut(&device)
                            && ml.active_workspace_mut().toggle_overview()
                        {
                            self.reflow();
                        }
                    }
                    return;
                }
                Action::CloseWindow => {
                    self.close_window(id);
                    return;
                }
                _ => {}
            }
            // Map a few tiling actions to float move/resize.
            let mut touched = false;
            if let Some(fs) = self.floating.get_mut(&id) {
                const STEP: f64 = 50.0;
                match action {
                    Action::MoveColumnLeft => {
                        fs.x -= STEP;
                        touched = true;
                    }
                    Action::MoveColumnRight => {
                        fs.x += STEP;
                        touched = true;
                    }
                    Action::MoveWindowUp => {
                        fs.y -= STEP;
                        touched = true;
                    }
                    Action::MoveWindowDown => {
                        fs.y += STEP;
                        touched = true;
                    }
                    Action::SetColumnWidth(spec) => {
                        if let Some(change) = SizeChange::parse(&spec) {
                            match change {
                                SizeChange::Delta(d) => fs.w = (fs.w + d).max(100.0),
                                SizeChange::Fixed(f) => fs.w = f.max(100.0),
                                SizeChange::Proportion(p) => {
                                    fs.w = (fs.w * p.clamp(0.05, 20.0)).max(100.0)
                                }
                            }
                            touched = true;
                        }
                    }
                    Action::SetWindowHeight(spec) => {
                        if let Some(change) = SizeChange::parse(&spec) {
                            match change {
                                SizeChange::Delta(d) => fs.h = (fs.h + d).max(100.0),
                                SizeChange::Fixed(f) => fs.h = f.max(100.0),
                                SizeChange::Proportion(p) => {
                                    fs.h = (fs.h * p.clamp(0.05, 20.0)).max(100.0)
                                }
                            }
                            touched = true;
                        }
                    }
                    _ => {}
                }
                // Keep at least 100 px of the float on its monitor.
                if touched && let Some(m) = self.monitors.iter().find(|m| m.device == fs.device) {
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
        // Windowed-fullscreen bookkeeping, applied after the layout
        // borrow ends (see below).
        let mut fullscreen_prev: Option<isize> = None;
        let mut fullscreen_now: Option<isize> = None;
        // Workspace we switched to (for the indicator overlay), if any.
        let mut ws_switch: Option<usize> = None;
        // Workspace we switched FROM (drives the slide direction).
        let mut ws_prev: Option<usize> = None;
        // Preset column widths for the bare set-column-width (cloned
        // out before the layout borrow below).
        let presets = self.params.preset_column_widths.clone();
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
                MoveWindowToColumnRight => changed = ws.move_tile_across(DirH::Right),
                ConsumeOrExpelWindowLeft => changed = ws.consume_or_expel(DirH::Left),
                ConsumeOrExpelWindowRight => changed = ws.consume_or_expel(DirH::Right),
                ConsumeWindowIntoColumn => changed = ws.consume_window_into_column(),
                ExpelWindowFromColumn => changed = ws.expel_window_from_column(),
                FocusColumnFirst => changed = ws.focus_column_edge(Edge::First),
                FocusColumnLast => changed = ws.focus_column_edge(Edge::Last),
                FocusColumnIndex(n) => {
                    changed = ws.focus_column_index(n.saturating_sub(1) as usize)
                }
                SetColumnWidth(spec) => {
                    match SizeChange::parse(&spec) {
                        Some(change) => changed = ws.set_column_width(&change),
                        // Bare `set-column-width;` cycles the presets
                        // (niri's preset-column-widths).
                        None => changed = ws.cycle_column_width(&presets),
                    }
                }
                SwitchPresetColumnWidth => {
                    changed = ws.cycle_column_width(&presets);
                }
                SetWindowHeight(spec) => {
                    if let Some(change) = SizeChange::parse(&spec) {
                        changed = ws.set_window_height(&change);
                    }
                }
                ToggleFullWidth => changed = ws.toggle_full_width(),
                MaximizeColumn => changed = ws.toggle_maximized(),
                ToggleWindowedFullscreen => {
                    fullscreen_prev = ws.fullscreen_id;
                    changed = ws.toggle_fullscreen();
                    fullscreen_now = ws.fullscreen_id;
                }
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
                }
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
                }
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
                }
                Spawn(cmd) => {
                    self.spawn(&cmd);
                }
                Quit => {
                    unsafe { PostQuitMessage(0) };
                }
                CloseWindow => {
                    if let Some(id) = focused_id {
                        self.close_window(id);
                    }
                }
                _ => {}
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
                // Workspace-switch slide (niri's vertical canvas): the
                // old workspace's windows animate out of view while
                // the new ones slide in from the opposite edge. Falls
                // back to the plain (instant) reflow when the
                // animation is `off` in the config, or while the user
                // is dragging a window.
                let slidden = ws_prev.is_some_and(|prev_idx| {
                    self.workspace_slide_reflow(&device, prev_idx, idx)
                });
                if !slidden {
                    self.reflow();
                }
            } else {
                if let Some(id) = focused_id {
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
            }
            Err(err) => {
                log::warn!("config reload failed ({err}); keeping the previous config");
            }
        }
    }

    /// Apply a (possibly new) configuration live: layout params, focus
    /// ring, animation tuning, binds and the Mod key.
    fn apply_config(&mut self, cfg: Config) {
        let mod_changed = cfg.mod_key != self.config.mod_key;
        self.animator.set_params(
            cfg.animations.movement_params(),
            cfg.animations.resize_params(),
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
        // it makes the ring trail the window. Use the animator's
        // in-flight rect (what we just sent) plus the visible-edge
        // insets measured from the live rects (insets don't change
        // mid-motion; both rects are stale by the same amount).
        let rect = if let Some((ax, ay, aw, ah)) = self.animator.in_flight_value(id)
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
    fn show_workspace_overlay(&self, device: &str, idx: usize) {
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
        let new_monitors = monitor::enumerate();
        let new_devices: Vec<String> = new_monitors.iter().map(|m| m.device.clone()).collect();

        let gone: Vec<String> = self
            .monitors
            .iter()
            .map(|m| m.device.clone())
            .filter(|d| !new_devices.contains(d))
            .collect();
        for device in gone {
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
                        log::info!("rehoming {} window(s) from {device} to {target}", ids.len());
                        for id in ids {
                            self.layout.add_window(&target, id);
                        }
                    }
                    None => log::warn!("no monitor left; {} window(s) left in place", ids.len()),
                }
            }
        }
        for m in &new_monitors {
            self.layout.add_monitor(&m.device);
        }
        self.monitors = new_monitors;
        self.reflow();
    }

    /// Restore decorations on a window we borderlessed. Safe to call
    /// for windows that no longer exist.
    fn restore_borders(&mut self, id: isize) {
        if self.borderless.remove(&id) {
            let hwnd = HWND(id as *mut _);
            if crate::win::api::is_alive(hwnd) {
                placement::set_borderless(hwnd, false);
            }
        }
    }

    /// Close a window politely (WM_CLOSE, lets apps prompt/save).
    fn close_window(&mut self, id: isize) {
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
    fn handle_tiled_drag_end(&mut self, hwnd: HWND) {
        let id = hwnd.0 as isize;

        // Tell real drags apart from clicks / tiny nudges: only a
        // meaningful position change triggers a reorder.
        let start = self.interact_start_rect.take();
        if let (Some(s), Some(e)) = (start, crate::win::api::window_rect(hwnd))
            && (s.0 - e.0).abs() < 10.0
            && (s.1 - e.1).abs() < 10.0
        {
            // A click while overview is active: exit, focused on the
            // clicked window.
            if self.click_in_overview(hwnd) {
                return;
            }
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
        let mut col_rects: Vec<Vec<geometry::TileRect>> = vec![Vec::new(); ws.columns.len()];
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
            }
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
            }
        }
        log::debug!("drag: window {id} reordered on {source_device}");
        self.update_focus_view(id);
        self.reflow();
        self.sync_focus_to_os();
    }

    /// Niri toggle-window-floating (tiling -> float): the window leaves
    /// the layout at its current position and is placed freely.
    fn float_window(&mut self, id: isize) {
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
                    let x = m.work.left as f64 + ((m.work.right - m.work.left) as f64 - w) / 2.0;
                    let y = m.work.top as f64 + ((m.work.bottom - m.work.top) as f64 - h) / 2.0;
                    (x, y, w, h)
                }
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
        self.animator.set_target(id, rect.0, rect.1, rect.2, rect.3);
        // Close the gap in the tiling.
        self.reflow();
    }

    /// Niri toggle-window-floating (float -> tiling): back into the
    /// layout at the focused position.
    fn unfloat_window(&mut self, id: isize, fs: FloatState) {
        self.floating.remove(&id);
        self.layout.add_window(&fs.device, id);
        self.reflow();
        self.sync_focus_to_os();
    }

    /// Run a command (niri spawn). The first token is the executable,
    /// the rest is passed as parameters.
    fn spawn(&self, cmd: &str) {
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
            .or_else(|| monitor::monitor_at_cursor(&self.monitors).map(|m| m.device.clone()))
        else {
            return;
        };
        let Some(focused_id) = self.layout.focused_id(&device) else {
            return;
        };
        let hwnd = windows::Win32::Foundation::HWND(focused_id as *mut _);
        let ok = crate::win::api::force_set_foreground(hwnd);
        log::debug!("sync_focus_to_os: force_set_foreground({focused_id}) -> {ok}");
        // SetForegroundWindow/BringWindowToTop put the window above
        // the desktop bar (yasb/zebar); bring bars back up (niri:
        // the layer-shell bar renders above tiled windows).
        if ok {
            placement::raise_bars(&self.monitors);
        }
    }

    /// Animate a workspace switch as niri does, with a CAMERA model:
    /// workspaces live on a vertical strip and we animate a single
    /// value — the camera (workspace render index) — from which every
    /// participant window's position derives each frame. This is
    /// niri's `workspace_render_idx()` design verbatim; see [`Slide`]
    /// for why per-window animation was replaced.
    ///
    /// One strip unit is the FULL monitor height + 10% gap (niri's
    /// `workspace_size_with_gap`): windows slide fully clear of the
    /// bar zone instead of parking at its edge, and intermediate
    /// workspaces sweep across the view like niri's. Bars (yasb/...)
    /// are re-raised above the sliding windows every frame.
    ///
    /// Returns false (and does nothing) when the animation is
    /// disabled (`off`), the monitor is unknown, or management is
    /// paused — the caller then does a plain reflow.
    fn workspace_slide_reflow(&mut self, device: &str, prev_idx: usize, idx: usize) -> bool {
        // While the user drags/resizes (or management is suspended)
        // reflow is paused; starting a slide then would strand windows
        // off screen. Plain switch instead.
        if self.interacting_window.is_some() || self.suspended {
            return false;
        }
        let ws_params = self.config.animations.workspace_switch_params();
        if ws_params.kind == crate::anim::AnimKind::instant() {
            return false;
        }
        let Some(mon) = self.monitors.iter().find(|m| m.device == device) else {
            return false;
        };
        let Some(ml) = self.layout.monitor(device) else {
            return false;
        };
        let params = self.params.clone();

        // niri's strip geometry: a workspace unit is the FULL output
        // height (bars are layer-shell overlays above the canvas; the
        // canvas itself is the whole monitor) plus a 10% gap. The
        // camera anchor is the WORK-area top: at camera position c,
        // workspace c's tiles sit at exactly their final (settled)
        // positions — dy = (k - c) * stride is 0 for the active
        // workspace at rest, so the settle handover to reflow() is
        // pixel-exact (no snap).
        let full_h = (mon.full.bottom - mon.full.top) as f64;
        let stride = full_h * 1.1;
        let area = (
            mon.work.left as f64,
            mon.work.top as f64,
            (mon.work.right - mon.work.left) as f64,
            (mon.work.bottom - mon.work.top) as f64,
        );

        // Every workspace between old and new (inclusive) takes part.
        let lo = prev_idx.min(idx);
        let hi = prev_idx.max(idx);
        let mut participants: Vec<usize> = Vec::new();
        let mut finals: Vec<(usize, Vec<geometry::TileRect>)> = Vec::new();
        for k in lo..=hi {
            let Some(ws) = ml.workspaces.get(k) else { continue };
            participants.push(k);
            finals.push((k, geometry::compute_workspace_geometry(ws, &params, area)));
        }
        // Extend an interrupted slide on this monitor with any
        // participants it had that the new range misses (e.g. 3 -> 2
        // -> 1: ws3 must keep sliding out while ws2/ws1 slide in) —
        // with their CURRENT camera-relative rects as finals so they
        // keep moving coherently instead of freezing mid-view.
        if let Some(old) = self.slides.get(device) {
            let cam_now = old.camera.value();
            let mut old_rects = old.rects_at(cam_now);
            let covered: std::collections::HashSet<isize> = finals
                .iter()
                .flat_map(|(_, rs)| rs.iter().map(|r| r.id))
                .collect();
            for (k, rects) in &old.finals {
                if (lo..=hi).contains(k) {
                    // Workspaces the new slide covers get fresh
                    // finals above; drop their rects from the old set
                    // (they may have moved since).
                    old_rects.retain(|r| !covered.contains(&r.id));
                    continue;
                }
                participants.push(*k);
                finals.push((*k, rects.clone()));
            }
            // Park the still-relevant old participants exactly where
            // they are right now, so the camera handover is seamless.
            let keep: std::collections::HashSet<isize> =
                old_rects.iter().map(|r| r.id).collect();
            for (k, rects) in finals.iter_mut() {
                if !participants.contains(k) || (lo..=hi).contains(k) {
                    continue;
                }
                rects.retain(|r| !keep.contains(&r.id));
                for r in old_rects.iter().filter(|r| keep.contains(&r.id)) {
                    if rects.iter().all(|x| x.id != r.id) && ml
                        .workspaces
                        .get(*k)
                        .is_some_and(|ws| ws.window_ids().any(|id| id == r.id))
                    {
                        rects.push(*r);
                    }
                }
            }
        }

        // The camera: starts at the OLD view (or wherever an
        // interrupted slide currently is — the retarget below then
        // continues from that position AND velocity, which is what
        // makes rapid switching smooth instead of stuttery) and
        // targets the NEW workspace index.
        let cam_from = match self.slides.get(device) {
            Some(old) => old.camera.value(),
            None => prev_idx as f64,
        };
        let mut camera = crate::anim::Val::to(cam_from, ws_params);
        camera.retarget(idx as f64, ws_params);

        // Build the slide record, then materialize one frame at the
        // camera's current value so windows jump to their
        // canvas-relative positions immediately (no one-frame flash
        // of the new workspace's final tiles).
        let slide = Slide {
            camera,
            finals,
            stride,
        };
        placement::apply_geometry(&slide.rects_at(cam_from));

        // Show every participant; hide everything else on this
        // monitor's inactive workspaces. No `raise` anywhere: all
        // participants keep their existing relative Z order through
        // the slide (raising the entering workspace's windows would
        // visibly cover the ones still sliding out — the rapid
        // 3 -> 2 -> 1 bug). The bars get re-raised instead.
        let participant_ids: std::collections::HashSet<isize> = slide
            .finals
            .iter()
            .flat_map(|(_, rs)| rs.iter().map(|r| r.id))
            .collect();
        for r in slide.finals.iter().flat_map(|(_, rs)| rs.iter()) {
            let hwnd = HWND(r.id as *mut _);
            if crate::win::api::is_alive(hwnd) {
                self.hidden_by_us.remove(&r.id);
                placement::set_shown(hwnd, true);
            }
        }
        for k in 0..ml.workspaces.len() {
            if k == idx {
                continue;
            }
            for id in ml.workspaces[k].window_ids() {
                if participant_ids.contains(&id) {
                    continue;
                }
                let hwnd = HWND(id as *mut _);
                // Register BEFORE hiding (hook race; see reflow).
                self.hidden_by_us.insert(id);
                if crate::win::api::is_alive(hwnd) {
                    placement::set_shown(hwnd, false);
                }
                self.animator.remove(id);
            }
        }
        // The sliding windows crossed the bar zone: put bars
        // (yasb/zebar/...) back on top of them.
        placement::raise_bars(&self.monitors);

        self.slides.insert(device.to_string(), slide);
        true
    }

    /// Recompute geometry for every monitor's active workspace and push
    /// it to the real windows (animated). Paused while the user is
    /// dragging or resizing a window, and while management is
    /// suspended (screen transition).
    fn reflow(&mut self) {
        if self.interacting_window.is_some() || self.suspended {
            return;
        }
        let params = self.params.clone();
        let mut targets: Vec<(isize, f64, f64, f64, f64)> = Vec::new();
        let mut raise_ids: Vec<isize> = Vec::new();
        // Windows on inactive workspaces are not rendered (niri
        // semantics); windows on the active one must be visible.
        let mut hide_ids: Vec<isize> = Vec::new();
        let mut show_ids: Vec<isize> = Vec::new();
        for mon in &self.monitors {
            let Some(ml) = self.layout.monitor(&mon.device) else {
                continue;
            };
            // A slide in flight on this monitor owns its windows'
            // geometry: applying final (settled) tile rects mid-slide
            // would snap the sliding windows to their end positions.
            // Floats of the active workspace are still managed below
            // (floats don't take part in the slide), but the tiled
            // targets for THIS monitor are skipped until the slide
            // settles (which runs the skipped reflow via
            // finish_slide).
            let mut sliding_ids: std::collections::HashSet<isize> =
                std::collections::HashSet::new();
            if let Some(slide) = self.slides.get(&mon.device) {
                sliding_ids.extend(
                    slide.finals.iter().flat_map(|(_, rs)| rs.iter().map(|r| r.id)),
                );
            }
            for (i, ws) in ml.workspaces.iter().enumerate() {
                if i != ml.active_workspace_idx {
                    // Windows still taking part in a slide on this
                    // monitor are hidden by finish_slide once the
                    // camera settles; don't hide them mid-slide.
                    hide_ids.extend(
                        ws.window_ids().filter(|id| !sliding_ids.contains(id)),
                    );
                }
            }
            let area = (
                mon.work.left as f64,
                mon.work.top as f64,
                (mon.work.right - mon.work.left) as f64,
                (mon.work.bottom - mon.work.top) as f64,
            );
            let fs_id = ml.active_workspace().fullscreen_id;
            for r in geometry::compute_workspace_geometry(ml.active_workspace(), &params, area) {
                let (x, y, w, h) = if Some(r.id) == fs_id {
                    // Windowed fullscreen: cover the whole monitor
                    // (taskbar included), not just the work area.
                    (
                        mon.full.left as f64,
                        mon.full.top as f64,
                        (mon.full.right - mon.full.left) as f64,
                        (mon.full.bottom - mon.full.top) as f64,
                    )
                } else {
                    (r.x as f64, r.y as f64, r.w as f64, r.h as f64)
                };
                if !sliding_ids.contains(&r.id) {
                    targets.push((r.id, x, y, w, h));
                }
                show_ids.push(r.id);
            }
            if let Some(fs_id) = fs_id {
                raise_ids.push(fs_id);
            }
        }
        // Floating windows: placed freely on their workspace, above
        // the tiles.
        let floats: Vec<(isize, f64, f64, f64, f64, bool)> = self
            .floating
            .iter()
            .map(|(id, fs)| {
                let visible = self
                    .layout
                    .monitor(&fs.device)
                    .map(|ml| ml.active_workspace_idx == fs.workspace_idx)
                    .unwrap_or(false);
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
                self.animator.remove(id);
            } else {
                if crate::win::api::is_alive(hwnd) {
                    placement::set_shown(hwnd, true);
                    placement::raise(hwnd);
                }
                self.hidden_by_us.remove(&id);
                self.animator.set_target(id, x, y, w, h);
            }
        }
        for id in hide_ids {
            let hwnd = HWND(id as *mut _);
            // Register BEFORE hiding (see the floats loop above).
            self.hidden_by_us.insert(id);
            if crate::win::api::is_alive(hwnd) {
                placement::set_shown(hwnd, false);
            }
            // No point animating a hidden window.
            self.animator.remove(id);
        }
        for id in &show_ids {
            let hwnd = HWND(*id as *mut _);
            if crate::win::api::is_alive(hwnd) {
                placement::set_shown(hwnd, true);
            }
            self.hidden_by_us.remove(id);
        }
        for (id, x, y, w, h) in targets {
            log::debug!("reflow target: {id} -> ({x:.0}, {y:.0}, {w:.0}x{h:.0})");
            self.animator.set_target(id, x, y, w, h);
        }
        // The fullscreen window must cover its tile siblings.
        for id in &raise_ids {
            let hwnd = HWND(*id as *mut _);
            if crate::win::api::is_alive(hwnd) {
                placement::raise(hwnd);
            }
        }
        // A windowed-fullscreen window covers the bar zone too; keep
        // bars (yasb/zebar/...) on top of it (niri: layer-shell top).
        if !raise_ids.is_empty() {
            placement::raise_bars(&self.monitors);
        }
        // Kick an immediate frame so first paint is not delayed.
        self.tick_animations();
        self.update_focus_border();
    }

    /// Advance all animations one frame and push geometry to windows.
    /// Called from the timer tick on the main thread. Never early-
    /// returns: at-rest rects whose target changed still owe their
    /// final frame (see `Animator::tick`).
    fn tick_animations(&mut self) {
        self.handle_tray_events();
        for (id, x, y, w, h) in self.animator.tick() {
            let rect = crate::layout::geometry::TileRect { id, x, y, w, h };
            placement::apply_geometry(&[rect]);
        }
        self.tick_slides();
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
                }
                crate::win::tray::TrayAction::ReloadConfig => {
                    // Force a reload even if the mtime poll already
                    // saw today's change (or the file was touched back
                    // to an old mtime).
                    self.config_mtime = None;
                    self.maybe_reload_config();
                }
                crate::win::tray::TrayAction::ToggleAutostart => {
                    let now = !crate::win::tray::autostart_enabled();
                    if crate::win::tray::set_autostart(now) {
                        if let Some(tray) = self.tray.as_ref() {
                            tray.set_autostart_checked(now);
                        }
                        log::info!("autostart {}", if now { "on" } else { "off" });
                    } else {
                        log::warn!("failed to toggle autostart");
                    }
                }
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
            let _ = std::fs::write(
                &path,
                include_str!("../config.example.kdl"),
            );
        }
        let path = windows::core::HSTRING::from(path.as_os_str());
        let r = unsafe { ShellExecuteW(None, None, &path, None, None, SW_SHOWNORMAL) };
        // ShellExecuteW returns a small value (<= 32) on failure.
        if r.0 as usize <= 32 {
            log::warn!("failed to open config file (code {})", r.0 as usize);
        }
    }

    /// Advance every in-flight workspace slide one frame: place all
    /// participant windows at their camera-derived rects, keep the
    /// bars above them, and settle finished slides (park the active
    /// workspace's windows at their final tiles, hide the rest, then
    /// run the reflow the slide had been holding back — floats and
    /// any layout changes that happened mid-slide land here).
    fn tick_slides(&mut self) {
        if self.slides.is_empty() {
            return;
        }
        let devices: Vec<String> = self.slides.keys().cloned().collect();
        for device in devices {
            let Some(slide) = self.slides.get(&device) else {
                continue;
            };
            let cam = slide.camera.value();
            let rects = slide.rects_at(cam);
            let done = slide.finished();
            log::debug!("slide[{device}]: camera={cam:.3} done={done}");
            placement::apply_geometry(&rects);
            // The sliding windows cross the bar zone every frame;
            // keep bars (yasb/zebar/...) above them.
            placement::raise_bars(&self.monitors);
            if done {
                let slide = self.slides.remove(&device).expect("checked above");
                log::debug!("slide[{device}]: settled, hiding non-active windows");
                // Park the final frame first (apply_geometry above
                // used the settled camera value already). Hide the
                // non-active participants.
                let active_idx = self
                    .layout
                    .monitor(&device)
                    .map(|ml| ml.active_workspace_idx);
                for (k, rs) in &slide.finals {
                    if Some(*k) == active_idx {
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
                // The slide no longer owns this monitor's geometry:
                // apply whatever accumulated while it was in flight
                // (e.g. floats shown/hidden mid-slide).
                self.reflow();
            }
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

        // Animation tuning: `animations { off; }` maps every kind to
        // the instant easing, which makes every retarget land
        // immediately. Movement and resize get their own params
        // (niri tunes them separately).
        let animator = Animator::new(
            cfg.animations.movement_params(),
            cfg.animations.resize_params(),
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
            slides: std::collections::HashMap::new(),
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
            animator,
        }));

        // Adopt existing windows through the same window-rule path as
        // newly opened ones (rules apply at startup too).
        {
            let infos: Vec<WindowInfo> = state.borrow().windows.iter().cloned().collect();
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

        let msg_window = MessageWindow::new()
            .ok_or_else(|| AppError("failed to create the message window".to_string()))?;

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
        //    the workspace pill / focus ring / transition cover would
        //    otherwise sit on top of the restored desktop.
        {
            let mut s = self.state.borrow_mut();
            if let Some(ov) = s.overlay.take() {
                drop(ov);
            }
            if let Some(fb) = s.focus_border.take() {
                drop(fb);
            }
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
