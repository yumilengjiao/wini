//! Structural column-motion regressions. Test the composed frame and its
//! layering requests, not just that a spring exists after mutation.

use super::*;
use crate::anim::{AnimKind, Curve};
use crate::layout::geometry::{CenterFocused, Struts};
use crate::layout::{ColumnWidth, DirV, Edge};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HMONITOR;

fn setup(tabbed: bool) -> (Layout, Vec<Monitor>, LayoutParams, Transport) {
        let mut layout = Layout::new();
        layout.add_window("D", 1);
        layout.add_window("D", 2);
        let p = LayoutParams {
                gaps: 1.0,
                default_column_width: ColumnWidth::Proportion(0.5),
                center_focused_column: CenterFocused::Never,
                struts: Struts {
                        left: 3.0,
                        right: 3.0,
                        top: 12.0,
                        bottom: 6.0,
                },
                ..Default::default()
        };
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.focus_column_edge(Edge::First);
        geometry::refresh_view_offset(ws, &p, 994.0, None);
        if tabbed {
                ws.toggle_column_tabbed_display();
        }
        let mons = vec![Monitor {
                handle: HMONITOR(std::ptr::null_mut()),
                device: "D".into(),
                full: RECT {
                        left: -1000,
                        top: 0,
                        right: 0,
                        bottom: 800,
                },
                work: RECT {
                        left: -1000,
                        top: 0,
                        right: 0,
                        bottom: 800,
                },
                is_primary: true,
        }];
        let slow = AnimParams {
                kind: AnimKind::Easing {
                        duration: std::time::Duration::from_secs(60),
                        curve: Curve::Linear,
                },
                slowdown: 1.0,
        };
        let mut t = Transport::new(slow, slow, slow, slow);
        t.sync(&layout, &mons, &p);
        (layout, mons, p, t)
}

#[test]
fn tabbed_consume_slides_under_destination_but_appends_after_its_tabs() {
        let (mut layout, mons, p, mut t) = setup(true);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        assert!(ws.consume_window_into_column());
        assert_eq!(
                ws.columns[0].tiles.iter().map(|t| t.id).collect::<Vec<_>>(),
                vec![1, 2]
        );
        assert_eq!(ws.focused_id(), Some(1));
        t.sync(&layout, &mons, &p);
        let frame = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(frame.show, vec![2, 1]); // bottom-to-top: incoming underneath A
        assert_eq!(frame.raise.last(), Some(&1));
        assert_eq!(t.take_focus_raises(&layout, "D").last(), Some(&1));
        assert_eq!(t.raised_windows(&layout, "D").last(), Some(&1));
        assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
        for _ in 0..3 {
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(frame.show, vec![2, 1]);
                assert_eq!(frame.raise.last(), Some(&1));
        }
        t.snap("D");
        assert_eq!(t.frame(&layout, &mons, &HashSet::new()).show, vec![1]);
        assert_eq!(layout.focused_id("D"), Some(1));
        assert!(t.frame(&layout, &mons, &HashSet::new()).raise.is_empty());
}

#[test]
fn consuming_single_tabbed_neighbor_keeps_destination_above_incoming() {
        let (mut layout, mons, p, mut t) = setup(true);
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.focus_column_index(1);
        ws.toggle_column_tabbed_display();
        ws.focus_column_index(0);
        t.sync(&layout, &mons, &p);
        t.snap("D");
        let before = t.frame(&layout, &mons, &HashSet::new());
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        t.sync(&layout, &mons, &p);
        for _ in 0..3 {
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(frame.show, vec![2, 1]);
                assert_eq!(frame.raise.last(), Some(&1));
                assert_eq!(t.take_focus_raises(&layout, "D").last(), Some(&1));
                for old in &before.moves {
                        let new = frame.moves.iter().find(|r| r.id == old.id).unwrap();
                        assert!((old.x - new.x).abs() <= 1, "{old:?} -> {new:?}");
                }
                assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
                t.sync(&layout, &mons, &p);
        }
        t.snap("D");
        let settled = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(settled.show, vec![1]);
        assert_eq!(settled.hide, vec![2]);
        assert_eq!(layout.focused_id("D"), Some(1));
}

