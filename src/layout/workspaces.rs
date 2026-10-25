//! Dynamic workspace topology. Keep occupied/active workspaces plus one
//! trailing empty slot. The app supplies floating occupancy and decides when
//! animation/overview ownership permits reindexing.

use super::{MonitorLayout, Workspace};
use std::collections::HashSet;

#[derive(Debug)]
pub struct WorkspaceRemap {
        pub old_to_new: Vec<Option<usize>>,
}

impl WorkspaceRemap {
        pub fn reindexed(&self) -> bool {
                self.old_to_new
                        .iter()
                        .enumerate()
                        .any(|(old, new)| *new != Some(old))
        }
}

impl MonitorLayout {
        pub fn maintain_workspaces(
                &mut self,
                floating: &HashSet<usize>,
                compact: bool,
        ) -> Option<WorkspaceRemap> {
                let occupied =
                        |idx: usize| !self.workspaces[idx].is_empty() || floating.contains(&idx);
                let mut keep: Vec<_> = (0..self.workspaces.len())
                        .filter(|&idx| {
                                !compact || idx == self.active_workspace_idx || occupied(idx)
                        })
                        .collect();
                // Reuse an existing tail instead of removing/recreating it
                // each idle tick. If the active empty is already last, it IS
                // the tail; don't keep another empty after it.
                let last = self.workspaces.len() - 1;
                if compact && keep.last().is_some_and(|&idx| occupied(idx)) && !occupied(last) {
                        keep.push(last);
                }
                let append = keep.last().is_none_or(|&idx| occupied(idx));
                if keep.len() == self.workspaces.len() && !append {
                        return None;
                }
                let mut old_to_new = vec![None; self.workspaces.len()];
                for (new, &old) in keep.iter().enumerate() {
                        old_to_new[old] = Some(new);
                }
                let old = std::mem::take(&mut self.workspaces);
                self.workspaces = old
                        .into_iter()
                        .enumerate()
                        .filter_map(|(idx, ws)| old_to_new[idx].map(|_| ws))
                        .collect();
                self.active_workspace_idx = old_to_new[self.active_workspace_idx]
                        .expect("active workspace is retained");
                self.previous_workspace_idx = self
                        .previous_workspace_idx
                        .and_then(|idx| old_to_new.get(idx).copied().flatten())
                        .filter(|&idx| idx != self.active_workspace_idx);
                for idx in self.window_map.values_mut() {
                        *idx = old_to_new[*idx].expect("occupied workspace is retained");
                }
                if append {
                        self.workspaces.push(Workspace::new());
                }
                Some(WorkspaceRemap { old_to_new })
        }

        #[cfg(test)]
        pub fn normalize_workspaces(&mut self) {
                self.maintain_workspaces(&HashSet::new(), true);
        }
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn startup_and_using_last_slot_always_leave_one_trailing_empty() {
                let mut ml = MonitorLayout::new("D".into());
                assert!(ml.maintain_workspaces(&HashSet::new(), true).is_none());
                for id in 1..5 {
                        ml.switch_workspace(ml.workspaces.len() - 1);
                        ml.add_window(id);
                        ml.maintain_workspaces(&HashSet::new(), false);
                        assert_eq!(ml.workspaces.len(), id as usize + 1);
                        assert!(ml.workspaces.last().unwrap().is_empty());
                        assert!(ml.maintain_workspaces(&HashSet::new(), false).is_none());
                }
        }

        #[test]
        fn deferred_cleanup_runs_after_switch_and_preserves_active_and_previous() {
                let mut ml = MonitorLayout::new("D".into());
                ml.add_window(1);
                ml.switch_workspace(4);
                ml.add_window(2);
                ml.switch_workspace(0);
                ml.maintain_workspaces(&HashSet::new(), false);
                assert_eq!(ml.workspaces.len(), 6); // do not reindex under the camera
                let remap = ml.maintain_workspaces(&HashSet::new(), true).unwrap();
                assert_eq!(
                        remap.old_to_new,
                        vec![Some(0), None, None, None, Some(1), Some(2)]
                );
                assert_eq!(ml.workspaces.len(), 3);
                assert_eq!(ml.previous_workspace_idx, Some(1));
                assert_eq!(ml.active_workspace().focused_id(), Some(1));
                assert_eq!(ml.workspace_of(2), Some(1));
                // Repeated idle ticks are true no-ops (no reflow/repaint churn).
                assert!(ml.maintain_workspaces(&HashSet::new(), true).is_none());
        }

        #[test]
        fn floating_only_workspaces_survive_and_map_to_the_same_content() {
                let mut ml = MonitorLayout::new("D".into());
                ml.add_window(1);
                ml.switch_workspace(3);
                let remap = ml
                        .maintain_workspaces(&HashSet::from([2, 3]), true)
                        .unwrap();
                assert_eq!(remap.old_to_new, vec![Some(0), None, Some(1), Some(2)]);
                assert_eq!(ml.active_workspace_idx, 2);
                assert_eq!(ml.workspaces.len(), 4); // tiled, float, active float, empty
                assert!(ml
                        .maintain_workspaces(&HashSet::from([1, 2]), true)
                        .is_none());
        }

        #[test]
        fn closing_all_windows_leaves_only_the_active_empty_slot() {
                let mut ml = MonitorLayout::new("D".into());
                ml.add_window(1);
                ml.maintain_workspaces(&HashSet::new(), false);
                ml.remove_window(1);
                ml.maintain_workspaces(&HashSet::new(), true);
                assert_eq!(ml.workspaces.len(), 1);
                assert_eq!(ml.active_workspace_idx, 0);
                assert!(ml.maintain_workspaces(&HashSet::new(), true).is_none());
        }

        #[test]
        fn leaving_temporary_empty_workspace_reclaims_it_but_not_active_empty() {
                let mut ml = MonitorLayout::new("D".into());
                ml.add_window(1);
                ml.switch_workspace(2);
                ml.add_window(2);
                ml.switch_workspace(1);
                ml.maintain_workspaces(&HashSet::new(), true);
                assert_eq!(ml.workspaces.len(), 4); // active interior empty is deliberate
                ml.switch_workspace(2);
                ml.maintain_workspaces(&HashSet::new(), true);
                assert_eq!(ml.workspaces.len(), 3);
                assert_eq!(ml.active_workspace_idx, 1);
                assert_eq!(ml.active_workspace().focused_id(), Some(2));
                assert_eq!(ml.previous_workspace_idx, None); // removed temporary empty
        }
}
