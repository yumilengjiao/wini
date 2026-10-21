//! Unified animated transport: the single per-frame geometry pipeline.
//!
//! This is wini's adaptation of niri's render model. Instead of three
//! separate systems (per-window reflow springs, a frozen workspace-switch
//! "slide", and the overview), every window's on-screen rectangle is
//! composed each frame from THREE independent animated values, so they
//! blend seamlessly under interruption:
//!
//! - **per-tile column-space rect** (`tiles`): a spring per window over its
//!   position/size *in column space* (before the horizontal scroll). Only
//!   moves on structural change — column reorder, tile resize, open.
//! - **per-workspace view offset** (`view`): one spring for the whole
//!   workspace's horizontal scroll, so all columns move in lockstep (the
//!   niri feel), rather than each window springing on its own.
//! - **per-monitor workspace camera** (`wsp`): one spring over the
//!   fractional workspace index on the vertical strip (workspace switch).
//!
//! Screen position of a tile on workspace `k`:
//! ```text
//! screen_x = area.left + colspace_x - view[k]
//! screen_y = area.top  + colspace_y + (k - wsp) * stride
//! ```
//! The layout stays the pure source of truth (structure + settled
//! targets); the transport only animates toward those targets. `sync`
//! retargets the springs after any layout change; `frame` samples them.
//!
//! The overview composes its zoom/workspace camera on top of these same
//! tile and view springs. It skips applying real-window geometry while
//! covered, but samples `workspace_rects` for DWM thumbnails each frame.

mod tab_state;
use tab_state::TabSelection;

use std::collections::{HashMap, HashSet};

use crate::anim::{AnimParams, AnimatedRect, Val};
use crate::layout::Layout;
use crate::layout::WindowId;
use crate::layout::geometry::{self, LayoutParams, TileRect};
use crate::win::monitor::Monitor;

/// One frame of work for the app to apply to real windows.
#[derive(Debug, Default)]
pub struct Frame {
        /// Windows to move/resize this frame (absolute screen rects).
        pub moves: Vec<TileRect>,
        /// Windows that must be visible this frame.
        pub show: Vec<WindowId>,
        /// Windows that must be hidden this frame (inactive workspaces).
        pub hide: Vec<WindowId>,
}

/// Per-monitor animated transport state.
#[derive(Debug)]
struct MonitorT {
        /// Workspace-switch camera: fractional workspace index on the strip.
        wsp: Val,
        /// Per-workspace horizontal view offset (column-space x at the view's
        /// left edge). Only the active workspace's target changes on scroll.
        view: HashMap<usize, Val>,
        /// Per-window column-space rect spring (reorder / resize / open).
        tiles: HashMap<WindowId, AnimatedRect>,
        /// One workspace unit on the vertical strip (full monitor height × 1.1,
        /// niri's `workspace_size_with_gap`).
        stride: f64,
        /// Target active workspace index (what `wsp` springs toward).
        active: usize,
        /// Screen origin of the tiling area (work area inset by struts).
        /// Column-space rects are composed relative to it.
        origin: (i32, i32),
        /// Last selected tab keyed by the column's first tile. Seed a newly
        /// selected tab from the old rendered rect, not its stale stack slot.
        /// Stable invisible resize margins, measured on entry to edge mode.
        edge_insets: HashMap<WindowId, crate::win::frame::FrameInsets>,
        /// Snapshot of each tab group, including its membership. Structural
        /// merges must not be mistaken for an ordinary tab focus switch.
        selected_tabs: HashMap<WindowId, TabSelection>,
}

/// The unified transport for every monitor.
#[derive(Debug)]
pub struct Transport {
        monitors: HashMap<String, MonitorT>,
        /// Per-tile move (position) spring params.
        movement: AnimParams,
        /// Per-tile resize (size) spring params.
        resize: AnimParams,
        /// Horizontal view-offset spring params.
        view: AnimParams,
        /// Workspace-switch spring params.
        switch: AnimParams,
}