#[test]
fn tabbed_merge_keeps_window_identity_when_selection_changes_before_arrival() {
        // [A] [B], A is tabbed. Cover all ways of bringing B into A's
        // column, followed by navigation/activation of A before B arrives.
        for action in 0..3 {
                let (mut layout, mons, p, mut t) = setup(true);
                t.snap("D");
                let before = t.frame(&layout, &mons, &HashSet::new()).moves;
                let a = before.iter().find(|r| r.id == 1).unwrap();
                let b = before.iter().find(|r| r.id == 2).unwrap();
                assert!(b.x > a.x + 100);
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                match action {
                        0 => assert!(ws.consume_window_into_column()),
                        1 => {
                                ws.focus_column_index(1);
                                assert!(ws.consume_or_expel(crate::layout::DirH::Left));
                        },
                        _ => {
                                ws.focus_column_index(1);
                                assert!(ws.move_tile_across(crate::layout::DirH::Left));
                        },
                }
                geometry::refresh_view_offset(ws, &p, 994.0, None);
                t.sync(&layout, &mons, &p);
                // Explicitly select the traveler, then come back to A before
                // arrival. Mod+Comma itself now leaves A selected.
                layout.focus_window(2);
                t.sync(&layout, &mons, &p);
                layout.focus_window(1);
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                let actual_a = frame.moves.iter().find(|r| r.id == 1).unwrap();
                assert!(
                        (actual_a.x - a.x).abs() <= 1,
                        "A inherited incoming B's animation ({action}): A={a:?}, B={b:?}, now={actual_a:?}"
                );
                assert!(
                        frame.show.contains(&2),
                        "incoming B disappeared before arriving"
                );
                let actual_b = frame.moves.iter().find(|r| r.id == 2).unwrap();
                assert!((actual_b.x - b.x).abs() <= 1);
                assert!(!frame.hide.contains(&1) && !frame.hide.contains(&2));
                assert_eq!(frame.raise.last(), Some(&1));
                assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
                for _ in 0..3 {
                        t.sync(&layout, &mons, &p);
                        let frame = t.frame(&layout, &mons, &HashSet::new());
                        assert!(frame.show.contains(&1) && frame.show.contains(&2));
                }
                t.snap("D");
                let settled = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(settled.show, vec![1]);
                assert_eq!(settled.hide, vec![2]);
                assert_eq!(layout.focused_id("D"), Some(1));
        }
}

#[test]
fn rightward_merge_with_new_first_tile_keeps_the_destination_surface() {
        for source_tabbed in [false, true] {
                let (mut layout, mons, p, mut t) = setup(true);
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                if source_tabbed {
                        ws.focus_column_index(1);
                        ws.toggle_column_tabbed_display();
                }
                // Mirror the repro: incoming B is now on the left of A.
                ws.columns.swap(0, 1);
                ws.focus_column_index(0);
                geometry::refresh_view_offset(ws, &p, 994.0, None);
                t.sync(&layout, &mons, &p);
                t.snap("D");
                let before = t.frame(&layout, &mons, &HashSet::new()).moves;
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                assert!(ws.move_tile_across(crate::layout::DirH::Right));
                assert_eq!(ws.focused_id(), Some(2));
                assert_eq!(ws.columns[0].tiles[0].id, 2); // key changed, not group identity
                geometry::refresh_view_offset(ws, &p, 994.0, None);
                t.sync(&layout, &mons, &p);
                layout.focus_window(1);
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(frame.show.len(), 2);
                for old in before {
                        let now = frame.moves.iter().find(|r| r.id == old.id).unwrap();
                        assert!((now.x - old.x).abs() <= 1, "{old:?} -> {now:?}");
                }
                assert_eq!(frame.raise.last(), Some(&1));
        }
}

#[test]
fn repeated_merge_into_same_selected_tab_keeps_every_incoming_window() {
        let (mut layout, mons, p, mut t) = setup(true);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .focus_column_index(1);
        layout.add_window("D", 3);
        layout.focus_window(1);
        t.sync(&layout, &mons, &p);
        t.snap("D");
        let before = t.frame(&layout, &mons, &HashSet::new()).moves;
        for _ in 0..2 {
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .consume_window_into_column();
                t.sync(&layout, &mons, &p);
                layout.focus_window(1);
                t.sync(&layout, &mons, &p);
        }
        let frame = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(frame.show.len(), 3);
        assert!(frame.hide.is_empty());
        for old in before {
                let now = frame.moves.iter().find(|r| r.id == old.id).unwrap();
                assert!((now.x - old.x).abs() <= 1, "{old:?} -> {now:?}");
        }
        assert!(frame.raise.contains(&2) && frame.raise.contains(&3));
        t.snap("D");
        let settled = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(settled.show, vec![1]);
        // Later resize cannot revive already completed merge participants.
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .set_column_width(&crate::layout::SizeChange::Fixed(700.0), &p, 994.0);
        t.sync(&layout, &mons, &p);
        assert_eq!(t.frame(&layout, &mons, &HashSet::new()).show, vec![1]);
}

