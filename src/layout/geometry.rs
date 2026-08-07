//! Geometry computation for the scrollable layout: turns the structural
//! model (columns/tiles/focus indices) into pixel rectangles.
//!
//! Coordinate systems:
//! - *Column space*: x runs left-to-right starting at 0 (the left edge of
//!   the monitor's padded content area). Column `i` occupies
//!   `[col_x[i], col_x[i] + width[i])`.
//! - *View*: the visible horizontal slice of column space. `view_pos` is
//!   the column-space x at the left edge of the content area. It equals
//!   `col_x[active] + view_offset` — the model stores the offset relative
//!   to the active column so that structural edits keep the view stable.
//!
//! The view-visibility algorithm is a port of niri's
//! `compute_new_view_offset` (see niri/src/layout/scrolling.rs): keep the
//! focused column visible with up to `gaps` padding, preferring the
//! scroll direction with less motion; center it when the source and
//! target columns can't fit together (OnOverflow mode).

use super::{ColumnWidth, WindowId, Workspace};

/// Whether / when the focused column gets centered in the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenterFocused {
    Never,
    OnOverflow,
    Always,
}

/// Layout tuning parameters (will be driven by config later).
#[derive(Debug, Clone)]
pub struct LayoutParams {
    /// Gap between columns and between tiles, in pixels.
    pub gaps: f64,
    /// Padding between the monitor work area and the content area.
    pub edge_padding: f64,
    /// Width for columns that don't specify one.
    pub default_column_width: ColumnWidth,
    /// Preset column widths cycled through by a bare `set-column-width`
    /// (niri's preset-column-widths).
    pub preset_column_widths: Vec<ColumnWidth>,
    pub center_focused_column: CenterFocused,
}

impl Default for LayoutParams {
    fn default() -> Self {
        // Niri defaults: gaps 8, default column width half the view
        // (niri's default config: default-column-width { proportion 0.5 }),
        // center focused column on overflow.
        LayoutParams {
            gaps: 8.0,
            edge_padding: 8.0,
            default_column_width: ColumnWidth::Proportion(0.5),
            // Niri's default presets (1/3, 1/2, 2/3 of the output) for
            // switch-preset-column-width (Mod+R).
            preset_column_widths: vec![
                ColumnWidth::Proportion(0.33333),
                ColumnWidth::Proportion(0.5),
                ColumnWidth::Proportion(0.66667),
            ],
            center_focused_column: CenterFocused::OnOverflow,
        }
    }
}

/// A computed rectangle for one window, in absolute screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRect {
    pub id: WindowId,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Resolve a column's pixel width.
pub fn resolve_width(
    width: Option<ColumnWidth>,
    params: &LayoutParams,
    view_width: f64,
) -> f64 {
    match width.unwrap_or(params.default_column_width) {
        ColumnWidth::Proportion(p) => (p * view_width).max(1.0),
        ColumnWidth::Fixed(f) => f.max(1.0),
    }
}

/// Pixel width of each column.
pub fn column_widths(ws: &Workspace, params: &LayoutParams, view_width: f64) -> Vec<f64> {
    ws.columns
        .iter()
        .map(|c| {
            // Never narrower than the largest minimum of the column's
            // windows: apps clamp SetWindowPos to their minimum track
            // size, which would visually overlap the next column.
            let min_w = c.tiles.iter().map(|t| t.min_w).fold(0.0, f64::max);
            if c.is_maximized {
                // A maximized column occupies a full-work-area slot in
                // scroll space (content area + both edge paddings), so
                // the view can scroll to and away from it like any
                // other column (niri semantics).
                (view_width + params.edge_padding * 2.0).max(min_w)
            } else if c.is_full_width {
                view_width.max(min_w)
            } else {
                resolve_width(c.width, params, view_width).max(min_w)
            }
        })
        .collect()
}

/// Column-space x of the left edge of every column.
pub fn column_xs(widths: &[f64], gaps: f64) -> Vec<f64> {
    let mut x = 0.0;
    widths
        .iter()
        .map(|&w| {
            let rv = x;
            x += w + gaps;
            rv
        })
        .collect()
}