impl Transport {
        pub fn new(
                movement: AnimParams,
                resize: AnimParams,
                view: AnimParams,
                switch: AnimParams,
        ) -> Self {
                Transport {
                        monitors: HashMap::new(),
                        movement,
                        resize,
                        view,
                        switch,
                }
        }

        /// Update the spring parameters (config hot reload). In-flight springs
        /// keep their current params; new retargets pick these up.
        pub fn set_params(
                &mut self,
                movement: AnimParams,
                resize: AnimParams,
                view: AnimParams,
                switch: AnimParams,
        ) {
                self.movement = movement;
                self.resize = resize;
                self.view = view;
                self.switch = switch;
        }

        /// Drop all state for a monitor that went away.
        pub fn remove_monitor(
                &mut self,
                device: &str,
        ) {
                self.monitors.remove(device);
        }

        /// Forget a window (closed / unmanaged).
        pub fn remove_window(
                &mut self,
                id: WindowId,
        ) {
                for m in self.monitors.values_mut() {
                        m.tiles.remove(&id);
                }
        }

        /// Retarget every spring from the layout's settled targets. Call after
        /// any structural or focus change. `params` is the layout tuning;
        /// `monitors` supplies each output's work area and full height.
        pub fn sync(
                &mut self,
                layout: &Layout,
                monitors: &[Monitor],
                params: &LayoutParams,
        ) {
                // Retire monitors no longer present.
                let live: HashSet<&str> = monitors.iter().map(|m| m.device.as_str()).collect();
                self.monitors.retain(|d, _| live.contains(d.as_str()));

                for mon in monitors {
                        let Some(ml) = layout.monitor(&mon.device) else {
                                continue;
                        };
                        let work = (
                                mon.work.left as f64,
                                mon.work.top as f64,
                                (mon.work.right - mon.work.left) as f64,
                                (mon.work.bottom - mon.work.top) as f64,
                        );
                        let area = params.tiling_area(work);
                        let origin = (area.0.round() as i32, area.1.round() as i32);
                        let view_width = area.2.max(1.0);
                        let stride = (mon.full.bottom - mon.full.top) as f64 * 1.1;

                        let entry = self.monitors.entry(mon.device.clone()).or_insert_with(|| {
                                MonitorT {
                                        wsp: Val::to(ml.active_workspace_idx as f64, self.switch),
                                        view: HashMap::new(),
                                        tiles: HashMap::new(),
                                        stride,
                                        active: ml.active_workspace_idx,
                                        origin,
                                        edge_insets: HashMap::new(),
                                        selected_tabs: HashMap::new(),
                                }
                        });
                        entry.stride = stride;
                        entry.origin = origin;
                        entry.active = ml.active_workspace_idx;
                        entry.wsp
                                .retarget(ml.active_workspace_idx as f64, self.switch);

                        // Which windows still exist on this monitor (for pruning).
                        let mut live_ids: HashSet<WindowId> = HashSet::new();
                        let mut live_ws: HashSet<usize> = HashSet::new();
                        let mut next_tabs = HashMap::new();
                        let mut edge_ids = HashSet::new();

                        for (k, ws) in ml.workspaces.iter().enumerate() {
                                if ws.is_empty() {
                                        continue;
                                }
                                live_ws.insert(k);
                                let target_vp = geometry::target_view_pos(ws, params, view_width);
                                let vslot = entry
                                        .view
                                        .entry(k)
                                        .or_insert_with(|| Val::to(target_vp, self.view));
                                vslot.retarget(target_vp, self.view);

                                for col in &ws.columns {
                                        if col.display
                                                != crate::layout::tabbed::ColumnDisplay::Tabbed
                                        {
                                                continue;
                                        }
                                        let Some(key) = col.tiles.first().map(|t| t.id) else {
                                                continue;
                                        };
                                        let Some(selection) = TabSelection::capture(col) else {
                                                continue;
                                        };
                                        let Some(active) = col.focused_id() else {
                                                continue;
                                        };
                                        if let Some(previous) =
                                                entry.selected_tabs.get(&key).and_then(|old| {
                                                        old.previous_for_focus_change(&selection)
                                                })
                                                && let Some(rect) =
                                                        entry.tiles.get(&previous).cloned()
                                        {
                                                entry.tiles.insert(active, rect);
                                        }
                                        next_tabs.insert(key, selection);
                                }
                                for mut r in geometry::workspace_colspace_rects_in_work_area(
                                        ws, params, work,
                                ) {
                                        if ws.find(r.id).is_some_and(|(ci, _)| {
                                                ws.columns[ci].is_edge_maximized
                                        }) {
                                                edge_ids.insert(r.id);
                                                let insets = *entry.edge_insets.entry(r.id).or_insert_with(|| crate::win::frame::frame_insets(
                                                        windows::Win32::Foundation::HWND(r.id as *mut _)));
                                                let (x, y, w, h) = insets
                                                        .outer_for_visible((r.x, r.y, r.w, r.h));
                                                r.x = x;
                                                r.y = y;
                                                r.w = w;
                                                r.h = h;
                                        }
                                        if ws.fullscreen_id == Some(r.id) {
                                                // Fullscreen must be part of the SAME scene for
                                                // overview/handoff/normal frames, not overridden
                                                // only after the host has disappeared.
                                                r.x = target_vp.round() as i32 + mon.full.left
                                                        - origin.0;
                                                r.y = mon.full.top - origin.1;
                                                r.w = mon.full.right - mon.full.left;
                                                r.h = mon.full.bottom - mon.full.top;
                                        }
                                        live_ids.insert(r.id);
                                        match entry.tiles.get_mut(&r.id) {
                                                Some(rect) => rect.retarget(
                                                        r.x as f64,
                                                        r.y as f64,
                                                        r.w as f64,
                                                        r.h as f64,
                                                        self.movement,
                                                        self.resize,
                                                ),
                                                None => {
                                                        entry.tiles.insert(
                                                                r.id,
                                                                AnimatedRect::new(
                                                                        r.x as f64,
                                                                        r.y as f64,
                                                                        r.w as f64,
                                                                        r.h as f64,
                                                                        self.movement,
                                                                        self.resize,
                                                                ),
                                                        );
                                                },
                                        }
                                }
                        }
                        entry.edge_insets.retain(|id, _| edge_ids.contains(id));
                        entry.selected_tabs = next_tabs;
                        entry.tiles.retain(|id, _| live_ids.contains(id));
                        entry.view.retain(|k, _| live_ws.contains(k));
                }
        }