#[test]
fn selecting_hidden_tab_during_merge_uses_base_surface_not_incoming_trajectory() {
        let (mut layout, mons, p, mut t) = setup(true);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        layout.focus_window(1);
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        layout.add_window("D", 3);
        layout.focus_window(1);
        t.sync(&layout, &mons, &p);
        t.snap("D");
        let before = t.frame(&layout, &mons, &HashSet::new()).moves;
        let base = before.iter().find(|r| r.id == 1).unwrap();
        let incoming = before.iter().find(|r| r.id == 3).unwrap();
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        t.sync(&layout, &mons, &p);
        let hidden = t.monitors.get_mut("D").unwrap().tiles.get_mut(&2).unwrap();
        hidden.retarget(777.0, 200.0, 300.0, 100.0, t.movement, t.resize);
        hidden.settle();
        layout.focus_window(2);
        t.sync(&layout, &mons, &p);
        let frame = t.frame(&layout, &mons, &HashSet::new());
        let selected = frame.moves.iter().find(|r| r.id == 2).unwrap();
        assert!((selected.x - base.x).abs() <= 1);
        assert!((selected.y - base.y).abs() <= 1);
        let moving = frame.moves.iter().find(|r| r.id == 3).unwrap();
        assert!((moving.x - incoming.x).abs() <= 1);
        assert_eq!(frame.raise.last(), Some(&2));
        assert_eq!(
                t.frame(&layout, &mons, &HashSet::new()).raise.last(),
                Some(&2)
        );
        t.snap("D");
        assert_eq!(t.frame(&layout, &mons, &HashSet::new()).show, vec![2]);
}

#[test]
fn revealing_hidden_sibling_during_merge_uses_destination_not_incoming_surface() {
        for expand_stack in [false, true] {
                let (mut layout, mons, p, mut t) = setup(true);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .consume_window_into_column();
                layout.focus_window(1);
                t.sync(&layout, &mons, &p);
                t.snap("D");
                t.frame(&layout, &mons, &HashSet::new());
                // A settled tab group [1, 2], with 2 hidden, beside incoming 3.
                layout.add_window("D", 3);
                layout.focus_window(1);
                t.sync(&layout, &mons, &p);
                t.snap("D");
                t.frame(&layout, &mons, &HashSet::new());
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .consume_window_into_column();
                layout.focus_window(3); // explicitly select the incoming traveler
                t.sync(&layout, &mons, &p);
                let before = t.frame(&layout, &mons, &HashSet::new());
                let base = before.moves.iter().find(|r| r.id == 1).unwrap();
                let incoming = before.moves.iter().find(|r| r.id == 3).unwrap();
                assert!(incoming.x > base.x + 100);
                assert!(before.hide.contains(&2));
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                if expand_stack {
                        ws.toggle_column_tabbed_display();
                } else {
                        // Splitting the selected traveler makes hidden 2 the replacement.
                        assert!(ws.expel_window_from_column());
                        assert_eq!(ws.focused_id(), Some(2));
                }
                t.sync(&layout, &mons, &p);
                let after = t.frame(&layout, &mons, &HashSet::new());
                let revealed = after.moves.iter().find(|r| r.id == 2).unwrap();
                assert!(
                        (revealed.x - base.x).abs() <= 1,
                        "hidden sibling inherited traveler (expand_stack={expand_stack}): {base:?} -> {revealed:?}"
                );
                assert!((revealed.y - base.y).abs() <= 1);
                assert!((revealed.w - base.w).abs() <= 1);
                assert!((revealed.h - base.h).abs() <= 1);
                let moving = after.moves.iter().find(|r| r.id == 3).unwrap();
                assert!((moving.x - incoming.x).abs() <= 1);
                assert_eq!(after.raise.last(), Some(&3));
                assert_eq!(t.workspace_rects(&layout, "D")[0].1, after.moves);
                t.snap("D");
                let settled = t.frame(&layout, &mons, &HashSet::new());
                assert!(settled.show.contains(&2) && settled.show.contains(&3));
                assert_eq!(settled.show.contains(&1), expand_stack);
        }
}

