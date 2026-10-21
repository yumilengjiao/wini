//! Explicit viewport actions: center a column or visible group, and expand
//! into unused viewport space. Uses settled layout targets, so repeated input
//! during animation is deterministic; Transport animates the resulting view.

use super::geometry::{self, CenterFocused, LayoutParams};
use super::{ColumnWidth, Workspace};

struct VisibleGroup {
        left: f64,
        width: f64,
        count: usize,
}

fn visible_group(
        ws: &Workspace,
        params: &LayoutParams,
        view_width: f64,
) -> Option<VisibleGroup> {
        let widths = geometry::column_widths(ws, params, view_width);
        let xs = geometry::column_xs(&widths, params.gaps);
        let vp = geometry::target_view_pos(ws, params, view_width);
        let mut group = None;
        let mut active_visible = false;
        for (idx, (&x, &w)) in xs.iter().zip(&widths).enumerate() {
                if x < vp + params.gaps - 1e-6 {
                        continue;
                }
                if x + w + params.gaps > vp + view_width + 1e-6 {
                        break;
                }
                let value = group.get_or_insert(VisibleGroup {
                        left: x,
                        width: 0.0,
                        count: 0,
                });
                value.width = x + w - value.left;
                value.count += 1;
                active_visible |= idx == ws.active_column_idx;
        }
        if active_visible { group } else { None }
}

impl Workspace {
        fn set_explicit_view(
                &mut self,
                vp: f64,
                active_x: f64,
        ) -> bool {
                let offset = vp - active_x;
                let changed =
                        (self.view_offset - offset).abs() > 1e-6 || !self.explicit_view_position;
                self.view_offset = offset;
                self.explicit_view_position = true;
                self.pending_view_rebase = None;
                changed
        }

        pub fn center_active_column(
                &mut self,
                params: &LayoutParams,
                view_width: f64,
        ) -> bool {
                if self.columns.is_empty() {
                        return false;
                }
                let widths = geometry::column_widths(self, params, view_width);
                let xs = geometry::column_xs(&widths, params.gaps);
                let idx = self.active_column_idx;
                let offset = geometry::view_offset_centered(view_width, xs[idx], widths[idx]);
                self.set_explicit_view(xs[idx] + offset, xs[idx])
        }

        /// Center only fully visible columns; a clipped active column or
        /// always-centered policy makes this action a no-op, like niri.
        pub fn center_visible_columns(
                &mut self,
                params: &LayoutParams,
                view_width: f64,
        ) -> bool {
                if params.center_focused_column == CenterFocused::Always {
                        return false;
                }
                let Some(group) = visible_group(self, params, view_width) else {
                        return false;
                };
                let widths = geometry::column_widths(self, params, view_width);
                let xs = geometry::column_xs(&widths, params.gaps);
                let vp = group.left - (view_width - group.width) / 2.0;
                self.set_explicit_view(vp, xs[self.active_column_idx])
        }

        /// Grow the active column without pushing other fully visible columns
        /// out of view. A lone visible column uses reversible full-width mode.
        pub fn expand_column_to_available_width(
                &mut self,
                params: &LayoutParams,
                view_width: f64,
        ) -> bool {
                let Some(col) = self.columns.get(self.active_column_idx) else {
                        return false;
                };
                if col.is_full_width
                        || col.is_maximized
                        || col.is_edge_maximized
                        || self.fullscreen_id.is_some()
                {
                        return false;
                }
                if params.center_focused_column == CenterFocused::Always {
                        return self.toggle_full_width();
                }
                let Some(group) = visible_group(self, params, view_width) else {
                        return false;
                };
                let free = view_width - group.width - 2.0 * params.gaps;
                if free <= 1e-6 {
                        return false;
                }
                if group.count == 1 {
                        return self.toggle_full_width();
                }
                let widths = geometry::column_widths(self, params, view_width);
                let idx = self.active_column_idx;
                self.columns[idx].width = Some(ColumnWidth::Fixed(widths[idx] + free));
                let xs = geometry::column_xs(
                        &geometry::column_widths(self, params, view_width),
                        params.gaps,
                );
                self.set_explicit_view(group.left - params.gaps, xs[idx]);
                true
        }
}

