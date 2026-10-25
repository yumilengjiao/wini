//! Overview geometry and open/close animation state.
//!
//! Layout motion comes from `Transport`; this module composes zoom and the
//! workspace camera without Win32 calls. Host lifecycle stays in the app.

use crate::anim::{AnimParams, Val};
use crate::layout::geometry::TileRect;
use crate::layout::{MonitorLayout, WindowId};

#[derive(Debug, Clone)]
pub(super) struct Overview {
        pub(super) progress: Val,
        pub(super) camera: Val,
        /// Zoom-1 samples of transport tile/view motion, grouped by workspace.
        pub(super) finals: Vec<(usize, Vec<TileRect>)>,
        /// Work-area canvas in physical screen coordinates.
        pub(super) full: (f64, f64, f64, f64),
        pub(super) zoom_target: f64,
        /// Initial progress of a synchronized zoom/camera close.
        pub(super) sync_from_progress: Option<f64>,
        /// Final geometry was submitted under the cover on a previous tick.
        pub(super) handoff_ready: bool,
}

impl Overview {
        pub(super) fn zoom(&self) -> f64 {
                // Springs can overshoot, but a thumbnail must never grow past
                // the real window's final size while closing.
                (1.0 - self.progress.value() * (1.0 - self.zoom_target))
                        .clamp(self.zoom_target, 1.0)
        }

        pub(super) fn opening(&self) -> bool {
                self.progress.target() > 0.5
        }

        pub(super) fn rects_at(
                &self,
                zoom: f64,
                cam: f64,
        ) -> Vec<TileRect> {
                let (fx, fy, fw, fh) = self.full;
                let ws_h = fh * zoom;
                let stride = ws_h + fh * 0.1 * zoom;
                let off_x = fx + (fw - fw * zoom) / 2.0;
                let off_y = fy + (fh - ws_h) / 2.0;
                self.finals
                        .iter()
                        .flat_map(|(k, rects)| {
                                let dy = (*k as f64 - cam) * stride;
                                rects.iter().map(move |r| TileRect {
                                        id: r.id,
                                        x: (off_x + (r.x as f64 - fx) * zoom).round() as i32,
                                        y: (off_y + dy + (r.y as f64 - fy) * zoom).round() as i32,
                                        w: (r.w as f64 * zoom).round().max(1.0) as i32,
                                        h: (r.h as f64 * zoom).round().max(1.0) as i32,
                                })
                        })
                        .collect()
        }

        /// Niri's workspace_render_idx correction: compress the camera tail
        /// as zoom reaches 1. Continuous at both endpoints of a joint close.
        pub(super) fn render_cam(&self) -> f64 {
                let cam = self.camera.value();
                match self.sync_from_progress {
                        Some(from) => {
                                let from_zoom = (1.0 - from * (1.0 - self.zoom_target)).max(0.0001);
                                let to = self.camera.target();
                                to + (cam - to) * (from_zoom / self.zoom())
                        },
                        None => cam,
                }
        }

        pub(super) fn current_rects(&self) -> Vec<TileRect> {
                self.rects_at(self.zoom(), self.render_cam())
        }