#[test]
fn consuming_from_tabbed_source_keeps_survivor_visible_and_seeds_hidden_incoming() {
        for source_active in [0, 1] {
                let (mut layout, mons, p, mut t) = setup(true);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .focus_column_index(1);
                layout.add_window("D", 3);
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.focus_column_index(1);
                assert!(ws.consume_window_into_column());
                ws.toggle_column_tabbed_display();
                ws.columns[1].active_tile_idx = source_active;
                ws.focus_column_index(0);
                t.sync(&layout, &mons, &p);
                t.snap("D");
                let before = t.frame(&layout, &mons, &HashSet::new());
                let source = before
                        .moves
                        .iter()
                        .find(|r| r.id == 2 + source_active as isize)
                        .unwrap();
                // A hidden tab's internal spring is not a submitted native rect.
                let hidden_id = if source_active == 0 { 3 } else { 2 };
                let hidden = t
                        .monitors
                        .get_mut("D")
                        .unwrap()
                        .tiles
                        .get_mut(&hidden_id)
                        .unwrap();
                hidden.retarget(777.0, 200.0, 300.0, 100.0, t.movement, t.resize);
                hidden.settle();
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                assert!(ws.consume_window_into_column());
                assert_eq!(ws.focused_id(), Some(1));
                assert_eq!(ws.columns[1].focused_id(), Some(3));
                t.sync(&layout, &mons, &p);
                let previews = t.workspace_rects(&layout, "D")[0].1.clone();
                let after = t.frame(&layout, &mons, &HashSet::new());
                for id in [2, 3] {
                        assert!(after.show.contains(&id) && !after.hide.contains(&id));
                        let rect = after.moves.iter().find(|r| r.id == id).unwrap();
                        assert!(
                                (rect.x - source.x).abs() <= 1,
                                "source_active={source_active}: {source:?} -> {rect:?}"
                        );
                        assert!((rect.y - source.y).abs() <= 1);
                        assert!((rect.w - source.w).abs() <= 1);
                        assert!((rect.h - source.h).abs() <= 1);
                }
                assert_eq!(previews, after.moves);
                t.snap("D");
                let settled = t.frame(&layout, &mons, &HashSet::new());
                assert!(settled.show.contains(&1) && settled.show.contains(&3));
                assert_eq!(settled.hide, vec![2]);
                assert_eq!(layout.focused_id("D"), Some(1));
        }
}

#[test]
fn merge_preserves_incoming_motion_with_display_specific_layering() {
        for tabbed in [false, true] {
                let (mut layout, mons, p, mut t) = setup(tabbed);
                let before = t.workspace_rects(&layout, "D")[0]
                        .1
                        .iter()
                        .find(|r| r.id == 2)
                        .copied()
                        .unwrap();
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.consume_window_into_column();
                assert_eq!(ws.focused_id(), Some(1));
                geometry::refresh_view_offset(ws, &p, 994.0, None);
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert!(frame.raise.contains(&2));
                let incoming = frame.moves.iter().find(|r| r.id == 2).unwrap();
                assert!((incoming.x - before.x).abs() <= 1);
                assert!((incoming.y - before.y).abs() <= 1);
                assert!(!frame.hide.contains(&1));
                assert!(frame.show.contains(&1) && frame.show.contains(&2));
                assert_eq!(frame.raise.last(), Some(if tabbed { &1 } else { &2 }));
                // Repeated reflow must not reset the structural animation.
                t.sync(&layout, &mons, &p);
                assert!(t.layout_animating("D"));
        }
}

#[test]
fn hidden_tab_expel_starts_from_current_visible_group_not_hidden_spring() {
        let (mut layout, mons, p, mut t) = setup(true);
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.consume_window_into_column();
        t.sync(&layout, &mons, &p);
        t.snap("D");
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.focus_tile(DirV::Up); // Keep first tab selected; bottom tab is hidden.
        t.sync(&layout, &mons, &p);
        t.snap("D");
        // Hidden HWNDs do not receive these requests while the selected tab resizes.
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.set_column_width(&crate::layout::SizeChange::Fixed(700.0), &p, 994.0);
        t.sync(&layout, &mons, &p);
        let before = t.workspace_rects(&layout, "D")[0].1[0];
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.expel_window_from_column();
        assert_eq!(ws.focused_id(), Some(1));
        geometry::refresh_view_offset(ws, &p, 994.0, None);
        t.sync(&layout, &mons, &p);
        let frame = t.frame(&layout, &mons, &HashSet::new());
        let expelled = frame.moves.iter().find(|r| r.id == 2).unwrap();
        assert!(
                (expelled.x - before.x).abs() <= 1,
                "hidden departure jumped: {before:?} -> {expelled:?}"
        );
        assert!((expelled.y - before.y).abs() <= 1);
        assert!((expelled.w - before.w).abs() <= 1);
        assert!(frame.raise.contains(&2));
        assert!(frame.show.contains(&1) && frame.show.contains(&2));
        assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
}