#[cfg(test)]
mod tests {
        use super::super::Edge;
        use super::*;

        fn workspace(widths: &[f64]) -> Workspace {
                let mut ws = Workspace::new();
                for (i, &width) in widths.iter().enumerate() {
                        ws.add_window(i as isize + 1);
                        ws.columns[i].width = Some(ColumnWidth::Fixed(width));
                }
                ws.focus_column_edge(Edge::First);
                ws
        }

        fn params() -> LayoutParams {
                LayoutParams {
                        gaps: 10.0,
                        ..Default::default()
                }
        }

        #[test]
        fn center_group_preserves_spacing_and_focus_even_when_all_content_fits() {
                let mut ws = workspace(&[200.0, 300.0]);
                let p = params();
                geometry::refresh_view_offset(&mut ws, &p, 1000.0, None);
                assert!(ws.center_visible_columns(&p, 1000.0));
                assert_eq!(geometry::target_view_pos(&ws, &p, 1000.0), -245.0);
                let rs = geometry::compute_workspace_geometry(&ws, &p, (0.0, 0.0, 1000.0, 800.0));
                assert_eq!(rs[0].x, 245);
                assert_eq!(rs[1].x, 455);
                assert_eq!(ws.focused_id(), Some(1));
                assert!(!ws.center_visible_columns(&p, 1000.0));
                geometry::refresh_view_offset(&mut ws, &p, 1000.0, None);
                assert!(!ws.explicit_view_position);
        }

        #[test]
        fn center_single_column_is_not_overridden_by_content_fits_clamp() {
                let mut ws = workspace(&[400.0]);
                assert!(ws.center_active_column(&params(), 1000.0));
                assert_eq!(geometry::target_view_pos(&ws, &params(), 1000.0), -300.0);
        }

        #[test]
        fn clipped_active_column_does_not_rearrange_view() {
                let mut ws = workspace(&[600.0, 600.0]);
                ws.view_offset = 100.0;
                assert!(!ws.center_visible_columns(&params(), 1000.0));
                assert!(!ws.expand_column_to_available_width(&params(), 1000.0));
                assert_eq!(ws.view_offset, 100.0);
        }

        #[test]
        fn expand_preserves_other_visible_columns_and_both_edge_gaps() {
                let mut ws = workspace(&[200.0, 300.0, 900.0]);
                let p = params();
                ws.view_offset = -10.0;
                assert!(ws.expand_column_to_available_width(&p, 1000.0));
                assert_eq!(ws.columns[0].width, Some(ColumnWidth::Fixed(670.0)));
                let rs = geometry::compute_workspace_geometry(&ws, &p, (0.0, 0.0, 1000.0, 800.0));
                assert_eq!(rs[0].x, 10);
                assert_eq!(rs[1].x + rs[1].w, 990);
                assert_eq!(rs[1].w, 300);
                assert!(!ws.expand_column_to_available_width(&p, 1000.0));
        }

        #[test]
        fn lone_column_full_width_is_reversible() {
                let mut ws = workspace(&[400.0]);
                assert!(ws.expand_column_to_available_width(&params(), 1000.0));
                assert!(ws.columns[0].is_full_width);
                assert!(ws.toggle_full_width());
                assert_eq!(ws.columns[0].width, Some(ColumnWidth::Fixed(400.0)));
        }

        #[test]
        fn always_centered_policy_and_empty_workspace() {
                let p = LayoutParams {
                        center_focused_column: CenterFocused::Always,
                        ..params()
                };
                let mut ws = workspace(&[200.0, 300.0]);
                assert!(!ws.center_visible_columns(&p, 1000.0));
                assert!(ws.expand_column_to_available_width(&p, 1000.0));
                assert!(ws.columns[0].is_full_width);
                let mut empty = Workspace::new();
                assert!(!empty.center_visible_columns(&p, 1000.0));
                assert!(!empty.expand_column_to_available_width(&p, 1000.0));
        }
}
