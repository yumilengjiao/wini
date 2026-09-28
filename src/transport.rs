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
//! The overview (zoomed DWM thumbnails) is layered on top by the app and
//! is not part of this transport — real windows can't be moved every
//! frame without app reflow/flicker, so the overview parks them and draws
//! thumbnails instead.

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
                        let area = (
                                mon.work.left as f64,
                                mon.work.top as f64,
                                (mon.work.right - mon.work.left) as f64,
                                (mon.work.bottom - mon.work.top) as f64,
                        );
                        let view_width = area.2.max(1.0);
                        let stride = (mon.full.bottom - mon.full.top) as f64 * 1.1;

                        let entry = self.monitors.entry(mon.device.clone()).or_insert_with(|| {
                                MonitorT {
                                        wsp: Val::to(ml.active_workspace_idx as f64, self.switch),
                                        view: HashMap::new(),
                                        tiles: HashMap::new(),
                                        stride,
                                        active: ml.active_workspace_idx,
                                }
                        });
                        entry.stride = stride;
                        entry.active = ml.active_workspace_idx;
                        entry.wsp
                                .retarget(ml.active_workspace_idx as f64, self.switch);

                        // Which windows still exist on this monitor (for pruning).
                        let mut live_ids: HashSet<WindowId> = HashSet::new();
                        let mut live_ws: HashSet<usize> = HashSet::new();

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

                                for r in geometry::workspace_colspace_rects(ws, params, area) {
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
                        entry.tiles.retain(|id, _| live_ids.contains(id));
                        entry.view.retain(|k, _| live_ws.contains(k));
                }
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

        /// Current on-screen rect of `id`, sampling the springs right now.
        /// Used by the focus ring (which must track the animated window).
        pub fn window_rect(
                &self,
                layout: &Layout,
                monitors: &[Monitor],
                id: WindowId,
        ) -> Option<(i32, i32, i32, i32)> {
                for mon in monitors {
                        let ml = layout.monitor(&mon.device)?;
                        let Some(k) = ml.workspace_of(id) else {
                                continue;
                        };
                        let m = self.monitors.get(&mon.device)?;
                        let rect = m.tiles.get(&id)?;
                        let (cx, cy, cw, ch) = rect.value();
                        let vp = m.view.get(&k).map(|v| v.value()).unwrap_or(0.0);
                        let dy = (k as f64 - m.wsp.value()) * m.stride;
                        let sx = mon.work.left + (cx as f64 - vp).round() as i32;
                        let sy = mon.work.top + cy + dy.round() as i32;
                        return Some((sx, sy, cw, ch));
                }
                None
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
                        let ax = mon.work.left;
                        let ay = mon.work.top;

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
                                for id in ws.window_ids() {
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
                        center_focused_column: CenterFocused::Never,
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