#[test]
fn background_tab_expel_stays_under_source_through_reveal_and_focus_repair() {
        for merge_settled in [false, true] {
                let (mut layout, mons, p, mut t) = setup(true);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .consume_window_into_column();
                t.sync(&layout, &mons, &p);
                if merge_settled {
                        t.snap("D");
                }
                let before = t.frame(&layout, &mons, &HashSet::new());
                let seed = before
                        .moves
                        .iter()
                        .find(|r| r.id == if merge_settled { 1 } else { 2 })
                        .unwrap();
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .expel_window_from_column();
                t.sync(&layout, &mons, &p);
                for _ in 0..3 {
                        let frame = t.frame(&layout, &mons, &HashSet::new());
                        assert_eq!(
                                frame.show,
                                vec![2, 1],
                                "background B must not cover A on reveal"
                        );
                        assert_eq!(frame.raise, vec![2, 1]);
                        assert_eq!(t.take_focus_raises(&layout, "D"), vec![2, 1]);
                        assert_eq!(t.raised_windows(&layout, "D"), vec![2, 1]);
                        assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
                        assert_eq!(layout.focused_id("D"), Some(1));
                        let b = frame.moves.iter().find(|r| r.id == 2).unwrap();
                        assert!((b.x - seed.x).abs() <= 1, "{seed:?} -> {b:?}");
                        t.sync(&layout, &mons, &p);
                }
                t.snap("D");
                let settled = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(settled.raise, vec![2, 1]);
                assert_eq!(t.take_focus_raises(&layout, "D"), vec![2, 1]);
                assert!(t.frame(&layout, &mons, &HashSet::new()).raise.is_empty());
                // This is a transition constraint, not an always-on-top setting.
                layout.focus_window(2);
                t.sync(&layout, &mons, &p);
                assert!(t.raised_windows(&layout, "D").is_empty());
        }
}

#[test]
fn background_tab_expel_follows_source_selection_but_yields_to_departing_focus() {
        let (mut layout, mons, p, mut t) = setup(true);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        layout.add_window("D", 3);
        layout.focus_window(1);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .expel_window_from_column();
        t.sync(&layout, &mons, &p);
        assert_eq!(t.frame(&layout, &mons, &HashSet::new()).raise, vec![3, 1]);
        layout.focus_window(2);
        for _ in 0..3 {
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(frame.show, vec![3, 2]);
                assert_eq!(frame.hide, vec![1]);
                assert_eq!(frame.raise, vec![3, 2]);
                assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
        }
        layout.focus_window(3); // Explicit activation of the moving, now separate column.
        t.sync(&layout, &mons, &p);
        let frame = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(frame.raise.last(), Some(&3));
        assert_eq!(t.take_focus_raises(&layout, "D").last(), Some(&3));
        layout.focus_window(2);
        t.sync(&layout, &mons, &p);
        assert_eq!(
                t.frame(&layout, &mons, &HashSet::new()).raise.last(),
                Some(&3)
        );
}

#[test]
fn instant_background_tab_expel_repairs_reveal_once_without_permanent_raise() {
        let (mut layout, mons, p, mut t) = setup(true);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        let instant = AnimParams {
                kind: AnimKind::instant(),
                slowdown: 1.0,
        };
        t.set_params(instant, instant, instant, instant);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .expel_window_from_column();
        t.sync(&layout, &mons, &p);
        assert_eq!(t.frame(&layout, &mons, &HashSet::new()).raise, vec![2, 1]);
        assert_eq!(t.take_focus_raises(&layout, "D"), vec![2, 1]);
        assert!(t.raised_windows(&layout, "D").is_empty());
        t.sync(&layout, &mons, &p);
        assert!(t.frame(&layout, &mons, &HashSet::new()).raise.is_empty());
}

#[test]
fn focus_handoff_retains_last_frame_even_when_motion_is_instant_or_settled() {
        for instant in [false, true] {
                let (mut layout, mons, p, mut t) = setup(false);
                // Start from a fully applied, resting scene. Val::to retains
                // the setup easing duration even when from == to.
                t.snap("D");
                if instant {
                        let params = AnimParams {
                                kind: AnimKind::instant(),
                                slowdown: 1.0,
                        };
                        t.set_params(params, params, params, params);
                }
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.consume_window_into_column();
                t.sync(&layout, &mons, &p);
                if !instant {
                        t.snap("D");
                }
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert!(frame.raise.contains(&2));
                assert!(t.raised_windows(&layout, "D").is_empty());
                // The action restores focus AFTER applying this final frame.
                assert!(t.take_focus_raises(&layout, "D").contains(&2));
                assert!(t.take_focus_raises(&layout, "D").is_empty());
                assert!(t.frame(&layout, &mons, &HashSet::new()).raise.is_empty());
        }
}