/// The current view position (column-space x at the content-area's left
/// edge).
pub fn view_pos(ws: &Workspace, xs: &[f64]) -> f64 {
    let active = xs.get(ws.active_column_idx).copied().unwrap_or(0.0);
    active + ws.view_offset
}

/// View offset that makes `(col_x, col_width)` visible, ported from
/// niri's `compute_new_view_offset`. `cur_vp` is the current view
/// position; the returned value is relative to the target column.
pub fn view_offset_to_show(
    cur_vp: f64,
    view_width: f64,
    col_x: f64,
    col_width: f64,
    gaps: f64,
) -> f64 {
    // A column wider than the view is always left-aligned.
    if view_width <= col_width {
        return 0.0;
    }
    // Padding shrinks when the column is wide.
    let padding = ((view_width - col_width) / 2.0).clamp(0.0, gaps);
    let desired_left = col_x - padding;
    let desired_right = col_x + col_width + padding;

    // Already fully visible: leave the view where it is.
    if cur_vp <= desired_left && desired_right <= cur_vp + view_width {
        return cur_vp - col_x;
    }

    // Prefer the alignment closer to the current position.
    let dist_left = (cur_vp - desired_left).abs();
    let dist_right = ((cur_vp + view_width) - desired_right).abs();
    if dist_left <= dist_right {
        -padding
    } else {
        -(view_width - padding - col_width)
    }
}

/// View offset that centers `(col_x, col_width)` in the view.
pub fn view_offset_centered(view_width: f64, col_x: f64, col_width: f64) -> f64 {
    if view_width <= col_width {
        return 0.0;
    }
    col_x + (col_width - view_width) / 2.0
}

/// Niri's OnOverflow rule: if the source and target columns (plus a
/// neighbor for context) fit in the view together, scroll minimally;
/// otherwise center the target.
pub fn view_offset_for_column(
    ws: &Workspace,
    params: &LayoutParams,
    view_width: f64,
    xs: &[f64],
    widths: &[f64],
    idx: usize,
    prev_idx: Option<usize>,
) -> f64 {
    let target_x = xs[idx];
    let target_w = widths[idx];

    let show = |vp: f64| view_offset_to_show(vp, view_width, target_x, target_w, params.gaps);
    let centered = || view_offset_centered(view_width, target_x, target_w);

    match params.center_focused_column {
        CenterFocused::Always => centered(),
        CenterFocused::Never => show(view_pos(ws, xs)),
        CenterFocused::OnOverflow => {
            let Some(prev_idx) = prev_idx else {
                return show(view_pos(ws, xs));
            };
            if prev_idx == idx {
                return show(view_pos(ws, xs));
            }
            // Take the neighbor of the target on the side we came from.
            let source_idx = if prev_idx > idx {
                (idx + 1).min(ws.columns.len() - 1)
            } else {
                idx.saturating_sub(1)
            };
            let source_x = xs[source_idx];
            let source_w = widths[source_idx];
            let span = if source_x < target_x {
                target_x - source_x + target_w
            } else {
                source_x - target_x + source_w
            } + params.gaps * 2.0;
            if span <= view_width {
                show(view_pos(ws, xs))
            } else {
                centered()
            }
        }
    }
}

/// Clamp a raw view position so the view never scrolls beyond the first
/// or last column. When everything fits, the content stays aligned to
/// the left edge (niri's behavior — `always-center-single-column`
/// defaults to false there).
pub fn clamp_view_pos(vp: f64, xs: &[f64], widths: &[f64], view_width: f64) -> f64 {
    let Some((&first_x, _)) = xs.first().zip(widths.first()) else {
        return vp;
    };
    let last_right = xs.last().unwrap() + widths.last().unwrap();
    let total = last_right - first_x;
    if total <= view_width {
        // Everything fits: keep the first column at the left edge so
        // growing/shrinking columns slides the others instead of
        // recentering the whole line.
        first_x
    } else {
        vp.clamp(first_x, last_right - view_width)
    }
}

