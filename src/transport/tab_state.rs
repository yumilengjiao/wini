//! Tab focus handoff is different from a structural merge/split. Only an
//! unchanged group may share the previous selected tab's animation state.
//! Otherwise a moved window must keep its own position and resize trajectory.

use crate::layout::{Column, WindowId};
use std::collections::HashSet;

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

        pub(super) fn previous_for_focus_change(
                &self,
                next: &Self,
        ) -> Option<WindowId> {
                (self.active != next.active && self.members == next.members).then_some(self.active)
        }
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