#[test]
fn delayed_focus_repair_does_not_advance_motion_or_expose_hidden_tabs() {
        let (mut layout, mons, p, mut t) = setup(true);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        layout.focus_window(2); // test delayed activation of a selected traveler
        t.sync(&layout, &mons, &p);
        t.frame(&layout, &mons, &HashSet::new());
        for _ in 0..3 {
                assert_eq!(t.raised_windows(&layout, "D"), vec![2, 1]);
                assert_eq!(t.take_focus_raises(&layout, "D"), vec![2, 1]);
        }
        assert!(t.layout_animating("D"));
        // Overview owns its output; sampling skipped frames must not consume
        // its pending final layer repair before the real-window handoff.
        let skipped = t.frame(&layout, &mons, &HashSet::from(["D".into()]));
        assert!(skipped.raise.is_empty());
        assert_eq!(t.raised_windows(&layout, "D"), vec![2, 1]);
        t.remove_window(2);
        assert!(t.raised_windows(&layout, "D").is_empty());
        assert!(t.take_focus_raises(&layout, "D").is_empty());
}

#[test]
fn ordinary_tab_selection_never_creates_structural_layering_requests() {
        let (mut layout, mons, p, mut t) = setup(true);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        t.take_focus_raises(&layout, "D");
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .focus_tile(DirV::Down);
        t.sync(&layout, &mons, &p);
        assert!(t.frame(&layout, &mons, &HashSet::new()).raise.is_empty());
        assert!(t.raised_windows(&layout, "D").is_empty());
        assert!(t.take_focus_raises(&layout, "D").is_empty());
}

#[test]
fn consuming_first_of_stacked_source_only_raises_the_incoming_window() {
        let (mut layout, mons, p, mut t) = setup(false);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .focus_column_index(1);
        layout.add_window("D", 3);
        layout.add_window("D", 4);
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.focus_column_index(1);
        ws.consume_window_into_column();
        ws.consume_window_into_column();
        ws.focus_column_edge(Edge::First);
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        t.take_focus_raises(&layout, "D");
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        t.sync(&layout, &mons, &p);
        let frame = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(frame.raise, vec![2]);
}

#[test]
fn splitting_multiple_siblings_into_new_column_still_raises_departing_tiles() {
        let (mut layout, mons, p, mut t) = setup(false);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        layout.add_window("D", 3);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .focus_column_edge(Edge::First);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        t.take_focus_raises(&layout, "D");
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        // Split two bottom tiles together to exercise a new key whose
        // members were old siblings, while the old key still exists.
        let tail = ws.columns[0].tiles.split_off(1);
        let mut col = crate::layout::Column::new(tail[0].id);
        col.width = ws.columns[0].width;
        col.tiles = tail;
        ws.columns[0].active_tile_idx = 0;
        ws.columns.push(col);
        t.sync(&layout, &mons, &p);
        let frame = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(frame.raise, vec![2, 3]);
}

#[test]
fn active_tab_expel_seeds_replacement_without_overwriting_departing_motion() {
        let (mut layout, mons, mut p, mut t) = setup(true);
        p.tab_indicator.place_within_column = true;
        p.tab_indicator.width = 10.0;
        p.tab_indicator.gap = 12.0;
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        layout.focus_window(2); // expel a selected tab, not the hidden bottom tab
        t.sync(&layout, &mons, &p);
        t.snap("D");
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.set_column_width(&crate::layout::SizeChange::Fixed(700.0), &p, 994.0);
        geometry::refresh_view_offset(ws, &p, 994.0, None);
        t.sync(&layout, &mons, &p);
        // A hidden tab's internal trajectory is not submitted to its HWND.
        // Force that trajectory ahead of the visible surface to ensure the
        // replacement is truly seeded, not accidentally equal at t ~= 0.
        t.monitors
                .get_mut("D")
                .unwrap()
                .tiles
                .get_mut(&1)
                .unwrap()
                .settle();
        let before = t.workspace_rects(&layout, "D")[0].1[0];
        assert_eq!(before.id, 2);
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.expel_window_from_column();
        assert_eq!(ws.focused_id(), Some(1));
        geometry::refresh_view_offset(ws, &p, 994.0, None);
        t.sync(&layout, &mons, &p);
        let frame = t.frame(&layout, &mons, &HashSet::new());
        for id in [1, 2] {
                let after = frame.moves.iter().find(|r| r.id == id).unwrap();
                assert!((after.x - before.x).abs() <= 1, "{before:?} -> {after:?}");
                assert!((after.y - before.y).abs() <= 1);
                assert!((after.w - before.w).abs() <= 1);
                assert!((after.h - before.h).abs() <= 1);
        }
        assert_eq!(frame.raise.last(), Some(&2)); // A selected departing tab stays in front.
        assert_eq!(t.take_focus_raises(&layout, "D").last(), Some(&2));
}

