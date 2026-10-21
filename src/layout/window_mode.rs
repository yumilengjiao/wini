//! Window-level sizing modes. Edge maximization is distinct from maximizing
//! a column inside the tiling area and from borderless monitor fullscreen.

use super::tabbed::ColumnDisplay;
use super::{DirH, Workspace};

impl Workspace {
        /// Niri: extract a stacked window before edge maximization; retain
        /// tabbed groups. Leaving this mode restores its column width/height.
        pub fn toggle_maximize_window_to_edges(&mut self) -> bool {
                let ci = self.active_column_idx;
                let Some(col) = self.columns.get(ci) else {
                        return false;
                };
                if col.is_edge_maximized {
                        self.columns[ci].is_edge_maximized = false;
                        return true;
                }
                if col.tiles.len() > 1 && col.display == ColumnDisplay::Normal {
                        self.consume_or_expel(DirH::Right);
                }
                let col = &mut self.columns[self.active_column_idx];
                col.is_edge_maximized = true;
                col.is_maximized = false;
                self.fullscreen_id = None;
                true
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::layout::{ColumnWidth, height::WindowHeight};

        #[test]
        fn edge_mode_extracts_focused_stack_window_and_retains_its_size_hints() {
                let mut ws = Workspace::new();
                ws.add_window(1);
                ws.add_window_to_column(2, 0, 1);
                ws.columns[0].width = Some(ColumnWidth::Fixed(500.0));
                ws.columns[0].tiles[1].height = Some(WindowHeight::Fixed(300.0));
                assert!(ws.toggle_maximize_window_to_edges());
                assert_eq!(ws.columns.len(), 2);
                assert_eq!(ws.focused_id(), Some(2));
                let col = &ws.columns[1];
                assert!(col.is_edge_maximized);
                assert_eq!(col.width, Some(ColumnWidth::Fixed(500.0)));
                assert_eq!(col.tiles[0].height, Some(WindowHeight::Fixed(300.0)));
                ws.toggle_maximize_window_to_edges();
                assert!(!ws.columns[1].is_edge_maximized);
                assert_eq!(ws.columns.len(), 2);
        }

        #[test]
        fn tabs_stay_grouped_and_fullscreen_is_mutually_exclusive() {
                let mut ws = Workspace::new();
                ws.add_window(1);
                ws.add_window_to_column(2, 0, 1);
                ws.toggle_column_tabbed_display();
                ws.fullscreen_id = Some(2);
                ws.toggle_maximize_window_to_edges();
                assert!(ws.fullscreen_id.is_none());
                assert_eq!(ws.columns.len(), 1);
                assert_eq!(ws.columns[0].display, ColumnDisplay::Tabbed);
                assert_eq!(ws.rendered_ids().collect::<Vec<_>>(), vec![2]);
                ws.toggle_fullscreen();
                assert!(!ws.columns[0].is_edge_maximized);
        }

        #[test]
        fn edge_maximized_tabs_ignore_indicator_reservation_and_struts() {
                let mut ws = Workspace::new();
                ws.add_window(1);
                ws.add_window_to_column(2, 0, 1);
                ws.toggle_column_tabbed_display();
                ws.toggle_maximize_window_to_edges();
                let mut p = crate::layout::geometry::LayoutParams::default();
                p.struts = crate::layout::geometry::Struts::uniform(20.0);
                p.tab_indicator.place_within_column = true;
                for side in [
                        crate::layout::tabbed::TabPosition::Left,
                        crate::layout::tabbed::TabPosition::Top,
                ] {
                        p.tab_indicator.position = side;
                        let rs = crate::layout::geometry::compute_workspace_geometry_in_work_area(
                                &ws,
                                &p,
                                (-1000.0, 48.0, 1000.0, 752.0),
                        );
                        assert_eq!(rs.len(), 1);
                        assert_eq!((rs[0].x, rs[0].y, rs[0].w, rs[0].h), (-1000, 48, 1000, 752));
                }
        }

        #[test]
        fn explicit_size_requests_leave_edge_mode() {
                let mut ws = Workspace::new();
                ws.add_window(1);
                let p = crate::layout::geometry::LayoutParams::default();
                ws.toggle_maximize_window_to_edges();
                ws.set_column_width(&super::super::SizeChange::Fixed(400.0), &p, 1000.0);
                assert!(!ws.columns[0].is_edge_maximized);
                ws.toggle_maximize_window_to_edges();
                ws.set_window_height(&super::super::SizeChange::Fixed(400.0), &p, 800.0);
                assert!(!ws.columns[0].is_edge_maximized);
        }
}
