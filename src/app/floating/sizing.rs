//! Niri-like floating sizing, independent of HWND submission. A float remembers
//! width/height presets separately; percentages refer to its output's working
//! area, not its own current size and not the tiled column's gap budget.

use super::FloatState;
use crate::config::Action;
use crate::layout::geometry::LayoutParams;
use crate::layout::height::HeightPreset;
use crate::layout::{ColumnWidth, SizeChange};

type Rect = (f64, f64, f64, f64);

impl FloatState {
        /// Geometry-only actions; the caller retains focus, ownership and the
        /// floating layer and submits the result through the normal reflow.
        pub(in crate::app) fn apply_geometry_action(
                &mut self,
                action: &Action,
                params: &LayoutParams,
                work: Rect,
                minimum: (f64, f64),
        ) -> bool {
                let before = (self.x, self.y, self.w, self.h);
                let area = params.tiling_area(work);
                let cycle_width = |state: &mut Self, forwards| {
                        let sizes: Vec<_> = params
                                .preset_column_widths
                                .iter()
                                .map(|preset| match *preset {
                                        ColumnWidth::Fixed(px) => bounded(px, 1.0),
                                        ColumnWidth::Proportion(p) => bounded(area.2 * p, 1.0),
                                })
                                .filter(|size| size.is_finite())
                                .collect();
                        if let Some(w) = cycle(state.w, &mut state.preset_width, &sizes, forwards) {
                                state.w = bounded(w, minimum.0);
                        }
                };
                match action {
                        Action::MoveColumnLeft => self.x -= 50.0,
                        Action::MoveColumnRight => self.x += 50.0,
                        Action::MoveWindowUp => self.y -= 50.0,
                        Action::MoveWindowDown => self.y += 50.0,
                        Action::SwitchPresetColumnWidth => cycle_width(self, true),
                        Action::SwitchPresetColumnWidthBack => cycle_width(self, false),
                        Action::SetColumnWidth(spec) if spec.trim().is_empty() => {
                                cycle_width(self, true)
                        },
                        Action::SwitchPresetWindowHeight | Action::SwitchPresetWindowHeightBack => {
                                let sizes: Vec<_> = params
                                        .preset_window_heights
                                        .iter()
                                        .map(|preset| match *preset {
                                                HeightPreset::Fixed(px) => bounded(px, 1.0),
                                                HeightPreset::Proportion(p) => {
                                                        bounded(area.3 * p, 1.0)
                                                },
                                        })
                                        .filter(|size| size.is_finite())
                                        .collect();
                                let forwards = matches!(action, Action::SwitchPresetWindowHeight);
                                if let Some(h) =
                                        cycle(self.h, &mut self.preset_height, &sizes, forwards)
                                {
                                        self.h = bounded(h, minimum.1);
                                }
                        },
                        Action::SetColumnWidth(spec) => {
                                let Some(w) = resolve(spec, self.w, area.2, minimum.0) else {
                                        return false;
                                };
                                self.w = w;
                                self.preset_width = None;
                        },
                        Action::SetWindowHeight(spec) => {
                                let Some(h) = resolve(spec, self.h, area.3, minimum.1) else {
                                        return false;
                                };
                                self.h = h;
                                self.preset_height = None;
                        },
                        // Like niri, reset-window-height only resets tiled
                        // auto-height sharing. It does not resize a float.
                        _ => return false,
                }
                if before != (self.x, self.y, self.w, self.h) {
                        self.x = keep_visible(self.x, self.w, area.0, area.2);
                        self.y = keep_visible(self.y, self.h, area.1, area.3);
                        return true;
                }
                false
        }

        /// A move alone preserves both presets; a manual resize invalidates
        /// only the changed axis. The next cycle searches from the actual size.
        pub(in crate::app) fn adopt_interactive_rect(
                &mut self,
                rect: Rect,
        ) {
                if (self.w - rect.2).abs() >= 0.5 {
                        self.preset_width = None;
                }
                if (self.h - rect.3).abs() >= 0.5 {
                        self.preset_height = None;
                }
                (self.x, self.y, self.w, self.h) = rect;
        }

        /// Reloading presets resets the corresponding cursor, without resizing
        /// the window or retaining an index into a different list.
        pub(in crate::app) fn update_sizing_config(
                &mut self,
                old: &LayoutParams,
                new: &LayoutParams,
        ) {
                if old.preset_column_widths != new.preset_column_widths {
                        self.preset_width = None;
                }
                if old.preset_window_heights != new.preset_window_heights {
                        self.preset_height = None;
                }
        }
}

fn bounded(
        size: f64,
        minimum: f64,
) -> f64 {
        size.round()
                .clamp(minimum.ceil().clamp(1.0, 100_000.0), 100_000.0)
}

fn resolve(
        spec: &str,
        current: f64,
        available: f64,
        minimum: f64,
) -> Option<f64> {
        let change = SizeChange::parse(spec)?;
        let value = match change {
                SizeChange::Fixed(v)
                | SizeChange::Delta(v)
                | SizeChange::Proportion(v)
                | SizeChange::ProportionDelta(v) => v,
        };
        if !value.is_finite() {
                return None;
        }
        let size = match change {
                SizeChange::Fixed(px) => px,
                SizeChange::Delta(px) => current + px,
                SizeChange::Proportion(p) => available * p,
                SizeChange::ProportionDelta(p) => current + available * p,
        };
        Some(bounded(size, minimum))
}

fn cycle(
        current: f64,
        cursor: &mut Option<usize>,
        sizes: &[f64],
        forwards: bool,
) -> Option<f64> {
        if sizes.is_empty() {
                return None;
        }
        let next = match cursor.filter(|&idx| idx < sizes.len()) {
                Some(idx) => (idx + if forwards { 1 } else { sizes.len() - 1 }) % sizes.len(),
                None if forwards => sizes.iter().position(|&s| current + 1.0 < s).unwrap_or(0),
                None => sizes
                        .iter()
                        .rposition(|&s| s + 1.0 < current)
                        .unwrap_or(sizes.len() - 1),
        };
        *cursor = Some(next);
        Some(sizes[next])
}

fn keep_visible(
        position: f64,
        size: f64,
        origin: f64,
        extent: f64,
) -> f64 {
        // Preserve the existing 100px grab area, but never form reversed clamp
        // bounds for a tiny float, tiny output or oversized struts.
        let visible = 100.0_f64.min(size).min(extent);
        position.clamp(origin - size + visible, origin + extent - visible)
}