        /// Sample layout motion without the workspace-switch camera. Overview
        /// applies its own zoom/camera to these SAME tile and view springs, so
        /// resizing or scrolling during a zoom never discards an in-flight move.
        pub fn workspace_rects(
                &self,
                layout: &Layout,
                device: &str,
        ) -> Vec<(usize, Vec<TileRect>)> {
                let Some(ml) = layout.monitor(device) else {
                        return Vec::new();
                };
                let Some(m) = self.monitors.get(device) else {
                        return Vec::new();
                };
                ml.workspaces
                        .iter()
                        .enumerate()
                        .filter_map(|(k, ws)| {
                                if ws.is_empty() {
                                        return None;
                                }
                                let vp = m.view.get(&k).map(|v| v.value()).unwrap_or(0.0);
                                let rects = ws
                                        .rendered_ids()
                                        .filter_map(|id| {
                                                let rect = m.tiles.get(&id)?;
                                                let (x, y, w, h) = rect.value();
                                                Some(TileRect {
                                                        id,
                                                        x: m.origin.0
                                                                + (x as f64 - vp).round() as i32,
                                                        y: m.origin.1 + y,
                                                        w,
                                                        h,
                                                })
                                        })
                                        .collect();
                                Some((k, rects))
                        })
                        .collect()
        }

