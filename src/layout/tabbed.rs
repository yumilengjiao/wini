//! Tabbed-column state and indicator geometry, independent of Win32.

use super::{Column, WindowId, Workspace};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ColumnDisplay {
        #[default]
        Normal,
        Tabbed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TabPosition {
        #[default]
        Left,
        Right,
        Top,
        Bottom,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabIndicatorConfig {
        pub enabled: bool,
        pub hide_when_single_tab: bool,
        pub place_within_column: bool,
        pub position: TabPosition,
        pub gap: f64,
        pub width: f64,
        pub total_proportion: f64,
        pub gaps_between_tabs: f64,
        pub radius: i32,
        pub active_color: u32,
        pub inactive_color: u32,
}

impl Default for TabIndicatorConfig {
        fn default() -> Self {
                Self {
                        enabled: true,
                        hide_when_single_tab: false,
                        place_within_column: false,
                        position: TabPosition::Left,
                        gap: 5.0,
                        width: 4.0,
                        total_proportion: 0.5,
                        gaps_between_tabs: 0.0,
                        radius: 0,
                        active_color: 0x7daea3,
                        inactive_color: 0x505050,
                }
        }
}

impl Column {
        pub fn rendered_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
                self.tiles
                        .iter()
                        .enumerate()
                        .filter(move |(i, _)| {
                                (self.display != ColumnDisplay::Tabbed && !self.is_maximized)
                                        || *i == self.active_tile_idx
                        })
                        .map(|(_, t)| t.id)
        }
}

impl Workspace {
        pub fn rendered_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
                self.columns.iter().flat_map(Column::rendered_ids)
        }

        /// Remove the active column while preserving its complete state.
        pub fn take_active_column(&mut self) -> Option<Column> {
                let ci = self.active_column_idx;
                let col = self.columns.get(ci)?.clone();
                if self.fullscreen_id
                        .is_some_and(|id| col.tiles.iter().any(|t| t.id == id))
                {
                        self.fullscreen_id = None;
                }
                for tile in &col.tiles {
                        self.remove_window(tile.id);
                }
                Some(col)
        }

        /// Add a whole column without discarding tab mode, sizes or selection.
        pub fn insert_preserved_column(
                &mut self,
                col: Column,
        ) {
                let idx = if self.columns.is_empty() {
                        0
                } else {
                        self.active_column_idx + 1
                };
                self.rebase_view_to_current();
                self.columns.insert(idx, col);
                self.active_column_idx = idx;
                self.activate_prev_column_on_removal = None;
        }

        pub fn active_column_is_tabbed(&self) -> bool {
                self.columns
                        .get(self.active_column_idx)
                        .is_some_and(|col| col.display == ColumnDisplay::Tabbed)
        }

        pub fn toggle_column_tabbed_display(&mut self) -> bool {
                let Some(col) = self.columns.get_mut(self.active_column_idx) else {
                        return false;
                };
                col.display = if col.display == ColumnDisplay::Normal {
                        ColumnDisplay::Tabbed
                } else {
                        ColumnDisplay::Normal
                };
                if col.display == ColumnDisplay::Normal && col.tiles.len() > 1 {
                        if col.is_maximized {
                                col.is_maximized = false;
                                col.is_full_width = false;
                        }
                        if self.fullscreen_id
                                .is_some_and(|id| col.tiles.iter().any(|t| t.id == id))
                        {
                                self.fullscreen_id = None;
                        }
                }
                true
        }
}

impl TabIndicatorConfig {
        pub fn visible(
                &self,
                count: usize,
        ) -> bool {
                self.enabled && count > 0 && (!self.hide_when_single_tab || count > 1)
        }

        /// Reserve indicator space only when requested. Keep at least 1px
        /// for content, even with oversized configuration values.
        pub fn content_rect(
                &self,
                count: usize,
                rect: (f64, f64, f64, f64),
        ) -> (f64, f64, f64, f64) {
                if !self.visible(count) || !self.place_within_column {
                        return rect;
                }
                let (mut x, mut y, mut w, mut h) = rect;
                let reserve = (self.width + self.gap).max(0.0);
                match self.position {
                        TabPosition::Left | TabPosition::Right => {
                                let r = reserve.min((w - 1.0).max(0.0));
                                if self.position == TabPosition::Left {
                                        x += r;
                                }
                                w -= r;
                        },
                        TabPosition::Top | TabPosition::Bottom => {
                                let r = reserve.min((h - 1.0).max(0.0));
                                if self.position == TabPosition::Top {
                                        y += r;
                                }
                                h -= r;
                        },
                }
                (x, y, w, h)
        }