/// Distribute the column's height among its tiles by weight, honoring
/// `gaps` between tiles. Returns `(y, height)` pairs in column-space.
pub fn tile_heights(ws: &Workspace, ci: usize, params: &LayoutParams, area_h: f64) -> Vec<(f64, f64)> {
    let col = &ws.columns[ci];
    let n = col.tiles.len();
    if n == 0 {
        return Vec::new();
    }
    let available = (area_h - params.gaps * (n - 1) as f64).max(n as f64);
    let total_weight: f64 = col.tiles.iter().map(|t| t.height_weight).sum();
    let mut out = Vec::with_capacity(n);
    let mut y = 0.0;
    let mut used = 0.0;
    for (i, tile) in col.tiles.iter().enumerate() {
        let share = if i == n - 1 {
            // Last tile takes the remainder to avoid rounding loss.
            available - used
        } else {
            let h = available * tile.height_weight / total_weight;
            used += h;
            h
        };
        // Respect the window's enforced minimum height; advance by the
        // clamped height so tiles never stack on top of each other.
        let h = share.max(1.0).max(tile.min_h);
        out.push((y, h));
        y += h + params.gaps;
    }
    out
}

/// Recompute and store the workspace's view offset so the active
/// column is visible. Call after any focus/placement change, before
/// `compute_workspace_geometry`.
pub fn refresh_view_offset(
    ws: &mut Workspace,
    params: &LayoutParams,
    view_width: f64,
    prev_idx: Option<usize>,
) {
    if ws.columns.is_empty() {
        ws.view_offset = 0.0;
        return;
    }
    let idx = ws.active_column_idx;
    let widths = column_widths(ws, params, view_width);
    let xs = column_xs(&widths, params.gaps);
    ws.view_offset = view_offset_for_column(ws, params, view_width, &xs, &widths, idx, prev_idx);
}

/// Compute the full on-screen geometry of a workspace, in absolute
/// screen coordinates. `area` is the monitor work rect
/// (left, top, width, height).
pub fn compute_workspace_geometry(
    ws: &Workspace,
    params: &LayoutParams,
    area: (f64, f64, f64, f64),
) -> Vec<TileRect> {
    // Overview mode: scale everything down to fit (niri's
    // toggle-overview filmstrip).
    if ws.is_overview && !ws.columns.is_empty() {
        return overview_geometry(ws, params, area);
    }
    let (ax, ay, aw, ah) = area;
    let view_width = (aw - params.edge_padding * 2.0).max(1.0);
    let view_height = (ah - params.edge_padding * 2.0).max(1.0);

    let widths = column_widths(ws, params, view_width);
    let xs = column_xs(&widths, params.gaps);
    let vp = clamp_view_pos(view_pos(ws, &xs), &xs, &widths, view_width);

    // Left edge of the content area in screen coordinates.
    let _origin_x = ax + params.edge_padding;
    let _origin_y = ay + params.edge_padding;

    let mut out = Vec::new();
    for (ci, col) in ws.columns.iter().enumerate() {
        let (col_x_off, col_w, col_h, col_pad) = if col.is_maximized {
            // Maximized column: the whole work area, no padding/gaps.
            // It keeps its scroll-space position — focusing another
                       // column scrolls it out of view instead of pinning it
            // over the viewport.
            (xs[ci] - vp, aw, ah, 0.0f64)
        } else {
            (xs[ci] - vp, widths[ci], view_height, 0.0)
        };
        let screen_x = if col.is_maximized {
            ax + col_x_off
        } else {
            ax + params.edge_padding + col_x_off
        };
        let base_y = if col.is_maximized { ay } else { ay + params.edge_padding };
        let heights = if col.is_maximized {
            // Only the first tile is visible; extra tiles render below
            // the monitor like niri (each maximized tile is full-size).
            vec![(0.0, col_h)]
        } else {
            tile_heights(ws, ci, params, col_h)
        };
        for (tile, &(y, h)) in col.tiles.iter().zip(heights.iter()) {
            out.push(TileRect {
                id: tile.id,
                x: (screen_x + col_pad).round() as i32,
                y: (base_y + y).round() as i32,
                w: col_w.round().max(1.0) as i32,
                h: h.round().max(1.0) as i32,
            });
        }
    }
    out
}

