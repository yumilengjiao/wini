//! Floating-window placement state and cross-layer focus selection.
//!
//! Selection is independent of Win32: never cross monitors/workspaces, never
//! pick a hidden layer in overview, and remember the last focused float.

use crate::layout::WindowId;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub(super) struct FloatState {
        pub(super) device: String,
        pub(super) workspace_idx: usize,
        pub(super) x: f64,
        pub(super) y: f64,
        pub(super) w: f64,
        pub(super) h: f64,
}

#[derive(Debug, Default)]
pub(super) struct FloatingFocus {
        recent: Vec<WindowId>,
}

impl FloatingFocus {
        pub(super) fn record(
                &mut self,
                id: WindowId,
        ) {
                self.remove(id);
                self.recent.push(id);
        }

        pub(super) fn remove(
                &mut self,
                id: WindowId,
        ) {
                self.recent.retain(|&i| i != id);
        }

        pub(super) fn switch_target(
                &self,
                floating: &HashMap<WindowId, FloatState>,
                current: Option<WindowId>,
                tiled: Option<WindowId>,
                device: &str,
                workspace: usize,
                overview: bool,
        ) -> Option<WindowId> {
                if overview {
                        return None;
                } // Floats are hidden under the host.
                let on_workspace = |id: &WindowId| {
                        floating.get(id).is_some_and(|fs| {
                                fs.device == device && fs.workspace_idx == workspace
                        })
                };
                let float = self
                        .recent
                        .iter()
                        .rev()
                        .find(|id| on_workspace(id))
                        .copied()
                        .or_else(|| floating.keys().filter(|id| on_workspace(id)).min().copied());
                // Empty destination layer is a no-op, not focus loss.
                let (Some(float), Some(tiled)) = (float, tiled) else {
                        return None;
                };
                if current.is_some_and(|id| on_workspace(&id)) {
                        Some(tiled)
                } else {
                        Some(float)
                }
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        fn float(
                device: &str,
                workspace: usize,
        ) -> FloatState {
                FloatState {
                        device: device.into(),
                        workspace_idx: workspace,
                        x: 0.0,
                        y: 0.0,
                        w: 500.0,
                        h: 500.0,
                }
        }

        #[test]
        fn switch_between_layers_remembers_last_float_without_retiling() {
                let floats = HashMap::from([(10, float("D", 0)), (20, float("D", 0))]);
                let mut focus = FloatingFocus::default();
                focus.record(10);
                focus.record(20);
                assert_eq!(
                        focus.switch_target(&floats, Some(1), Some(1), "D", 0, false),
                        Some(20)
                );
                assert_eq!(
                        focus.switch_target(&floats, Some(20), Some(1), "D", 0, false),
                        Some(1)
                );
                assert_eq!(floats.len(), 2);
                focus.record(10);
                assert_eq!(
                        focus.switch_target(&floats, Some(1), Some(1), "D", 0, false),
                        Some(10)
                );
        }

        #[test]
        fn hidden_or_other_monitor_floats_are_not_selected() {
                let floats = HashMap::from([
                        (10, float("D", 1)),
                        (20, float("E", 0)),
                        (30, float("D", 0)),
                ]);
                let mut focus = FloatingFocus::default();
                focus.record(30);
                focus.record(20);
                focus.record(10);
                assert_eq!(
                        focus.switch_target(&floats, Some(1), Some(1), "D", 0, false),
                        Some(30)
                );
                assert_eq!(
                        focus.switch_target(&floats, Some(1), Some(1), "D", 0, true),
                        None
                );
        }

        #[test]
        fn empty_layer_and_removed_float_are_safe_noops() {
                let mut floats = HashMap::from([(10, float("D", 0)), (20, float("D", 0))]);
                let mut focus = FloatingFocus::default();
                focus.record(20);
                floats.remove(&20);
                focus.remove(20);
                assert_eq!(
                        focus.switch_target(&floats, Some(1), Some(1), "D", 0, false),
                        Some(10)
                );
                assert_eq!(
                        focus.switch_target(&floats, Some(10), None, "D", 0, false),
                        None
                );
                floats.clear();
                assert_eq!(
                        focus.switch_target(&floats, Some(1), Some(1), "D", 0, false),
                        None
                );
        }
}