#[test]
fn tab_to_stack_toggle_preserves_selected_surface_above_all_revealed_siblings() {
        for active in 0..3 {
                let (mut layout, mons, p, mut t) = setup(false);
                layout.add_window("D", 3);
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.focus_column_edge(Edge::First);
                ws.consume_window_into_column();
                ws.consume_window_into_column();
                ws.columns[0].active_tile_idx = active;
                ws.toggle_column_tabbed_display();
                t.sync(&layout, &mons, &p);
                t.snap("D");
                t.frame(&layout, &mons, &HashSet::new());
                let before = t.workspace_rects(&layout, "D")[0].1[0];
                assert_eq!(Some(before.id), layout.focused_id("D"));
                // Make hidden internal springs differ from the submitted
                // selected surface (e.g. sizing while overview is open).
                for (&id, rect) in &mut t.monitors.get_mut("D").unwrap().tiles {
                        if id != before.id {
                                rect.retarget(80.0, 200.0, 300.0, 100.0, t.movement, t.resize);
                                rect.settle();
                        }
                }
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .toggle_column_tabbed_display();
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(frame.raise.len(), 3);
                assert_eq!(frame.raise.last(), Some(&before.id));
                assert_eq!(t.take_focus_raises(&layout, "D").last(), Some(&before.id));
                assert_eq!(t.raised_windows(&layout, "D").last(), Some(&before.id));
                for after in &frame.moves {
                        assert!((after.x - before.x).abs() <= 1, "{before:?} -> {after:?}");
                        assert!((after.y - before.y).abs() <= 1);
                        assert!((after.w - before.w).abs() <= 1);
                        assert!((after.h - before.h).abs() <= 1);
                }
                assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
                t.snap("D");
                let final_frame = t.frame(&layout, &mons, &HashSet::new());
                assert_eq!(final_frame.raise.last(), Some(&before.id));
                assert!(t.frame(&layout, &mons, &HashSet::new()).raise.is_empty());
        }
}

#[test]
fn stack_to_tab_keeps_outgoing_surfaces_until_expansion_finishes() {
        let (mut layout, mons, p, mut t) = setup(false);
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.consume_window_into_column();
        ws.columns[0].active_tile_idx = 0;
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        let before = t.workspace_rects(&layout, "D")[0].1.clone();
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .toggle_column_tabbed_display();
        t.sync(&layout, &mons, &p);
        let frame = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(frame.show, vec![2, 1]);
        assert!(frame.hide.is_empty());
        assert_eq!(frame.raise.last(), Some(&1));
        assert_eq!(t.workspace_rects(&layout, "D")[0].1, frame.moves);
        for old in &before {
                let new = frame.moves.iter().find(|r| r.id == old.id).unwrap();
                assert!((new.y - old.y).abs() <= 1);
                assert!((new.h - old.h).abs() <= 1);
        }
        // Idempotent refresh, including overview refresh, must retain tails.
        t.sync(&layout, &mons, &p);
        assert_eq!(t.frame(&layout, &mons, &HashSet::new()).show, vec![2, 1]);
        t.snap("D");
        let settled = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(settled.show, vec![1]);
        assert_eq!(settled.hide, vec![2]);
        assert_eq!(t.workspace_rects(&layout, "D")[0].1, settled.moves);
}

#[test]
fn rapid_display_toggle_reversal_keeps_each_visible_surface_continuous() {
        let (mut layout, mons, p, mut t) = setup(false);
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .consume_window_into_column();
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        for _ in 0..5 {
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .toggle_column_tabbed_display();
                t.sync(&layout, &mons, &p);
                let before = t.frame(&layout, &mons, &HashSet::new()).moves;
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .toggle_column_tabbed_display();
                t.sync(&layout, &mons, &p);
                let after = t.frame(&layout, &mons, &HashSet::new());
                for old in &before {
                        let new = after.moves.iter().find(|r| r.id == old.id).unwrap();
                        assert!((new.y - old.y).abs() <= 1, "{old:?} -> {new:?}");
                        assert!((new.h - old.h).abs() <= 1);
                }
                assert_eq!(after.raise.last(), Some(&1));
        }
}

