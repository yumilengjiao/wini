//! Z-order for structural tiled-window animations. Repair both the immediate
//! focus handoff and delayed foreground activation without activating tiles or
//! briefly lifting them over floating/fullscreen windows or overview hosts.

use super::AppState;
use crate::win::{api, layering};
use windows::Win32::Foundation::HWND;

impl AppState {
        pub(super) fn raise_tiled_motion(
                &self,
                ids: &[isize],
        ) {
                self.raise_tiled_motion_with_visibility(ids, &self.hidden_by_us);
        }

        /// Stage Z-order while newly appearing HWNDs are still hidden, so no
        /// full-size replacement surface is exposed over the selected tile.
        pub(super) fn raise_tiled_motion_before_show(
                &self,
                ids: &[isize],
                show: &[isize],
        ) {
                let mut hidden = self.hidden_by_us.clone();
                for id in show {
                        hidden.remove(id);
                }
                self.raise_tiled_motion_with_visibility(ids, &hidden);
        }

        fn raise_tiled_motion_with_visibility(
                &self,
                ids: &[isize],
                hidden: &std::collections::HashSet<isize>,
        ) {
                let moving = motion_order(&self.layout, &self.overviews, hidden, ids);
                if moving.is_empty() {
                        return;
                }
                // Z-order is global, not per monitor. Protect all visible
                // overlays, including an overview on a different output.
                let mut protected: Vec<_> = self.floating.keys().copied().collect();
                protected.extend(self
                        .layout
                        .monitors
                        .iter()
                        .flat_map(|ml| ml.workspaces.iter().filter_map(|ws| ws.fullscreen_id)));
                protected.extend(self.overview_hosts.values().map(|host| host.hwnd_addr()));
                protected.extend(crate::win::placement::desktop_bars(&self.monitors)
                        .into_iter()
                        .map(|hwnd| hwnd.0 as isize));
                let protected: Vec<_> = protected
                        .into_iter()
                        .filter(|id| !self.hidden_by_us.contains(id))
                        .map(|id| HWND(id as *mut _))
                        .collect();
                // OS activation may have put the source tile over the floats
                // too. Put it back in the tiled band before the moving tiles;
                // do not move a focused float/fullscreen/overview participant.
                let focus = self.focused.map(|hwnd| hwnd.0 as isize);
                let order =
                        focus_repair_order(&self.layout, &self.overviews, hidden, focus, &moving);
                let windows: Vec<_> = order
                        .into_iter()
                        .map(|id| HWND(id as *mut _))
                        .filter(|&hwnd| api::is_alive(hwnd))
                        .collect();
                layering::raise_below(&windows, &protected);
        }
}

/// A queued activation of the old selected tab must not undo a structural
/// handoff. Workspace-hidden windows are different: genuine activation may
/// still bring their workspace into view through the normal focus path.
pub(super) fn hidden_tab_activation(
        layout: &crate::layout::Layout,
        hidden: &std::collections::HashSet<isize>,
        id: isize,
) -> bool {
        hidden.contains(&id)
                && layout.monitors.iter().any(|ml| {
                        ml.workspace_of(id).is_some_and(|k| {
                                !ml.workspaces[k].rendered_ids().any(|visible| visible == id)
                        })
                })
}

fn motion_order(
        layout: &crate::layout::Layout,
        overviews: &std::collections::HashMap<String, super::Overview>,
        hidden: &std::collections::HashSet<isize>,
        ids: &[isize],
) -> Vec<isize> {
        ids.iter()
                .copied()
                .filter(|id| {
                        !hidden.contains(id)
                                && layout.monitors.iter().any(|ml| {
                                        !overviews.contains_key(&ml.device)
                                                && ml.workspaces.iter().any(|ws| {
                                                        ws.fullscreen_id != Some(*id)
                                                                && ws.window_ids().any(|v| v == *id)
                                                })
                                })
                })
                .collect()
}

