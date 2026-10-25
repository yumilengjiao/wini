//! Tab focus handoff is different from a structural merge/split. Only an
//! unchanged group may share the previous selected tab's animation state.
//! Otherwise a moved window must keep its own position and resize trajectory.

use crate::layout::{Column, WindowId};
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub(super) struct TabSelection {
        active: WindowId,
        members: HashSet<WindowId>,
}

impl TabSelection {
        pub(super) fn capture(col: &Column) -> Option<Self> {
                Some(Self {
                        active: col.focused_id()?,
                        members: col.tiles.iter().map(|t| t.id).collect(),
                })
        }

        pub(super) fn active(&self) -> WindowId {
                self.active
        }

        pub(super) fn hidden_member(
                &self,
                id: WindowId,
        ) -> bool {
                id != self.active && self.members.contains(&id)
        }

        pub(super) fn contains(
                &self,
                id: WindowId,
        ) -> bool {
                self.members.contains(&id)
        }

        /// Merge additions, not replacements or splits. Deterministic order
        /// also makes multi-window mutations independent of HashSet iteration.
        pub(super) fn added_to(
                &self,
                old: &Self,
        ) -> Vec<WindowId> {
                if !old.members.is_subset(&self.members) {
                        return Vec::new();
                }
                let mut added: Vec<_> = self.members.difference(&old.members).copied().collect();
                added.sort_unstable();
                added
        }

        pub(super) fn membership_changed(
                &self,
                next: &Self,
        ) -> bool {
                self.members != next.members
        }

        pub(super) fn previous_for_focus_change(
                &self,
                next: &Self,
        ) -> Option<WindowId> {
                (self.active != next.active && self.members == next.members).then_some(self.active)
        }
}

/// Rightward merges can leave the incoming tile stationary while the target
/// column slides left into the removed source slot. Both surfaces must arrive.
pub(super) fn merge_in_flight(
        rects: &HashMap<WindowId, crate::anim::AnimatedRect>,
        base: WindowId,
        incoming: WindowId,
) -> bool {
        [base, incoming]
                .iter()
                .any(|id| rects.get(id).is_some_and(|r| !r.finished()))
}

/// Follow overlapping merges back to the surface at rest in the target
/// column. A selected incoming traveler must never become another tab's seed.
pub(super) fn merge_anchor(
        selected: WindowId,
        merges: &HashSet<(WindowId, WindowId)>,
) -> WindowId {
        let mut anchor = selected;
        for _ in 0..merges.len() {
                let Some(&(base, _)) = merges.iter().find(|(_, incoming)| *incoming == anchor)
                else {
                        break;
                };
                anchor = base;
        }
        anchor
}

#[cfg(test)]
mod tests {
        use super::*;
        fn selection(
                active: WindowId,
                ids: &[WindowId],
        ) -> TabSelection {
                TabSelection {
                        active,
                        members: ids.iter().copied().collect(),
                }
        }

        #[test]
        fn merge_seeding_uses_stationary_base_not_incoming_tab() {
                let merges = HashSet::from([(1, 2), (2, 3)]);
                assert_eq!(merge_anchor(3, &merges), 1);
                assert_eq!(merge_anchor(2, &merges), 1);
                assert_eq!(merge_anchor(1, &merges), 1);
                assert_eq!(merge_anchor(9, &merges), 9);
                assert_eq!(selection(2, &[1, 2]).added_to(&selection(1, &[1])), vec![2]);
                assert!(selection(2, &[2])
                        .added_to(&selection(1, &[1, 2]))
                        .is_empty());
        }

        #[test]
        fn focus_only_switch_shares_old_rendered_frame() {
                assert_eq!(
                        selection(1, &[1, 2]).previous_for_focus_change(&selection(2, &[1, 2])),
                        Some(1)
                );
        }

        #[test]
        fn merge_split_or_replacement_keeps_window_own_motion() {
                for next in [
                        selection(3, &[1, 2, 3]),
                        selection(2, &[2]),
                        selection(3, &[1, 3]),
                ] {
                        assert_eq!(selection(1, &[1, 2]).previous_for_focus_change(&next), None);
                }
        }
}