#[test]
fn workspace_compaction_remaps_camera_and_view_without_restarting_tile_motion() {
        let (mut layout, mons, p, mut t) = setup(false);
        let ml = layout.monitor_mut("D").unwrap();
        ml.switch_workspace(2);
        ml.add_window(3);
        ml.add_window(4);
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.set_column_width(&crate::layout::SizeChange::Fixed(750.0), &p, 994.0);
        geometry::refresh_view_offset(ws, &p, 994.0, None);
        t.sync(&layout, &mons, &p);
        assert!(!t.switching("D"));
        let before = t.frame(&layout, &mons, &HashSet::new());
        let old_view_target = t.monitors["D"].view[&2].target();
        let remap = layout
                .monitor_mut("D")
                .unwrap()
                .maintain_workspaces(&HashSet::new(), true)
                .unwrap();
        t.remap_workspaces("D", &remap);
        t.sync(&layout, &mons, &p);
        assert_eq!(t.monitors["D"].view[&1].target(), old_view_target);
        assert!(!t.switching("D"));
        assert!(t.layout_animating("D"));
        let after = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(before.show, after.show);
        assert_eq!(before.hide, after.hide);
        for old in before.moves {
                let new = after.moves.iter().find(|r| r.id == old.id).unwrap();
                assert!((new.x - old.x).abs() <= 1);
                assert!((new.y - old.y).abs() <= 1);
                assert!((new.w - old.w).abs() <= 1);
                assert!((new.h - old.h).abs() <= 1);
        }
}

#[test]
fn expelling_background_tab_during_display_toggle_keeps_source_above_it() {
        let (mut layout, mons, p, mut t) = setup(false);
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.consume_window_into_column();
        ws.columns[0].active_tile_idx = 0;
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .toggle_column_tabbed_display();
        t.sync(&layout, &mons, &p);
        assert_eq!(
                t.frame(&layout, &mons, &HashSet::new()).raise.last(),
                Some(&1)
        );
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .expel_window_from_column();
        t.sync(&layout, &mons, &p);
        assert_eq!(
                t.frame(&layout, &mons, &HashSet::new()).raise.last(),
                Some(&1)
        );
        assert_eq!(t.take_focus_raises(&layout, "D").last(), Some(&1));
}

#[test]
fn settled_display_toggle_keeps_background_expel_below_source() {
        let (mut layout, mons, p, mut t) = setup(false);
        let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
        ws.consume_window_into_column();
        ws.columns[0].active_tile_idx = 0;
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .toggle_column_tabbed_display();
        t.sync(&layout, &mons, &p);
        t.snap("D");
        t.frame(&layout, &mons, &HashSet::new());
        layout.monitor_mut("D")
                .unwrap()
                .active_workspace_mut()
                .expel_window_from_column();
        t.sync(&layout, &mons, &p);
        let frame = t.frame(&layout, &mons, &HashSet::new());
        assert_eq!(frame.raise.last(), Some(&1));
        assert_eq!(frame.moves.last().map(|r| r.id), Some(1));
        assert_eq!(t.take_focus_raises(&layout, "D").last(), Some(&1));
}

#[test]
fn rapid_merge_then_expel_stays_continuous_on_negative_monitor() {
        for tabbed in [false, true] {
                let (mut layout, mons, p, mut t) = setup(tabbed);
                layout.monitor_mut("D")
                        .unwrap()
                        .active_workspace_mut()
                        .consume_window_into_column();
                t.sync(&layout, &mons, &p);
                let before = t.workspace_rects(&layout, "D")[0]
                        .1
                        .iter()
                        .find(|r| r.id == 2)
                        .copied()
                        .unwrap();
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.expel_window_from_column();
                geometry::refresh_view_offset(ws, &p, 994.0, None);
                t.sync(&layout, &mons, &p);
                let frame = t.frame(&layout, &mons, &HashSet::new());
                let after = frame.moves.iter().find(|r| r.id == 2).unwrap();
                assert!((before.x - after.x).abs() <= 1);
                assert!((before.y - after.y).abs() <= 1);
                assert!(frame.raise.contains(&2));
                t.snap("D");
                let final_frame = t.frame(&layout, &mons, &HashSet::new());
                // One last raise at settle; no persistent topmost behavior.
                assert!(final_frame.raise.contains(&2));
                assert!(t.frame(&layout, &mons, &HashSet::new()).raise.is_empty());
        }
}