fn focus_repair_order(
        layout: &crate::layout::Layout,
        overviews: &std::collections::HashMap<String, super::Overview>,
        hidden: &std::collections::HashSet<isize>,
        focus: Option<isize>,
        moving: &[isize],
) -> Vec<isize> {
        let mut order = motion_order(
                layout,
                overviews,
                hidden,
                &focus.into_iter().collect::<Vec<_>>(),
        );
        order.retain(|id| !moving.contains(id));
        order.extend_from_slice(moving);
        order
}

#[cfg(test)]
mod tests {
        use super::*;
        use std::collections::{HashMap, HashSet};

        #[test]
        fn queued_hidden_tab_activation_is_ignored_but_workspace_activation_is_not() {
                let mut layout = crate::layout::Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.focus_column_edge(crate::layout::Edge::First);
                ws.toggle_column_tabbed_display();
                ws.consume_window_into_column();
                let hidden = HashSet::from([1, 2, 3, 99]);
                assert!(!hidden_tab_activation(&layout, &hidden, 1));
                assert!(hidden_tab_activation(&layout, &hidden, 2));
                assert!(!hidden_tab_activation(&layout, &HashSet::new(), 2));
                layout.monitor_mut("D")
                        .unwrap()
                        .add_window_to_workspace(3, 1);
                assert!(!hidden_tab_activation(&layout, &hidden, 3));
                assert!(!hidden_tab_activation(&layout, &hidden, 99));
        }

        #[test]
        fn focus_repair_keeps_expelled_window_above_source_without_activation() {
                let mut layout = crate::layout::Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                // Same ordering is applied after synchronous focus restoration
                // and after the delayed foreground event for the source tile.
                for _ in 0..2 {
                        assert_eq!(
                                focus_repair_order(
                                        &layout,
                                        &HashMap::new(),
                                        &HashSet::new(),
                                        Some(1),
                                        &[2]
                                ),
                                vec![1, 2]
                        );
                }
                assert_eq!(
                        focus_repair_order(
                                &layout,
                                &HashMap::new(),
                                &HashSet::new(),
                                Some(2),
                                &[2]
                        ),
                        vec![2]
                );
        }

        #[test]
        fn focus_repair_preserves_background_departure_below_source() {
                let mut layout = crate::layout::Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                // Before-show staging includes the hidden B in the upcoming
                // visible set. The later focus repair must use the same order,
                // not append every departure above the focused source again.
                for focused in [Some(1), None] {
                        assert_eq!(
                                focus_repair_order(
                                        &layout,
                                        &HashMap::new(),
                                        &HashSet::new(),
                                        focused,
                                        &[2, 1],
                                ),
                                vec![2, 1],
                        );
                }
        }

        #[test]
        fn floats_hidden_tabs_fullscreen_and_overview_are_not_tiled_motion() {
                let mut layout = crate::layout::Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                layout.add_window("E", 3);
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.fullscreen_id = Some(1);
                let hidden = HashSet::from([2]);
                let overviews = HashMap::from([(
                        "E".into(),
                        super::super::Overview {
                                full: (0.0, 0.0, 1000.0, 800.0),
                                finals: Vec::new(),
                                progress: crate::anim::Val::to(
                                        1.0,
                                        crate::anim::AnimParams::default(),
                                ),
                                camera: crate::anim::Val::to(
                                        0.0,
                                        crate::anim::AnimParams::default(),
                                ),
                                zoom_target: 0.4,
                                sync_from_progress: None,
                                handoff_ready: false,
                        },
                )]);
                assert!(motion_order(&layout, &overviews, &hidden, &[1, 2, 3, 99]).is_empty());
                // A focused float (not in Layout) is never moved into the
                // tiled band just to restore a sibling animation.
                assert_eq!(
                        focus_repair_order(
                                &layout,
                                &HashMap::new(),
                                &HashSet::new(),
                                Some(99),
                                &[2]
                        ),
                        vec![2]
                );
        }
}
