//! Window-height requests and minimum-aware vertical allocation.
//!
//! Auto windows share remaining space by weight; fixed/preset heights use
//! physical pixels or a proportion of the tiling area's height. Explicitly
//! sizing a different tile converts old requests to auto while preserving
//! apparent heights, matching niri's one-sized-window-per-column behavior.

use super::geometry::LayoutParams;
use super::{Column, SizeChange, Workspace};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HeightPreset {
        Fixed(f64),
        Proportion(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowHeight {
        Fixed(f64),
        Preset(usize),
}

pub fn default_presets() -> Vec<HeightPreset> {
        vec![
                HeightPreset::Proportion(1.0 / 3.0),
                HeightPreset::Proportion(0.5),
                HeightPreset::Proportion(2.0 / 3.0),
        ]
}

fn resolve(
        preset: HeightPreset,
        area_height: f64,
) -> f64 {
        match preset {
                HeightPreset::Fixed(px) => px,
                HeightPreset::Proportion(p) => area_height * p,
        }
        .clamp(1.0, 100_000.0)
}

/// Allocate a column without overlaps or hiding min-size overflow. If the
/// sum of minimum heights cannot fit, the column extends below the work area.
pub fn allocate(
        col: &Column,
        params: &LayoutParams,
        area_height: f64,
) -> Vec<(f64, f64)> {
        let n = col.tiles.len();
        if n > 0 && col.display == super::tabbed::ColumnDisplay::Tabbed {
                let minimum = col
                        .tiles
                        .iter()
                        .map(|t| t.min_h.max(1.0))
                        .fold(1.0, f64::max);
                let request = col.tiles.iter().find_map(|t| match t.height {
                        Some(WindowHeight::Fixed(px)) => Some(px),
                        Some(WindowHeight::Preset(idx)) => params
                                .preset_window_heights
                                .get(idx)
                                .map(|&p| resolve(p, area_height)),
                        None => None,
                });
                let h = request
                        .unwrap_or(area_height)
                        .clamp(minimum, area_height.max(minimum));
                return vec![(0.0, h); n];
        }
        if n == 0 {
                return Vec::new();
        }
        let mins: Vec<f64> = col.tiles.iter().map(|t| t.min_h.max(1.0)).collect();
        let budget = (area_height - params.gaps * (n - 1) as f64).max(mins.iter().sum());
        let requests: Vec<Option<f64>> = col
                .tiles
                .iter()
                .map(|tile| match tile.height {
                        Some(WindowHeight::Fixed(px)) => Some(px),
                        Some(WindowHeight::Preset(idx)) => params
                                .preset_window_heights
                                .get(idx)
                                .map(|&preset| resolve(preset, area_height)),
                        None => None,
                })
                .collect();
        let mut heights = mins.clone();
        let mut remaining = budget - mins.iter().sum::<f64>();
        // Usually only one request exists, but tile merges may bring several.
        // Allocate their desired excess proportionally when they cannot fit.
        let excess: Vec<f64> = requests
                .iter()
                .zip(&mins)
                .map(|(request, min)| request.map(|r| (r - min).max(0.0)).unwrap_or(0.0))
                .collect();
        let total_excess: f64 = excess.iter().sum();
        let scale = if total_excess > 0.0 {
                (remaining / total_excess).min(1.0)
        } else {
                0.0
        };
        for i in 0..n {
                let extra = excess[i] * scale;
                heights[i] += extra;
                remaining -= extra;
        }
        // Weighted water filling: reserve clamped minima then redistribute
        // their unused shares. Never let a min-size tile overlap its neighbor.
        let mut auto: Vec<usize> = (0..n).filter(|&i| requests[i].is_none()).collect();
        let mut auto_budget = remaining + auto.iter().map(|&i| mins[i]).sum::<f64>();
        while !auto.is_empty() {
                let weights: f64 = auto
                        .iter()
                        .map(|&i| col.tiles[i].height_weight.max(0.0001))
                        .sum();
                let constrained: Vec<usize> = auto
                        .iter()
                        .copied()
                        .filter(|&i| {
                                auto_budget * col.tiles[i].height_weight.max(0.0001) / weights
                                        < mins[i]
                        })
                        .collect();
                if constrained.is_empty() {
                        for &i in &auto {
                                heights[i] = auto_budget * col.tiles[i].height_weight.max(0.0001)
                                        / weights;
                        }
                        break;
                }
                for &i in &constrained {
                        heights[i] = mins[i];
                        auto_budget -= mins[i];
                }
                auto.retain(|i| !constrained.contains(i));
        }
        let mut y = 0.0;
        heights.into_iter()
                .map(|h| {
                        let out = (y, h);
                        y += h + params.gaps;
                        out
                })
                .collect()
}

impl Workspace {
        fn prepare_height_change(
                &mut self,
                params: &LayoutParams,
                area_height: f64,
        ) -> Option<(usize, usize, f64)> {
                let ci = self.active_column_idx;
                let col = self.columns.get(ci)?;
                let ti = col.active_tile_idx;
                let heights = allocate(col, params, area_height);
                let current = match col.tiles.get(ti)?.height {
                        Some(WindowHeight::Fixed(px)) if !col.is_maximized => px,
                        _ => heights.get(ti)?.1,
                };
                // Selecting another auto-height window releases previous sizing.
                if col.tiles[ti].height.is_none() {
                        let mut sorted: Vec<f64> = heights.iter().map(|&(_, h)| h).collect();
                        sorted.sort_by(f64::total_cmp);
                        let median = sorted[sorted.len() / 2].max(1.0);
                        for (tile, &(_, h)) in self.columns[ci].tiles.iter_mut().zip(&heights) {
                                tile.height = None;
                                tile.height_weight = h / median;
                        }
                }
                Some((ci, ti, current))
        }

        pub fn set_window_height(
                &mut self,
                spec: &SizeChange,
                params: &LayoutParams,
                area_height: f64,
        ) -> bool {
                let Some((ci, ti, current)) = self.prepare_height_change(params, area_height)
                else {
                        return false;
                };
                let requested = match *spec {
                        SizeChange::Fixed(px) => px,
                        SizeChange::Delta(px) => current + px,
                        SizeChange::Proportion(p) => area_height * p,
                        SizeChange::ProportionDelta(p) => current + area_height * p,
                };
                let col = &mut self.columns[ci];
                let min_others: f64 = if col.display == super::tabbed::ColumnDisplay::Tabbed {
                        0.0
                } else {
                        col.tiles
                                .iter()
                                .enumerate()
                                .filter(|&(i, _)| i != ti)
                                .map(|(_, t)| t.min_h.max(1.0) + params.gaps)
                                .sum()
                };
                let content_h = if col.display == super::tabbed::ColumnDisplay::Tabbed {
                        params.tab_indicator
                                .content_rect(col.tiles.len(), (0.0, 0.0, 1.0, area_height))
                                .3
                } else {
                        area_height
                };
                let max = (content_h - min_others).max(col.tiles[ti].min_h.max(1.0));
                let height = requested
                        .clamp(col.tiles[ti].min_h.max(1.0), max)
                        .min(100_000.0);
                if col.display == super::tabbed::ColumnDisplay::Tabbed {
                        for t in &mut col.tiles {
                                t.height = None;
                        }
                }
                col.tiles[ti].height = Some(WindowHeight::Fixed(height));
                col.is_maximized = false;
                true
        }

        pub fn cycle_window_height(
                &mut self,
                params: &LayoutParams,
                area_height: f64,
                forwards: bool,
        ) -> bool {
                if params.preset_window_heights.is_empty() {
                        return false;
                }
                let Some((ci, ti, _)) = self.prepare_height_change(params, area_height) else {
                        return false;
                };
                let col = &mut self.columns[ci];
                let current = allocate(col, params, area_height)[ti].1;
                let presets = &params.preset_window_heights;
                let next = match col.tiles[ti].height {
                        Some(WindowHeight::Preset(idx))
                                if idx < presets.len() && !col.is_maximized =>
                        {
                                (idx + if forwards { 1 } else { presets.len() - 1 }) % presets.len()
                        },
                        _ if forwards => presets
                                .iter()
                                .position(|&p| resolve(p, area_height) > current + 1.0)
                                .unwrap_or(0),
                        _ => presets
                                .iter()
                                .rposition(|&p| resolve(p, area_height) < current - 1.0)
                                .unwrap_or(presets.len() - 1),
                };
                if col.display == super::tabbed::ColumnDisplay::Tabbed {
                        for t in &mut col.tiles {
                                t.height = None;
                        }
                }
                col.tiles[ti].height = Some(WindowHeight::Preset(next));
                col.is_maximized = false;
                true
        }

        pub fn reset_window_height(&mut self) -> bool {
                let Some(col) = self.columns.get_mut(self.active_column_idx) else {
                        return false;
                };
                if col.display == super::tabbed::ColumnDisplay::Tabbed {
                        let changed = col
                                .tiles
                                .iter()
                                .any(|t| t.height.is_some() || t.height_weight != 1.0);
                        for t in &mut col.tiles {
                                t.height = None;
                                t.height_weight = 1.0;
                        }
                        return changed;
                }
                let Some(tile) = col.tiles.get_mut(col.active_tile_idx) else {
                        return false;
                };
                let changed = tile.height.is_some() || tile.height_weight != 1.0;
                tile.height = None;
                tile.height_weight = 1.0;
                changed
        }
}

#[cfg(test)]
mod tests {
        use super::super::{DirV, WindowId};
        use super::*;
        fn stacked(count: usize) -> Workspace {
                let mut ws = Workspace::new();
                ws.add_window(1);
                for id in 2..=count {
                        ws.columns[0]
                                .tiles
                                .push(super::super::Tile::new(id as WindowId));
                }
                ws
        }
        fn params() -> LayoutParams {
                LayoutParams {
                        gaps: 10.0,
                        ..Default::default()
                }
        }
        fn heights(
                ws: &Workspace,
                p: &LayoutParams,
        ) -> Vec<f64> {
                allocate(&ws.columns[0], p, 1000.0)
                        .iter()
                        .map(|&(_, h)| h)
                        .collect()
        }

        #[test]
        fn min_size_allocation_uses_remaining_height_without_wasting_space() {
                let mut ws = stacked(3);
                let p = params();
                ws.columns[0].tiles[0].min_h = 500.0;
                assert_eq!(heights(&ws, &p), vec![500.0, 240.0, 240.0]);
                for count in 1..=8 {
                        let mut ws = stacked(count);
                        for i in 0..count {
                                ws.columns[0].tiles[i].min_h = 10.0 + 17.0 * i as f64;
                                ws.columns[0].tiles[i].height_weight = (i + 1) as f64;
                        }
                        let rs = allocate(&ws.columns[0], &p, 1000.0);
                        for i in 0..count {
                                assert!(rs[i].1 + 1e-6 >= ws.columns[0].tiles[i].min_h);
                                if i > 0 {
                                        assert!((rs[i].0 - rs[i - 1].0 - rs[i - 1].1 - p.gaps)
                                                .abs()
                                                < 1e-6);
                                }
                        }
                        let bottom = rs.last().unwrap().0 + rs.last().unwrap().1;
                        assert!((bottom - 1000.0).abs() < 1e-6);
                }
        }

        #[test]
        fn fixed_height_is_pixels_and_auto_siblings_get_remaining_space() {
                let mut ws = stacked(3);
                let p = params();
                ws.set_window_height(&SizeChange::Fixed(500.0), &p, 1000.0);
                assert_eq!(heights(&ws, &p), vec![500.0, 240.0, 240.0]);
                ws.set_window_height(&SizeChange::Delta(-100.0), &p, 1000.0);
                assert_eq!(heights(&ws, &p), vec![400.0, 290.0, 290.0]);
                ws.set_window_height(&SizeChange::ProportionDelta(0.1), &p, 1000.0);
                assert_eq!(heights(&ws, &p), vec![500.0, 240.0, 240.0]);
        }

        #[test]
        fn minimum_sizes_redistribute_and_overflow_without_overlaps() {
                let mut ws = stacked(3);
                let p = params();
                ws.columns[0].tiles[1].min_h = 500.0;
                ws.set_window_height(&SizeChange::Fixed(900.0), &p, 1000.0);
                assert_eq!(heights(&ws, &p), vec![479.0, 500.0, 1.0]);
                for tile in &mut ws.columns[0].tiles {
                        tile.min_h = 600.0;
                }
                let rs = allocate(&ws.columns[0], &p, 1000.0);
                assert_eq!(rs, vec![(0.0, 600.0), (610.0, 600.0), (1220.0, 600.0)]);
        }

        #[test]
        fn preset_cycle_forward_backward_and_reset() {
                let mut ws = stacked(2);
                let p = params();
                assert!(ws.cycle_window_height(&p, 1000.0, true));
                assert_eq!(ws.columns[0].tiles[0].height, Some(WindowHeight::Preset(1)));
                ws.cycle_window_height(&p, 1000.0, true);
                assert_eq!(ws.columns[0].tiles[0].height, Some(WindowHeight::Preset(2)));
                ws.cycle_window_height(&p, 1000.0, false);
                assert_eq!(ws.columns[0].tiles[0].height, Some(WindowHeight::Preset(1)));
                assert!(ws.reset_window_height());
                assert_eq!(heights(&ws, &p), vec![495.0, 495.0]);
                assert!(!ws.reset_window_height());
        }

        #[test]
        fn moving_sizing_to_another_tile_preserves_auto_weights() {
                let mut ws = stacked(3);
                let p = params();
                ws.set_window_height(&SizeChange::Fixed(500.0), &p, 1000.0);
                ws.focus_tile(DirV::Down);
                ws.set_window_height(&SizeChange::Fixed(240.0), &p, 1000.0);
                assert!(ws.columns[0].tiles[0].height.is_none());
                assert_eq!(heights(&ws, &p), vec![500.0, 240.0, 240.0]);
        }

        #[test]
        fn single_window_can_be_shorter_and_reset_to_full_height() {
                let mut ws = stacked(1);
                let p = params();
                ws.set_window_height(&SizeChange::Proportion(0.5), &p, 1000.0);
                assert_eq!(heights(&ws, &p), vec![500.0]);
                ws.reset_window_height();
                assert_eq!(heights(&ws, &p), vec![1000.0]);
        }

        #[test]
        fn preset_reload_resolves_new_values_and_missing_index_falls_back_to_auto() {
                let mut ws = stacked(2);
                let mut p = params();
                p.preset_window_heights = vec![HeightPreset::Fixed(250.0)];
                ws.cycle_window_height(&p, 1000.0, true);
                assert_eq!(heights(&ws, &p), vec![250.0, 740.0]);
                p.preset_window_heights[0] = HeightPreset::Fixed(300.0);
                assert_eq!(heights(&ws, &p), vec![300.0, 690.0]);
                p.preset_window_heights.clear();
                assert_eq!(heights(&ws, &p), vec![495.0, 495.0]);
                assert!(!ws.cycle_window_height(&p, 1000.0, true));
                assert!(!Workspace::new().set_window_height(&SizeChange::Fixed(100.0), &p, 1000.0));
        }
}