        /// Motion which must finish before revealing overview's real windows.
        /// The overview owns the workspace camera; only layout/view matter here.
        pub fn layout_animating(
                &self,
                device: &str,
        ) -> bool {
                self.monitors.get(device).is_some_and(|m| {
                        m.view.values().any(|v| !v.finished())
                                || m.tiles.values().any(|r| !r.finished())
                })
        }

        /// Any monitor still animating (a spring in flight)?
        pub fn animating(&self) -> bool {
                self.monitors.values().any(|m| {
                        !m.wsp.finished()
                                || m.view.values().any(|v| !v.finished())
                                || m.tiles.values().any(|r| !r.finished())
                })
        }

        /// Is the workspace-switch camera still animating on this monitor?
        /// (Reindexing workspaces mid-switch would corrupt the camera target.)
        pub fn switching(
                &self,
                device: &str,
        ) -> bool {
                self.monitors.get(device).is_some_and(|m| !m.wsp.finished())
        }

        /// Snap every spring on `device` to its target (no animation). Used
        /// to hand geometry over instantly — e.g. when an overview closes,
        /// so the transport shows windows exactly at their settled tiles
        /// instead of animating from stale pre-overview positions (a
        /// visible "move back" right after the host drops).
        pub fn snap(
                &mut self,
                device: &str,
        ) {
                if let Some(m) = self.monitors.get_mut(device) {
                        m.wsp.settle();
                        for v in m.view.values_mut() {
                                v.settle();
                        }
                        for r in m.tiles.values_mut() {
                                r.settle();
                        }
                }
        }

        /// Sample every monitor's springs and produce the frame to apply.
        /// `skip` names monitors whose geometry the caller owns this frame
        /// (an open overview), which the transport must not touch.
        pub fn frame(
                &mut self,
                layout: &Layout,
                monitors: &[Monitor],
                skip: &HashSet<String>,
        ) -> Frame {
                let mut frame = Frame::default();
                for mon in monitors {
                        if skip.contains(&mon.device) {
                                continue;
                        }
                        let Some(ml) = layout.monitor(&mon.device) else {
                                continue;
                        };
                        let Some(m) = self.monitors.get(&mon.device) else {
                                continue;
                        };
                        let wsp = m.wsp.value();
                        let (ax, ay) = m.origin;

                        // A workspace renders when it is within the switch sweep (its
                        // strip slot overlaps the view) or it is the active target.
                        for (k, ws) in ml.workspaces.iter().enumerate() {
                                if ws.is_empty() {
                                        continue;
                                }
                                let dy = (k as f64 - wsp) * m.stride;
                                let visible = (k as f64 - wsp).abs() < 1.0 || k == m.active;
                                if !visible {
                                        frame.hide.extend(ws.window_ids());
                                        continue;
                                }
                                let vp = m.view.get(&k).map(|v| v.value()).unwrap_or(0.0);
                                let visible_ids: HashSet<_> = ws.rendered_ids().collect();
                                frame.hide.extend(ws
                                        .window_ids()
                                        .filter(|id| !visible_ids.contains(id)));
                                for id in ws.rendered_ids() {
                                        let Some(rect) = m.tiles.get(&id) else {
                                                continue;
                                        };
                                        let (cx, cy, cw, ch) = rect.value();
                                        frame.moves.push(TileRect {
                                                id,
                                                x: ax + (cx as f64 - vp).round() as i32,
                                                y: ay + cy + dy.round() as i32,
                                                w: cw,
                                                h: ch,
                                        });
                                        frame.show.push(id);
                                }
                        }
                }
                frame
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::anim::{AnimKind, AnimParams};
        use crate::layout::ColumnWidth;
        use crate::layout::geometry::{CenterFocused, LayoutParams};
        use windows::Win32::Foundation::RECT;
        use windows::Win32::Graphics::Gdi::HMONITOR;

        fn instant() -> AnimParams {
                AnimParams {
                        kind: AnimKind::instant(),
                        slowdown: 1.0,
                }
        }

        fn transport() -> Transport {
                Transport::new(instant(), instant(), instant(), instant())
        }

        fn params() -> LayoutParams {
                LayoutParams {
                        gaps: 0.0,
                        default_column_width: ColumnWidth::Proportion(0.5),
                        preset_column_widths: Vec::new(),
                        preset_window_heights: Vec::new(),
                        tab_indicator: crate::layout::tabbed::TabIndicatorConfig::default(),
                        center_focused_column: CenterFocused::Never,
                        struts: geometry::Struts::default(),
                }
        }

        fn monitor(device: &str) -> Monitor {
                Monitor {
                        handle: HMONITOR(std::ptr::null_mut()),
                        full: RECT {
                                left: 0,
                                top: 0,
                                right: 1000,
                                bottom: 800,
                        },
                        work: RECT {
                                left: 0,
                                top: 0,
                                right: 1000,
                                bottom: 800,
                        },
                        device: device.to_string(),
                        is_primary: true,
                }
        }

        #[test]
        fn single_window_fills_active_workspace() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                let mons = vec![monitor("D")];
                let p = params();
                let mut t = transport();
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(frame.moves.len(), 1);
                let r = frame.moves[0];
                assert_eq!(r.id, 1);
                // Half-width (0.5 * 1000 = 500) column, left-aligned (view clamps
                // to the left with 0 gaps), full height.
                assert_eq!((r.x, r.y, r.w, r.h), (0, 0, 500, 800));
        }