        /// Scene rectangles are bottom-to-top. During stack/tab transitions
        /// siblings overlap; clicking must select the visible top surface.
        pub(super) fn hit_test(
                &self,
                x: i32,
                y: i32,
        ) -> Option<TileRect> {
                self.current_rects()
                        .into_iter()
                        .rev()
                        .find(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
        }

        /// Bake the close correction into camera coordinates before changing
        /// its target (or reopening). Keeping the old zoom basis with a new
        /// target jumps by (new_target - old_target) * (1 - scale).
        fn rendered_camera_motion(&self) -> (f64, f64) {
                let cam = self.camera.value();
                let velocity = self.camera.velocity();
                let Some(from) = self.sync_from_progress else {
                        return (cam, velocity);
                };
                let zoom = self.zoom();
                let from_zoom = (1.0 - from * (1.0 - self.zoom_target)).max(0.0001);
                let scale = from_zoom / zoom;
                let to = self.camera.target();
                let raw_zoom = 1.0 - self.progress.value() * (1.0 - self.zoom_target);
                let zoom_velocity = if raw_zoom > self.zoom_target && raw_zoom < 1.0 {
                        -(1.0 - self.zoom_target) * self.progress.velocity()
                } else {
                        0.0
                };
                (
                        to + (cam - to) * scale,
                        scale * (velocity - (cam - to) * zoom_velocity / zoom),
                )
        }

        pub(super) fn close_to(
                &mut self,
                target: f64,
                anim: AnimParams,
        ) {
                if !self.opening() && self.camera.target() == target {
                        return;
                }
                let (cam, velocity) = self.rendered_camera_motion();
                let progress = self.progress.value();
                self.handoff_ready = false;
                self.progress.retarget(0.0, anim);
                self.camera.retarget_from(cam, velocity, target, anim);
                self.sync_from_progress = Some(progress);
        }

        pub(super) fn reopen(
                &mut self,
                anim: AnimParams,
        ) {
                let (cam, velocity) = self.rendered_camera_motion();
                self.camera
                        .retarget_from(cam, velocity, self.camera.target(), anim);
                self.handoff_ready = false;
                self.progress.retarget(1.0, anim);
                self.sync_from_progress = None;
        }

        /// Numbered navigation creates workspaces on demand, exactly like
        /// normal mode. Relative navigation remains bounded to existing ones.
        pub(super) fn activate_workspace(
                &mut self,
                layout: &mut MonitorLayout,
                requested: usize,
                create: bool,
                anim: AnimParams,
        ) -> Option<WindowId> {
                let idx = if create {
                        requested
                } else {
                        requested.min(layout.workspaces.len().saturating_sub(1))
                };
                layout.switch_workspace(idx);
                self.handoff_ready = false;
                if self.opening() {
                        self.camera.retarget(idx as f64, anim);
                        self.sync_from_progress = None;
                } else {
                        // Navigation can interrupt the last prepare/reveal turn.
                        // Keep the close camera synchronized with zoom, rather
                        // than revealing one workspace with another selected.
                        self.close_to(idx as f64, anim);
                }
                layout.active_workspace().focused_id()
        }

        /// Structural moves and tab clicks also change the active workspace.
        pub(super) fn follow_workspace(
                &mut self,
                idx: usize,
                anim: AnimParams,
        ) {
                if (self.camera.target() - idx as f64).abs() < f64::EPSILON {
                        return;
                }
                self.handoff_ready = false;
                if self.opening() {
                        self.camera.retarget(idx as f64, anim);
                        self.sync_from_progress = None;
                } else {
                        self.close_to(idx as f64, anim);
                }
        }

        pub(super) fn settled(&self) -> bool {
                self.progress.finished() && self.camera.finished()
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::anim::{AnimKind, Curve};
        use std::time::Duration;

        fn instant() -> AnimParams {
                AnimParams {
                        kind: AnimKind::instant(),
                        slowdown: 1.0,
                }
        }

        fn tile(
                id: isize,
                x: i32,
                y: i32,
                w: i32,
                h: i32,
        ) -> TileRect {
                TileRect { id, x, y, w, h }
        }

        fn overview() -> Overview {
                Overview {
                        progress: Val::to(1.0, instant()),
                        camera: Val::to(0.0, instant()),
                        finals: vec![
                                (0, vec![tile(1, 100, 60, 400, 700)]),
                                (1, vec![tile(2, 150, 60, 400, 700)]),
                        ],
                        full: (0.0, 0.0, 1000.0, 800.0),
                        zoom_target: 0.35,
                        sync_from_progress: None,
                        handoff_ready: false,
                }
        }

        #[test]
        fn startup_overview_numbered_navigation_creates_workspace() {
                let mut layout = MonitorLayout::new("D".into());
                layout.add_window(1);
                assert_eq!(layout.workspaces.len(), 1);
                let mut ov = overview();
                ov.handoff_ready = true;
                assert_eq!(ov.activate_workspace(&mut layout, 1, true, instant()), None);
                assert_eq!(layout.workspaces.len(), 2);
                assert_eq!(layout.active_workspace_idx, 1);
                assert_eq!(layout.previous_workspace_idx, Some(0));
                assert_eq!(ov.camera.target(), 1.0);
                assert!(!ov.handoff_ready);
                assert!(ov.opening());
                assert_eq!(
                        ov.activate_workspace(&mut layout, 0, true, instant()),
                        Some(1)
                );
                assert_eq!(layout.previous_workspace_idx, Some(1));
                assert_eq!(ov.camera.target(), 0.0);
        }

        #[test]
        fn overview_numbered_navigation_matches_normal_mode_without_warmup() {
                let mut normal = MonitorLayout::new("D".into());
                normal.add_window(1);
                let mut preview = normal.clone();
                let mut ov = overview();
                for idx in [1, 0, 8, 1, 8, 0] {
                        normal.switch_workspace(idx);
                        ov.activate_workspace(&mut preview, idx, true, instant());
                        assert_eq!(preview.active_workspace_idx, normal.active_workspace_idx);
                        assert_eq!(
                                preview.previous_workspace_idx,
                                normal.previous_workspace_idx
                        );
                        assert_eq!(preview.workspaces.len(), normal.workspaces.len());
                        assert_eq!(preview.workspace_of(1), normal.workspace_of(1));
                }
        }

        #[test]
        fn overview_relative_navigation_does_not_grow_workspaces() {
                let mut layout = MonitorLayout::new("D".into());
                let mut ov = overview();
                ov.activate_workspace(&mut layout, 1, false, instant());
                assert_eq!(layout.workspaces.len(), 1);
                assert_eq!(ov.camera.target(), 0.0);
        }

        #[test]
        fn navigation_during_close_keeps_camera_zoom_and_selection_together() {
                let mut layout = MonitorLayout::new("D".into());
                layout.add_window(1);
                layout.add_window_to_workspace(2, 1);
                let mut ov = overview();
                for idx in [1, 0, 1, 2, 0] {
                        ov.close_to(layout.active_workspace_idx as f64, instant());
                        ov.handoff_ready = true;
                        let focus = ov.activate_workspace(&mut layout, idx, true, instant());
                        assert_eq!(ov.camera.target(), layout.active_workspace_idx as f64);
                        assert_eq!(focus, layout.active_workspace().focused_id());
                        assert!(!ov.opening());
                        assert!(!ov.handoff_ready);
                        assert!(ov.sync_from_progress.is_some());
                        ov.reopen(instant());
                }
        }

        #[test]
        fn moving_window_during_close_retargets_reveal_to_its_new_workspace() {
                let mut layout = MonitorLayout::new("D".into());
                layout.add_window(1);
                let mut ov = overview();
                for idx in [1, 0, 2, 0] {
                        ov.close_to(layout.active_workspace_idx as f64, instant());
                        ov.handoff_ready = true;
                        layout.move_focused_window_to_workspace(idx, true);
                        ov.follow_workspace(layout.active_workspace_idx, instant());
                        assert_eq!(layout.active_workspace().focused_id(), Some(1));
                        assert_eq!(ov.camera.target(), idx as f64);
                        assert!(!ov.handoff_ready);
                        assert!(!ov.opening());
                        assert!(ov.sync_from_progress.is_some());
                }
        }

        #[test]
        fn closing_retargets_and_invalidates_prepared_frame() {
                let mut ov = overview();
                ov.handoff_ready = true;
                ov.close_to(1.0, instant());
                assert!(!ov.handoff_ready);
                assert!(!ov.opening());
                assert!(ov.settled());
                assert_eq!(ov.camera.target(), 1.0);
                assert_eq!(
                        ov.current_rects().into_iter().find(|r| r.id == 2).unwrap(),
                        tile(2, 150, 60, 400, 700)
                );
        }

        #[test]
        fn rapid_reopening_cancels_prepared_reveal() {
                let mut ov = overview();
                ov.close_to(0.0, instant());
                ov.handoff_ready = true;
                ov.reopen(instant());
                assert!(ov.opening());
                assert!(!ov.handoff_ready);
                assert!(ov.sync_from_progress.is_none());
                ov.close_to(1.0, instant());
                assert_eq!(ov.camera.target(), 1.0);
                assert!(!ov.handoff_ready);
        }

        #[test]
        fn overlap_hit_testing_selects_top_surface_during_display_transition() {
                let mut ov = overview();
                ov.finals = vec![(
                        0,
                        vec![tile(2, 100, 60, 400, 700), tile(1, 100, 60, 400, 700)],
                )];
                assert_eq!(ov.hit_test(400, 300).map(|r| r.id), Some(1));
                ov.close_to(0.0, instant());
                assert_eq!(ov.hit_test(200, 100).map(|r| r.id), Some(1));
                assert!(ov.hit_test(999, 799).is_none());
        }

        #[test]
        fn workspace_navigation_at_close_tail_does_not_jump() {
                let slow = AnimParams {
                        kind: AnimKind::Easing {
                                duration: Duration::from_secs(60),
                                curve: Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                for elapsed in [30, 59, 60] {
                        let mut ov = overview();
                        ov.close_to(0.0, slow);
                        ov.progress.advance_for_test(Duration::from_secs(elapsed));
                        ov.camera.advance_for_test(Duration::from_secs(elapsed));
                        // Include the final prepared frame before its host is removed.
                        ov.handoff_ready = elapsed == 60;
                        let before = ov.current_rects();
                        ov.close_to(1.0, slow);
                        let after = ov.current_rects();
                        for (a, b) in before.iter().zip(&after) {
                                assert!(
                                        (a.y - b.y).abs() <= 1,
                                        "close tail {elapsed}s: {a:?} -> {b:?}"
                                );
                        }
                        assert!(!ov.handoff_ready);
                        assert_eq!(ov.camera.target(), 1.0);
                }
        }

        #[test]
        fn retargeting_and_reopening_a_moving_close_preserves_rendered_camera() {
                for kind in [
                        AnimKind::Spring(crate::anim::SpringParams::niri_default()),
                        AnimKind::Easing {
                                duration: Duration::from_secs(1),
                                curve: Curve::Linear,
                        },
                ] {
                        let params = AnimParams {
                                kind,
                                slowdown: 100.0,
                        };
                        let mut ov = overview();
                        ov.close_to(1.0, params);
                        for target in [0.0, 2.0, 1.0] {
                                ov.progress.advance_for_test(Duration::from_secs(8));
                                ov.camera.advance_for_test(Duration::from_secs(8));
                                let before = ov.current_rects();
                                ov.close_to(target, params);
                                let after = ov.current_rects();
                                for (a, b) in before.iter().zip(&after) {
                                        assert!((a.y - b.y).abs() <= 1, "retarget: {a:?} -> {b:?}");
                                }
                        }
                        ov.progress.advance_for_test(Duration::from_secs(8));
                        ov.camera.advance_for_test(Duration::from_secs(8));
                        let before = ov.current_rects();
                        ov.reopen(params);
                        let after = ov.current_rects();
                        for (a, b) in before.iter().zip(&after) {
                                assert!((a.y - b.y).abs() <= 1, "reopen: {a:?} -> {b:?}");
                        }
                        assert!(ov.sync_from_progress.is_none());
                }
        }

        #[test]
        fn overview_zoom_follows_progress() {
                let mut ov = overview();
                assert!((ov.zoom() - 0.35).abs() < 1e-9);
                ov.progress = Val::to(0.0, instant());
                assert!((ov.zoom() - 1.0).abs() < 1e-9);
        }

        #[test]
        fn overview_rects_continuous_at_zoom_one() {
                let ov = overview();
                let rects = ov.rects_at(1.0, 1.0);
                assert_eq!(rects.len(), 2);
                assert_eq!(
                        *rects.iter().find(|r| r.id == 2).unwrap(),
                        tile(2, 150, 60, 400, 700)
                );
                assert_eq!(
                        *rects.iter().find(|r| r.id == 1).unwrap(),
                        tile(1, 100, 60 - 880, 400, 700)
                );
        }

        #[test]
        fn overview_scales_and_centers() {
                let rects = overview().rects_at(0.35, 0.0);
                assert_eq!(
                        *rects.iter().find(|r| r.id == 1).unwrap(),
                        tile(1, 360, 281, 140, 245)
                );
                assert_eq!(rects.iter().find(|r| r.id == 2).unwrap().y, 281 + 308);
        }

        #[test]
        fn overview_camera_offsets_all_workspaces() {
                let rects = overview().rects_at(0.35, 0.5);
                assert_eq!(rects.iter().find(|r| r.id == 1).unwrap().y, 281 - 154);
                assert_eq!(rects.iter().find(|r| r.id == 2).unwrap().y, 281 + 154);
                assert_eq!(rects[0].x, 360);
        }

        #[test]
        fn overview_sync_correction_continuous_at_ends() {
                let slow = AnimParams {
                        kind: AnimKind::Easing {
                                duration: Duration::from_secs(10),
                                curve: Curve::Linear,
                        },
                        slowdown: 1.0,
                };
                let mut ov = overview();
                ov.camera = Val::to(0.0, slow);
                ov.camera.retarget(1.0, slow);
                ov.sync_from_progress = Some(1.0);
                assert!(ov.render_cam().abs() < 1e-3);
                ov.progress = Val::to(0.0, instant());
                ov.camera = Val::to(1.0, instant());
                assert!((ov.render_cam() - 1.0).abs() < 1e-9);
                ov.camera = Val::to(0.4, slow);
                ov.camera.retarget(1.0, slow);
                let cam = ov.camera.value();
                let mid = ov.render_cam();
                assert!((mid - (1.0 + (cam - 1.0) * 0.35)).abs() < 1e-3);
                assert!(mid > cam && mid < 1.0);
                ov.sync_from_progress = None;
                assert!((ov.render_cam() - ov.camera.value()).abs() < 1e-3);
        }
}