/// Overview geometry: all columns scaled by the same factor so the
/// whole workspace fits the view, centered horizontally and
/// vertically. Focus/navigation keep working on the scaled-down tiles.
fn overview_geometry(
    ws: &Workspace,
    params: &LayoutParams,
    area: (f64, f64, f64, f64),
) -> Vec<TileRect> {
    let (ax, ay, aw, ah) = area;
    let view_width = (aw - params.edge_padding * 2.0).max(1.0);
    let view_height = (ah - params.edge_padding * 2.0).max(1.0);
    let widths = column_widths(ws, params, view_width);
    let n = ws.columns.len();
    let total: f64 = widths.iter().sum::<f64>() + params.gaps * (n - 1) as f64;
    // Uniform scale (with a small margin), never scaled *up*.
    let scale = ((view_width / total.max(1.0)) * 0.96).min(1.0);
    let scaled_total = total * scale;
    let content_left = ax + (aw - scaled_total) / 2.0;
    let base_y = ay + (ah - view_height * scale) / 2.0;

    let mut out = Vec::new();
    let mut x = content_left;
    for (ci, col) in ws.columns.iter().enumerate() {
        let w = widths[ci] * scale;
        let heights = tile_heights(ws, ci, params, view_height);
        for (tile, &(y, h)) in col.tiles.iter().zip(heights.iter()) {
            out.push(TileRect {
                id: tile.id,
                x: x.round() as i32,
                y: (base_y + y * scale).round() as i32,
                w: w.round().max(1.0) as i32,
                h: (h * scale).round().max(1.0) as i32,
            });
        }
        x += w + params.gaps * scale;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{DirH, Workspace};
    use super::*;

    const W: f64 = 1000.0;
    const H: f64 = 800.0;

    fn params() -> LayoutParams {
        LayoutParams {
            gaps: 8.0,
            edge_padding: 8.0,
            default_column_width: ColumnWidth::Proportion(0.25),
            preset_column_widths: Vec::new(),
            center_focused_column: CenterFocused::OnOverflow,
        }
    }

    fn area() -> (f64, f64, f64, f64) {
        (0.0, 0.0, W, H)
    }

    #[test]
    fn widths_default_and_overrides() {
        let mut ws = Workspace::new();
        ws.add_window(1);
        ws.add_window(2);
        ws.columns[1].width = Some(ColumnWidth::Fixed(300.0));
        let p = params();
        let ws_w = W - 16.0;
        let widths = column_widths(&ws, &p, ws_w);
        assert_eq!(widths, vec![0.25 * ws_w, 300.0]);
        let xs = column_xs(&widths, 8.0);
        assert_eq!(xs, vec![0.0, 0.25 * ws_w + 8.0]);
    }

    #[test]
    fn view_pos_is_relative_to_active_column() {
        let mut ws = Workspace::new();
        ws.add_window(1);
        ws.add_window(2);
        let p = params();
        let widths = column_widths(&ws, &p, W);
        let xs = column_xs(&widths, p.gaps);
        assert_eq!(view_pos(&ws, &xs), xs[1]); // active = col 1, offset 0
        ws.view_offset = -10.0;
        assert_eq!(view_pos(&ws, &xs), xs[1] - 10.0);
    }

    #[test]
    fn offset_when_already_visible() {
        // Column at x=500 width 200, view 1000 wide, currently at 0:
        // fully visible -> view stays.
        let off = view_offset_to_show(0.0, 1000.0, 500.0, 200.0, 8.0);
        assert_eq!(off, 0.0 - 500.0);
    }

    #[test]
    fn offset_prefers_less_motion() {
        // Column at x=1200 width 200; view at 0. Desired right edge:
        // 1200+200+8 = 1408 > 1000 -> must move. Right-align needs
        // vp = 1408-1000 = 408; left-align vp = 1192. Right is closer.
        let off = view_offset_to_show(0.0, 1000.0, 1200.0, 200.0, 8.0);
        assert_eq!(off, -(1000.0 - 8.0 - 200.0)); // right-aligned: -792
    }

    #[test]
    fn wide_column_left_aligns() {
        let off = view_offset_to_show(300.0, 500.0, 100.0, 800.0, 8.0);
        assert_eq!(off, 0.0);
    }

    #[test]
    fn centered_offset() {
        // Centering the column at x=400: view pos = col_x - (view-col)/2.
        assert_eq!(view_offset_centered(1000.0, 400.0, 200.0), 0.0);
        assert_eq!(view_offset_centered(1000.0, 0.0, 200.0), -400.0);
    }

    #[test]
    fn clamp_left_aligns_when_content_fits() {
        let xs = vec![0.0, 260.0];
        let widths = vec![250.0, 250.0];
        // Total 510 < 1000: pinned to the left edge, not centered
        // (niri's default).
        let vp = clamp_view_pos(0.0, &xs, &widths, 1000.0);
        assert_eq!(vp, 0.0);
        // Content larger than the view: clamped to [0, last_right - view].
        let xs2 = vec![0.0, 2000.0];
        let widths2 = vec![500.0, 500.0];
        assert_eq!(clamp_view_pos(-50.0, &xs2, &widths2, 1000.0), 0.0);
        assert_eq!(clamp_view_pos(3000.0, &xs2, &widths2, 1000.0), 1500.0);
    }

    #[test]
    fn tile_heights_split_by_weight() {
        let mut ws = Workspace::new();
        ws.add_window(1);
        ws.add_window_to_column(2, 0, 1);
        let p = params();
        let hs = tile_heights(&ws, 0, &p, 800.0);
        let (y0, h0) = hs[0];
        let (y1, h1) = hs[1];
        assert_eq!(y0, 0.0);
        assert_eq!(h0, (800.0 - 8.0) / 2.0);
        assert_eq!(y1, h0 + 8.0);
        assert_eq!(h1, (800.0 - 8.0) / 2.0);
    }

    #[test]
    fn full_geometry_three_columns() {
        let mut ws = Workspace::new();
        ws.add_window(1);
        ws.add_window(2);
        ws.add_window(3);
        let p = params();
        let rects = compute_workspace_geometry(&ws, &p, area());
        assert_eq!(rects.len(), 3);
        let content_w = W - 16.0;
        let col_w = 0.25 * content_w;
        // Content (3 cols + gaps) fits: niri keeps it left-aligned —
        // first column at the left padding, no centering.
        let total = 3.0 * col_w + 16.0;
        assert!(total < content_w);
        for (i, r) in rects.iter().enumerate() {
            assert_eq!(r.w, col_w as i32);
            let expected_x = (8.0 + i as f64 * (col_w + 8.0)).round() as i32;
            assert_eq!(r.x, expected_x);
            assert_eq!(r.y, 8);
            assert_eq!(r.h, H as i32 - 16);
        }
    }

    #[test]
    fn overview_scales_and_fits() {
        let mut ws = Workspace::new();
        for i in 1..=8 {
            ws.add_window(i);
        }
        ws.toggle_overview();
        let p = params();
        let rects = compute_workspace_geometry(&ws, &p, area());
        assert_eq!(rects.len(), 8);
        // Everything must fit inside the area.
        for r in &rects {
            assert!(r.x >= 0 && r.x + r.w <= W as i32, "column fits: {r:?}");
            assert!(r.y >= 0 && r.y + r.h <= H as i32);
            assert!(r.h < H as i32, "scaled down vertically");
        }
        // Column order preserved, one row.
        let mut last_x = i32::MIN;
        for r in &rects {
            assert!(r.x >= last_x);
            last_x = r.x;
        }
        // Toggling off returns to normal full-height geometry.
        ws.toggle_overview();
        let rects = compute_workspace_geometry(&ws, &p, area());
        assert!(rects.iter().all(|r| r.h == H as i32 - 16));
    }

    #[test]
    fn maximize_covers_whole_area() {
        let mut ws = Workspace::new();
        ws.add_window(1);
        assert!(ws.toggle_maximized());
        let p = params();
        let rects = compute_workspace_geometry(&ws, &p, area());
        assert_eq!(rects.len(), 1);
        // Full monitor rect, no padding.
        assert_eq!((rects[0].x, rects[0].y, rects[0].w, rects[0].h), (0, 0, W as i32, H as i32));
    }

    #[test]
    fn min_size_widens_column() {
        let mut ws = Workspace::new();
        ws.add_window(1);
        ws.add_window(2);
        assert!(ws.set_min_size(2, 500.0, 68.0));
        let p = params();
        let view_w = W - 16.0;
        let widths = column_widths(&ws, &p, view_w);
        // Column 2 widened to the window's enforced minimum; column 1
        // keeps its proportion width.
        assert_eq!(widths[1], 500.0);
        assert_eq!(widths[0], 246.0);
        // Heights honor the minimum too.
        let rects = compute_workspace_geometry(&ws, &p, area());
        let second = rects.iter().find(|r| r.id == 2).unwrap();
        assert!(second.h >= 68, "min height respected: {second:?}");
    }

    #[test]
    fn maximized_column_scrolls_with_view() {
        let mut ws = Workspace::new();
        for i in 1..=3 {
            ws.add_window(i);
        }
        // Column 3 (focused) maximized: covers the whole area.
        assert!(ws.toggle_maximized());
        let p = params();
        let view_w = W - 16.0;
        refresh_view_offset(&mut ws, &p, view_w, None);
        let rects = compute_workspace_geometry(&ws, &p, area());
        let max_rect = rects.iter().find(|r| r.id == 3).unwrap();
        assert_eq!((max_rect.x, max_rect.y, max_rect.w, max_rect.h), (0, 0, W as i32, H as i32));

        // Focus the first column: the view scrolls left and the
        // maximized column must move with it (not stay pinned on top
        // of the viewport).
        ws.focus_column(DirH::Left);
        ws.focus_column(DirH::Left);
        refresh_view_offset(&mut ws, &p, view_w, None);
        let rects = compute_workspace_geometry(&ws, &p, area());
        let max_rect = rects.iter().find(|r| r.id == 3).unwrap();
        assert!(max_rect.x > 0, "maximized column scrolled: {max_rect:?}");
        let first = rects.iter().find(|r| r.id == 1).unwrap();
        assert!(first.x >= 0 && first.x + first.w <= W as i32, "first column visible: {first:?}");
    }

    #[test]
    fn focus_far_column_scrolls_view() {
        let mut ws = Workspace::new();
        for i in 1..=8 {
            ws.add_window(i);
        }
        // All columns have default width; total >> view.
        let p = params();
        let view_w = W - 16.0;
        let widths = column_widths(&ws, &p, view_w);
        let xs = column_xs(&widths, p.gaps);
        // Focus the last column.
        ws.focus_column(DirH::Right);
        ws.focus_column(DirH::Right);
        ws.focus_column(DirH::Right);
        ws.focus_column(DirH::Right);
        ws.focus_column(DirH::Right);
        ws.focus_column(DirH::Right);
        ws.focus_column(DirH::Right);
        let prev = Some(6usize);
        let new_off = view_offset_for_column(&ws, &p, view_w, &xs, &widths, 7, prev);
        let vp = xs[7] + new_off;
        // The last column must be fully visible.
        assert!(vp <= xs[7], "left edge visible: {vp} <= {}", xs[7]);
        assert!(xs[7] + widths[7] <= vp + view_w, "right edge visible");
    }
}