        #[test]
        fn inactive_workspace_hidden_after_switch_settles() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                // Put window 2 on workspace 1, switch back to 0.
                if let Some(ml) = layout.monitor_mut("D") {
                        ml.add_window_to_workspace(2, 1);
                        ml.switch_workspace(0);
                }
                let mons = vec![monitor("D")];
                let p = params();
                let mut t = transport();
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                // Active workspace 0 shows window 1; workspace 1 (window 2) hidden
                // once the (instant) switch settled.
                assert!(frame.show.contains(&1));
                assert!(frame.hide.contains(&2));
                assert!(!frame.moves.iter().any(|r| r.id == 2));
        }

        #[test]
        fn window_removed_is_forgotten() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                let mons = vec![monitor("D")];
                let p = params();
                let mut t = transport();
                t.sync(&layout, &mons, &p);
                layout.remove_window(2);
                t.remove_window(2);
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert!(!frame.moves.iter().any(|r| r.id == 2));
                assert!(frame.moves.iter().any(|r| r.id == 1));
        }

        #[test]
        fn struts_match_settled_geometry_on_offset_monitor() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                let mut mon = monitor("D");
                mon.work = RECT {
                        left: -1000,
                        top: 48,
                        right: 0,
                        bottom: 800,
                };
                let mons = vec![mon];
                let mut p = params();
                p.struts = geometry::Struts {
                        left: 3.0,
                        right: 7.0,
                        top: 11.0,
                        bottom: 13.0,
                };
                let mut t = transport();
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                let area = p.tiling_area((-1000.0, 48.0, 1000.0, 752.0));
                let exact = geometry::compute_workspace_geometry(
                        layout.monitor("D").unwrap().active_workspace(),
                        &p,
                        area,
                );
                assert_eq!(frame.moves, exact);
                assert_eq!(frame.moves[0].x, -997);
                assert_eq!(frame.moves[0].y, 59);
                assert_eq!(frame.moves[0].h, 728);
        }

        #[test]
        fn overview_resize_keeps_shared_motion_until_handoff() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                let mons = vec![monitor("D")];
                let p = params();
                let slow = AnimParams {
                        kind: AnimKind::Easing {
                                duration: std::time::Duration::from_secs(60),
                                curve: crate::anim::Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                let mut t = Transport::new(slow, slow, slow, slow);
                t.sync(&layout, &mons, &p);
                let before = t.workspace_rects(&layout, "D")[0].1.clone();
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                assert!(ws.set_column_width(&crate::layout::SizeChange::Fixed(800.0), &p, 1000.0));
                t.sync(&layout, &mons, &p);
                let during = t.workspace_rects(&layout, "D")[0].1.clone();
                assert!(t.layout_animating("D"));
                assert!((during[0].w - before[0].w).abs() <= 1);
                assert!(during.iter().all(|r| r.w < 800));
                // Overview skips applying real-window geometry, not sampling
                // layout motion. Reflow must not snap or restart the springs.
                let skip = HashSet::from(["D".to_string()]);
                assert!(t.frame(&layout, &mons, &skip).moves.is_empty());
                t.sync(&layout, &mons, &p);
                assert!(t.layout_animating("D"));
                t.snap("D");
                assert!(!t.layout_animating("D"));
                let final_overview = t.workspace_rects(&layout, "D")[0].1.clone();
                assert_eq!(
                        final_overview,
                        t.frame(&layout, &mons, &HashSet::new()).moves
                );
                assert_eq!(final_overview.iter().find(|r| r.id == 2).unwrap().w, 800);
        }

        #[test]
        fn overview_focus_scroll_uses_same_view_spring_as_normal_mode() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                layout.add_window("D", 3);
                let mons = vec![monitor("D")];
                let p = params();
                let slow = AnimParams {
                        kind: AnimKind::Easing {
                                duration: std::time::Duration::from_secs(60),
                                curve: crate::anim::Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                let mut t = Transport::new(slow, slow, slow, slow);
                t.sync(&layout, &mons, &p);
                let before = t.workspace_rects(&layout, "D")[0].1.clone();
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.focus_window(1);
                geometry::refresh_view_offset(ws, &p, 1000.0, None);
                t.sync(&layout, &mons, &p);
                assert!(t.layout_animating("D"));
                let during = t.workspace_rects(&layout, "D")[0].1.clone();
                for (a, b) in before.iter().zip(&during) {
                        assert!(
                                (a.x - b.x).abs() <= 1,
                                "focus scroll jumped: {a:?} -> {b:?}"
                        );
                }
                t.snap("D");
                assert_eq!(
                        t.workspace_rects(&layout, "D")[0].1,
                        t.frame(&layout, &mons, &HashSet::new()).moves
                );
        }

        #[test]
        fn height_resize_is_shared_by_overview_and_normal_transport() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.consume_or_expel(crate::layout::DirH::Left);
                let mons = vec![monitor("D")];
                let p = params();
                let slow = AnimParams {
                        kind: AnimKind::Easing {
                                duration: std::time::Duration::from_secs(60),
                                curve: crate::anim::Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                let mut t = Transport::new(slow, slow, slow, slow);
                t.sync(&layout, &mons, &p);
                let before = t.workspace_rects(&layout, "D")[0].1.clone();
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.set_window_height(&crate::layout::SizeChange::Fixed(600.0), &p, 800.0);
                t.sync(&layout, &mons, &p);
                assert!(t.layout_animating("D"));
                let during = t.workspace_rects(&layout, "D")[0].1.clone();
                for (a, b) in before.iter().zip(during) {
                        assert!((a.h - b.h).abs() <= 1);
                }
                t.snap("D");
                let final_rects = t.workspace_rects(&layout, "D")[0].1.clone();
                assert_eq!(final_rects, t.frame(&layout, &mons, &HashSet::new()).moves);
                assert_eq!(final_rects.iter().find(|r| r.id == 2).unwrap().h, 600);
        }

        #[test]
        fn tabbed_visibility_is_identical_in_normal_and_overview_frames() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.consume_or_expel(crate::layout::DirH::Left);
                ws.toggle_column_tabbed_display();
                let p = params();
                let mons = vec![monitor("D")];
                let mut t = transport();
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(frame.show, vec![2]);
                assert_eq!(frame.hide, vec![1]);
                assert_eq!(frame.moves[0].h, 800);
                assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .focus_tile(crate::layout::DirV::Up);
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(frame.show, vec![1]);
                assert_eq!(frame.hide, vec![2]);
                assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .toggle_column_tabbed_display();
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(frame.show.len(), 2);
                assert!(frame.hide.is_empty());
        }

        #[test]
        fn consuming_into_tabbed_column_keeps_incoming_horizontal_animation() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                let p = params();
                let mons = vec![monitor("D")];
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.focus_column_edge(crate::layout::Edge::First);
                geometry::refresh_view_offset(ws, &p, 1000.0, None);
                ws.toggle_column_tabbed_display();
                let slow = AnimParams {
                        kind: AnimKind::Easing {
                                duration: std::time::Duration::from_secs(60),
                                curve: crate::anim::Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                let mut t = Transport::new(slow, slow, slow, slow);
                t.sync(&layout, &mons, &p);
                let before = t.workspace_rects(&layout, "D")[0]
                        .1
                        .iter()
                        .copied()
                        .find(|r| r.id == 2)
                        .unwrap();
                assert!(layout
                        .monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .consume_window_into_column());
                t.sync(&layout, &mons, &p);
                let during = t.workspace_rects(&layout, "D")[0].1[0];
                assert_eq!(during.id, 2);
                assert!(
                        (during.x - before.x).abs() <= 1,
                        "merge snapped: {before:?} -> {during:?}"
                );
                assert!(during.x > 400);
                assert!(t.layout_animating("D"));
                t.snap("D");
                let final_rect = t.workspace_rects(&layout, "D")[0].1[0];
                assert_eq!(final_rect.x, 0);
                assert_eq!(t.frame(&layout, &mons, &HashSet::new()).hide, vec![1]);
        }

        #[test]
        fn consuming_into_normal_stack_keeps_position_and_resize_animation() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                let p = params();
                let mons = vec![monitor("D")];
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.focus_column_edge(crate::layout::Edge::First);
                geometry::refresh_view_offset(ws, &p, 1000.0, None);
                let slow = AnimParams {
                        kind: AnimKind::Easing {
                                duration: std::time::Duration::from_secs(60),
                                curve: crate::anim::Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                let mut t = Transport::new(slow, slow, slow, slow);
                t.sync(&layout, &mons, &p);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .consume_window_into_column();
                t.sync(&layout, &mons, &p);
                let during = t.workspace_rects(&layout, "D")[0].1.clone();
                let incoming = during.iter().find(|r| r.id == 2).unwrap();
                assert!(incoming.x > 400 && incoming.h > 700);
                t.snap("D");
                let final_rects = t.workspace_rects(&layout, "D")[0].1.clone();
                assert_eq!(
                        final_rects
                                .iter()
                                .map(|r| (r.x, r.y, r.h))
                                .collect::<Vec<_>>(),
                        vec![(0, 0, 400), (0, 400, 400)]
                );
        }

        #[test]
        fn switching_tabs_mid_animation_does_not_jump_to_stale_stack_slot() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .consume_or_expel(crate::layout::DirH::Left);
                let p = params();
                let mons = vec![monitor("D")];
                let slow = AnimParams {
                        kind: AnimKind::Easing {
                                duration: std::time::Duration::from_secs(60),
                                curve: crate::anim::Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                let mut t = Transport::new(slow, slow, slow, slow);
                t.sync(&layout, &mons, &p);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .toggle_column_tabbed_display();
                t.sync(&layout, &mons, &p);
                let before = t.workspace_rects(&layout, "D")[0].1[0];
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .focus_tile(crate::layout::DirV::Up);
                t.sync(&layout, &mons, &p);
                let after = t.workspace_rects(&layout, "D")[0].1[0];
                assert!((before.y - after.y).abs() <= 1);
                assert!((before.h - after.h).abs() <= 1);
                assert_eq!(after.id, 1);
        }

        #[test]
        fn sizing_modes_share_exact_overview_handoff_and_normal_targets() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                let mut mon = monitor("D");
                mon.full = RECT {
                        left: -1000,
                        top: -100,
                        right: 0,
                        bottom: 700,
                };
                let mut p = params();
                p.struts = geometry::Struts {
                        left: 3.0,
                        right: 7.0,
                        top: 12.0,
                        bottom: 6.0,
                };
                for work in [
                        RECT {
                                left: -1000,
                                top: -52,
                                right: 0,
                                bottom: 700,
                        },
                        RECT {
                                left: -1000,
                                top: -100,
                                right: 0,
                                bottom: 652,
                        },
                        RECT {
                                left: -952,
                                top: -100,
                                right: 0,
                                bottom: 700,
                        },
                        RECT {
                                left: -1000,
                                top: -100,
                                right: -48,
                                bottom: 700,
                        },
                ] {
                        mon.work = work;
                        let mons = vec![mon.clone()];
                        let mut t = transport();
                        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                        ws.fullscreen_id = None;
                        ws.columns[0].is_edge_maximized = false;
                        ws.toggle_maximize_window_to_edges();
                        t.sync(&layout, &mons, &p);
                        let frame = t.frame(&layout, &mons, &HashSet::new());
                        let expected = (
                                work.left,
                                work.top,
                                work.right - work.left,
                                work.bottom - work.top,
                        );
                        let r = frame.moves[0];
                        assert_eq!((r.x, r.y, r.w, r.h), expected);
                        assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
                        let work_tuple = (
                                work.left as f64,
                                work.top as f64,
                                (work.right - work.left) as f64,
                                (work.bottom - work.top) as f64,
                        );
                        assert_eq!(
                                geometry::compute_workspace_geometry_in_work_area(
                                        layout.monitor("D").unwrap().active_workspace(),
                                        &p,
                                        work_tuple
                                ),
                                frame.moves
                        );
                        layout.monitor_mut("D")
                                .unwrap()
                                .active_workspace_mut()
                                .toggle_fullscreen();
                        t.sync(&layout, &mons, &p);
                        let full = t.frame(&layout, &mons, &HashSet::new());
                        let r = full.moves[0];
                        assert_eq!((r.x, r.y, r.w, r.h), (-1000, -100, 1000, 800));
                        assert_eq!(t.workspace_rects(&layout, "D")[0].1, full.moves);
                }
        }

        #[test]
        fn edge_mode_resize_remains_animated_under_overview_interruption() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                let mons = vec![monitor("D")];
                let p = params();
                let slow = AnimParams {
                        kind: AnimKind::Easing {
                                duration: std::time::Duration::from_secs(60),
                                curve: crate::anim::Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                let mut t = Transport::new(slow, slow, slow, slow);
                t.sync(&layout, &mons, &p);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .toggle_maximize_window_to_edges();
                t.sync(&layout, &mons, &p);
                assert!(t.layout_animating("D"));
                let r = t.workspace_rects(&layout, "D")[0].1[0];
                assert!(r.w < 1000);
                assert!((r.w - 500).abs() <= 1);
                t.snap("D");
                assert_eq!(
                        t.workspace_rects(&layout, "D")[0].1,
                        t.frame(&layout, &mons, &HashSet::new()).moves
                );
        }

        #[test]
        fn skip_monitor_is_untouched() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                let mons = vec![monitor("D")];
                let p = params();
                let mut t = transport();
                t.sync(&layout, &mons, &p);
                let mut skip = HashSet::new();
                skip.insert("D".to_string());
                let frame = t.frame(&layout, &mons, &skip);
                assert!(frame.moves.is_empty());
        }
}
