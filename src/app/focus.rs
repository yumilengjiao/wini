//! Internal selection is authoritative while overview owns a monitor. WinEvent
//! foreground notifications are observations, not commands: queued/stale events
//! and covered source windows must not undo keyboard or thumbnail selection.

use super::{AppState, Overview, floating::FloatState};
use crate::layout::{Layout, WindowId};
use std::collections::{HashMap, HashSet};

pub(super) fn window_device<'a>(
        layout: &'a Layout,
        floating: &'a HashMap<WindowId, FloatState>,
        id: WindowId,
) -> Option<&'a str> {
        layout.monitors
                .iter()
                .find_map(|ml| ml.workspace_of(id).map(|_| ml.device.as_str()))
                .or_else(|| floating.get(&id).map(|fs| fs.device.as_str()))
}

pub(super) fn accept_foreground(
        layout: &Layout,
        floating: &HashMap<WindowId, FloatState>,
        overviews: &HashMap<String, Overview>,
        hidden: &HashSet<WindowId>,
        event: WindowId,
        current: WindowId,
) -> bool {
        if event != current {
                return false;
        }
        if super::layering::hidden_tab_activation(layout, hidden, event) {
                return false;
        }
        match window_device(layout, floating, event) {
                Some(device) => !overviews.contains_key(device),
                // Hiding a source may foreground the desktop or an unrelated
                // app. Keep overview's selection even when its workspace is empty.
                None => overviews.is_empty(),
        }
}

pub(super) fn selection_device<'a>(
        layout: &'a Layout,
        floating: &'a HashMap<WindowId, FloatState>,
        focused: Option<WindowId>,
        selected: Option<&'a str>,
        cursor: Option<&'a str>,
) -> Option<&'a str> {
        focused.and_then(|id| window_device(layout, floating, id))
                .or_else(|| selected.filter(|d| layout.monitor(d).is_some()))
                .or(cursor)
}

impl AppState {
        pub(super) fn focus_device(&self) -> Option<String> {
                let cursor = crate::win::monitor::monitor_at_cursor(&self.monitors);
                selection_device(
                        &self.layout,
                        &self.floating,
                        self.focused.map(|h| h.0 as isize),
                        self.focused_device.as_deref(),
                        cursor.map(|m| m.device.as_str()),
                )
                .map(str::to_owned)
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::anim::{AnimKind, AnimParams, Val};

        fn overview() -> Overview {
                let instant = AnimParams {
                        kind: AnimKind::instant(),
                        slowdown: 1.0,
                };
                Overview {
                        progress: Val::to(1.0, instant),
                        camera: Val::to(0.0, instant),
                        finals: Vec::new(),
                        full: (0.0, 0.0, 1000.0, 800.0),
                        zoom_target: 0.35,
                        sync_from_progress: None,
                        handoff_ready: false,
                }
        }

        #[test]
        fn covered_foreground_does_not_undo_repeated_workspace_selection() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.monitor_mut("D")
                        .unwrap()
                        .add_window_to_workspace(2, 1);
                let mut overviews = HashMap::from([("D".into(), overview())]);
                let floats = HashMap::new();
                let hidden = HashSet::new();
                let instant = AnimParams {
                        kind: AnimKind::instant(),
                        slowdown: 1.0,
                };
                for target in [1, 0, 1, 2, 0, 2, 1] {
                        let ov = overviews.get_mut("D").unwrap();
                        let expected = ov.activate_workspace(
                                layout.monitor_mut("D").unwrap(),
                                target,
                                true,
                                instant,
                        );
                        // Both stale notifications and currently foreground source
                        // windows are ignored during open/close/prepare/reopen.
                        for phase in 0..4 {
                                let ov = overviews.get_mut("D").unwrap();
                                match phase {
                                        1 => ov.close_to(target as f64, instant),
                                        2 => ov.handoff_ready = true,
                                        3 => ov.reopen(instant),
                                        _ => {},
                                }
                                for (event, current) in [(1, 2), (1, 1), (2, 2), (99, 99)] {
                                        if accept_foreground(
                                                &layout, &floats, &overviews, &hidden, event,
                                                current,
                                        ) {
                                                layout.focus_window(event);
                                        }
                                        assert_eq!(
                                                layout.monitor("D").unwrap().active_workspace_idx,
                                                target
                                        );
                                        assert_eq!(layout.focused_id("D"), expected);
                                }
                        }
                }
        }

        #[test]
        fn stale_events_after_reveal_do_not_switch_back_but_real_activation_does() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.monitor_mut("D")
                        .unwrap()
                        .add_window_to_workspace(2, 1);
                layout.monitor_mut("D").unwrap().switch_workspace(1);
                let floats = HashMap::new();
                let overviews = HashMap::new();
                let hidden = HashSet::from([1]);
                assert!(!accept_foreground(
                        &layout, &floats, &overviews, &hidden, 1, 2
                ));
                // Genuine taskbar/Alt+Tab activation may still select another workspace.
                assert!(accept_foreground(
                        &layout, &floats, &overviews, &hidden, 1, 1
                ));
                assert!(accept_foreground(
                        &layout, &floats, &overviews, &hidden, 99, 99
                ));
        }

        #[test]
        fn empty_workspace_keeps_selected_output_even_when_cursor_is_elsewhere() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("E", 2);
                layout.monitor_mut("D").unwrap().switch_workspace(1);
                let floats = HashMap::new();
                assert_eq!(
                        selection_device(&layout, &floats, None, Some("D"), Some("E")),
                        Some("D")
                );
                assert_eq!(
                        selection_device(&layout, &floats, Some(2), Some("D"), Some("D")),
                        Some("E")
                );
                assert_eq!(
                        selection_device(&layout, &floats, None, Some("removed"), Some("E")),
                        Some("E")
                );
        }

        #[test]
        fn overview_policy_is_per_output_and_includes_hidden_floats() {
                let mut layout = Layout::new();
                layout.add_window("D", 1);
                layout.add_window("E", 2);
                let floats = HashMap::from([(
                        10,
                        FloatState {
                                device: "D".into(),
                                workspace_idx: 0,
                                x: 0.0,
                                y: 0.0,
                                w: 400.0,
                                h: 600.0,
                        },
                )]);
                let overviews = HashMap::from([("D".into(), overview())]);
                assert!(!accept_foreground(
                        &layout,
                        &floats,
                        &overviews,
                        &HashSet::from([10]),
                        10,
                        10
                ));
                assert!(accept_foreground(
                        &layout,
                        &floats,
                        &overviews,
                        &HashSet::new(),
                        2,
                        2
                ));
        }
}