        pub fn selection_at(
                &self,
                area: (i32, i32, i32, i32),
                ids: &[WindowId],
                zoom: f64,
                x: i32,
                y: i32,
        ) -> Option<WindowId> {
                ids.iter()
                        .copied()
                        .zip(self.rects(area, ids.len(), zoom))
                        .find(|(_, (l, t, w, h))| x >= *l && x < l + w && y >= *t && y < t + h)
                        .map(|(id, _)| id)
        }

        pub fn rects(
                &self,
                area: (i32, i32, i32, i32),
                count: usize,
                zoom: f64,
        ) -> Vec<(i32, i32, i32, i32)> {
                if !self.visible(count) {
                        return Vec::new();
                }
                let (x, y, w, h) = area;
                let vertical = matches!(self.position, TabPosition::Left | TabPosition::Right);
                let side = if vertical { h } else { w };
                let thickness = (self.width * zoom).round().max(1.0) as i32;
                let gap = (self.gap * zoom).round() as i32;
                let between = (self.gaps_between_tabs * zoom).round().max(0.0) as i32;
                let length = (side as f64 * self.total_proportion)
                        .round()
                        .max(count as f64) as i32;
                let span = (length - between * (count as i32 - 1)).max(count as i32);
                let actual = span + between * (count as i32 - 1);
                let mut offset = (side - actual) / 2;
                (0..count)
                        .map(|i| {
                                let size = span / count as i32
                                        + i32::from((i as i32) < span % count as i32);
                                let rect = match self.position {
                                        TabPosition::Left => {
                                                (x - gap - thickness, y + offset, thickness, size)
                                        },
                                        TabPosition::Right => {
                                                (x + w + gap, y + offset, thickness, size)
                                        },
                                        TabPosition::Top => {
                                                (x + offset, y - gap - thickness, size, thickness)
                                        },
                                        TabPosition::Bottom => {
                                                (x + offset, y + h + gap, size, thickness)
                                        },
                                };
                                offset += size + between;
                                rect
                        })
                        .collect()
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::layout::{DirV, height::WindowHeight};
        fn stacked() -> Workspace {
                let mut ws = Workspace::new();
                ws.add_window(1);
                ws.add_window_to_column(2, 0, 1);
                ws
        }
        #[test]
        fn toggle_select_remove_restore_preserves_tiles_and_height_requests() {
                let mut ws = stacked();
                ws.columns[0].tiles[0].height = Some(WindowHeight::Fixed(200.0));
                assert!(ws.toggle_column_tabbed_display());
                assert_eq!(ws.rendered_ids().collect::<Vec<_>>(), vec![2]);
                ws.focus_tile(DirV::Up);
                assert_eq!(ws.rendered_ids().collect::<Vec<_>>(), vec![1]);
                ws.remove_window(1);
                assert_eq!(ws.rendered_ids().collect::<Vec<_>>(), vec![2]);
                ws.toggle_column_tabbed_display();
                assert_eq!(ws.columns[0].display, ColumnDisplay::Normal);
        }
        #[test]
        fn tabbed_geometry_uses_active_tab_and_retains_shared_size() {
                let mut ws = stacked();
                ws.toggle_column_tabbed_display();
                let mut p = crate::layout::geometry::LayoutParams::default();
                p.gaps = 10.0;
                let area = (-1000.0, 12.0, 1000.0, 782.0);
                let rs = crate::layout::geometry::compute_workspace_geometry(&ws, &p, area);
                assert_eq!(rs.len(), 1);
                assert_eq!((rs[0].id, rs[0].y, rs[0].h), (2, 12, 782));
                ws.focus_tile(DirV::Up);
                let rs = crate::layout::geometry::compute_workspace_geometry(&ws, &p, area);
                assert_eq!((rs[0].id, rs[0].y, rs[0].h), (1, 12, 782));
                ws.set_window_height(&crate::layout::SizeChange::Fixed(500.0), &p, 782.0);
                assert_eq!(
                        crate::layout::height::allocate(&ws.columns[0], &p, 782.0),
                        vec![(0.0, 500.0); 2]
                );
                ws.focus_tile(DirV::Down);
                ws.set_window_height(&crate::layout::SizeChange::Fixed(400.0), &p, 782.0);
                assert_eq!(
                        crate::layout::height::allocate(&ws.columns[0], &p, 782.0),
                        vec![(0.0, 400.0); 2]
                );
                ws.reset_window_height();
                assert!(ws.columns[0].tiles.iter().all(|t| t.height.is_none()));
                p.tab_indicator.place_within_column = true;
                p.tab_indicator.position = TabPosition::Top;
                let rs = crate::layout::geometry::compute_workspace_geometry(&ws, &p, area);
                assert_eq!((rs[0].y, rs[0].h), (21, 773));
        }

        #[test]
        fn moving_entire_column_preserves_tab_mode_selection_and_heights() {
                let mut layout = crate::layout::Layout::new();
                layout.add_window("D", 1);
                layout.add_window("D", 2);
                layout.add_monitor("E");
                let ws = layout.monitor_mut("D").unwrap().active_workspace_mut();
                ws.consume_or_expel(crate::layout::DirH::Left);
                ws.toggle_column_tabbed_display();
                ws.columns[0].tiles[0].height = Some(WindowHeight::Fixed(300.0));
                let selected = ws.focused_id();
                layout.monitor_mut("D")
                        .unwrap()
                        .move_focused_column_to_workspace(1, true);
                let ws = layout.monitor("D").unwrap().active_workspace();
                assert_eq!(ws.columns[0].display, ColumnDisplay::Tabbed);
                assert_eq!(ws.focused_id(), selected);
                layout.move_focused_column_to_monitor("D", "E");
                let ws = layout.monitor("E").unwrap().active_workspace();
                assert_eq!(ws.columns[0].display, ColumnDisplay::Tabbed);
                assert_eq!(ws.focused_id(), selected);
                assert_eq!(
                        ws.columns[0].tiles[0].height,
                        Some(WindowHeight::Fixed(300.0))
                );
        }

        #[test]
        fn returning_to_stacked_mode_retains_nonmaximized_full_width() {
                let mut ws = stacked();
                ws.columns[0].is_full_width = true;
                ws.toggle_column_tabbed_display();
                ws.toggle_column_tabbed_display();
                assert!(ws.columns[0].is_full_width);
                ws.toggle_column_tabbed_display();
                ws.columns[0].is_maximized = true;
                ws.fullscreen_id = Some(2);
                ws.toggle_column_tabbed_display();
                assert!(!ws.columns[0].is_maximized);
                assert!(ws.fullscreen_id.is_none());
        }

        #[test]
        fn indicator_click_selects_the_displayed_order_not_hidden_hwnd_order() {
                let cfg = TabIndicatorConfig::default();
                let ids = [50, 10, 30];
                let rects = cfg.rects((100, 100, 800, 600), ids.len(), 1.0);
                for (&id, &(x, y, w, h)) in ids.iter().zip(&rects) {
                        assert_eq!(
                                cfg.selection_at(
                                        (100, 100, 800, 600),
                                        &ids,
                                        1.0,
                                        x + w / 2,
                                        y + h / 2
                                ),
                                Some(id)
                        );
                }
                assert_eq!(
                        cfg.selection_at((100, 100, 800, 600), &ids, 1.0, 400, 400),
                        None
                );
        }

        #[test]
        fn indicator_positions_and_zoom_are_consistent() {
                let mut cfg = TabIndicatorConfig::default();
                for position in [
                        TabPosition::Left,
                        TabPosition::Right,
                        TabPosition::Top,
                        TabPosition::Bottom,
                ] {
                        cfg.position = position;
                        let rects = cfg.rects((-100, 50, 800, 600), 3, 1.0);
                        assert_eq!(rects.len(), 3);
                        assert!(rects.iter().all(|r| r.2 > 0 && r.3 > 0));
                        assert_eq!(cfg.rects((-100, 50, 400, 300), 3, 0.5).len(), 3);
                }
                cfg.hide_when_single_tab = true;
                assert!(cfg.rects((0, 0, 100, 100), 1, 1.0).is_empty());
                cfg.place_within_column = true;
                cfg.width = 1000.0;
                assert!(cfg.content_rect(2, (0.0, 0.0, 50.0, 50.0)).3 >= 1.0);
        }
}
