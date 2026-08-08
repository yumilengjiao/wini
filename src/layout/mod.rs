//! The layout data model: a pure, Win32-free description of how windows
//! are organized. Structure mirrors niri's conceptual model:
//!
//! ```text
//! Layout
//!  └─ Monitor        (per output)
//!      └─ Workspace  (per monitor, one active at a time)
//!          └─ Column (horizontal scroll axis)
//!              └─ Tile (vertical stack inside a column)
//! ```
//!
//! This module only manages *structure and focus indices*. Geometry
//! computation (pixel sizes, view offsets) lives in `geometry`, and
//! applying geometry to real windows lives in `win`.
//!
//! Key semantics inherited from niri:
//! - Columns form the horizontal scroll axis; each workspace scrolls
//!   independently (`view_offset` relative to the active column).
//! - A column stacks tiles vertically; the column remembers its active
//!   tile.
//! - When a new column is created and immediately removed again without
//!   focus changes, focus returns to the previously active column
//!   (`activate_prev_column_on_removal`).
//! - Column width is either a proportion of the view, or fixed pixels;
//!   `None` means "use the default width".

// Mutation/focus methods not yet wired to key bindings will be consumed
// by the input module; silence the interim dead-code warnings.
#![allow(dead_code)]

pub mod geometry;

use std::collections::HashMap;

/// Identity of a tracked window (the HWND pointer value).
pub type WindowId = isize;

/// A single window inside a column.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub id: WindowId,
    /// Height hint: proportional weight for auto height distribution.
    /// 1.0 = equal share. Fixed heights arrive later with sizing actions.
    pub height_weight: f64,
    /// Minimum size the real window enforces (WM_GETMINMAXINFO),
    /// physical px. The layout must never compute a smaller tile:
    /// apps like Windows Terminal silently clamp SetWindowPos to
    /// their minimum, which would make them overlap neighbors.
    pub min_w: f64,
    pub min_h: f64,
}

impl Tile {
    fn new(id: WindowId) -> Self {
        Tile {
            id,
            height_weight: 1.0,
            min_w: 0.0,
            min_h: 0.0,
        }
    }
}

/// How wide a column wants to be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColumnWidth {
    /// Proportion of the view width, e.g. 0.5 = half the screen.
    Proportion(f64),
    /// Fixed width in logical pixels.
    Fixed(f64),
}

/// A vertical stack of tiles; one element of the horizontal scroll axis.
#[derive(Debug, Clone)]
pub struct Column {
    pub tiles: Vec<Tile>,
    /// Index of the focused tile in this column.
    pub active_tile_idx: usize,
    /// Desired width; `None` = config default.
    pub width: Option<ColumnWidth>,
    /// Whether the column is full-width (stretched to view edges).
    pub is_full_width: bool,
    /// Whether the column is maximized (covers the whole monitor, no
    /// padding, like niri's maximize-column).
    pub is_maximized: bool,
}

impl Column {
    fn new(id: WindowId) -> Self {
        Column {
            tiles: vec![Tile::new(id)],
            active_tile_idx: 0,
            width: None,
            is_full_width: false,
            is_maximized: false,
        }
    }

    pub fn active_tile(&self) -> Option<&Tile> {
        self.tiles.get(self.active_tile_idx)
    }

    pub fn focused_id(&self) -> Option<WindowId> {
        self.active_tile().map(|t| t.id)
    }
}

/// One workspace: a horizontally scrolling sequence of columns.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub columns: Vec<Column>,
    /// Index of the focused column.
    pub active_column_idx: usize,
    /// Horizontal view offset, in pixels, relative to the position that
    /// centers the active column. Positive = view scrolled right.
    /// (Geometry module interprets this.)
    pub view_offset: f64,
    /// The windowed-fullscreen window, rendered covering the whole
    /// monitor (niri's `toggle-windowed-fullscreen`). The layout
    /// structure underneath stays intact.
    pub fullscreen_id: Option<WindowId>,
    /// Overview mode (niri's `toggle-overview`): all columns are scaled
    /// down so the whole workspace is visible at once. Esc exits.
    pub is_overview: bool,
    /// Niri semantics: if the active column is removed without any
    /// intermediate focus change, restore focus (and view offset) to the
    /// previously active column instead of the neighbor.
    activate_prev_column_on_removal: Option<(usize, f64)>,
}

impl Workspace {
    pub fn new() -> Self {
        Workspace {
            columns: Vec::new(),
            active_column_idx: 0,
            view_offset: 0.0,
            fullscreen_id: None,
            is_overview: false,
            activate_prev_column_on_removal: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// The window this workspace currently focuses.
    pub fn focused_id(&self) -> Option<WindowId> {
        self.columns.get(self.active_column_idx)?.focused_id()
    }

    /// Location (column, tile) of a window, if present.
    pub fn find(&self, id: WindowId) -> Option<(usize, usize)> {
        for (ci, col) in self.columns.iter().enumerate() {
            if let Some(ti) = col.tiles.iter().position(|t| t.id == id) {
                return Some((ci, ti));
            }
        }
        None
    }

    /// Record the minimum size the real window enforces
    /// (WM_GETMINMAXINFO). Returns true when a tile's minimum grew,
    /// i.e. the layout needs a reflow to honor it.
    pub fn set_min_size(&mut self, id: WindowId, min_w: f64, min_h: f64) -> bool {
        let Some((ci, ti)) = self.find(id) else {
            return false;
        };
        let tile = &mut self.columns[ci].tiles[ti];
        let grew = min_w > tile.min_w + 0.5 || min_h > tile.min_h + 0.5;
        tile.min_w = tile.min_w.max(min_w);
        tile.min_h = tile.min_h.max(min_h);
        grew
    }

    /// Add a window as a new column at the end (niri's default placement)
    /// and focus it.
    pub fn add_window(&mut self, id: WindowId) {
        let prev_active = if self.columns.is_empty() {
            None
        } else {
            Some((self.active_column_idx, self.view_offset))
        };
        self.columns.push(Column::new(id));
        self.active_column_idx = self.columns.len() - 1;
        self.view_offset = 0.0;
        self.activate_prev_column_on_removal = prev_active;
    }

    /// Add several windows as one new column (for
    /// move-column-to-workspace), focused.
    pub fn add_column(&mut self, ids: &[WindowId]) {
        if ids.is_empty() {
            return;
        }
        let prev_active = if self.columns.is_empty() {
            None
        } else {
            Some((self.active_column_idx, self.view_offset))
        };
        let mut col = Column::new(ids[0]);
        for &id in &ids[1..] {
            col.tiles.push(Tile::new(id));
        }
        self.columns.push(col);
        self.active_column_idx = self.columns.len() - 1;
        self.view_offset = 0.0;
        self.activate_prev_column_on_removal = prev_active;
    }

    /// All window ids in layout order.
    pub fn window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.columns.iter().flat_map(|c| c.tiles.iter().map(|t| t.id))
    }

    /// Insert a window as a new column at `idx` (drag-and-drop between
    /// columns). Clamped to the valid range; focuses the new column.
    pub fn insert_column_at(&mut self, idx: usize, id: WindowId) {
        let idx = idx.min(self.columns.len());
        self.columns.insert(idx, Column::new(id));
        self.active_column_idx = idx;
        self.view_offset = 0.0;
        self.activate_prev_column_on_removal = None;
    }

    /// Add a window into an existing column at `column_idx` (used by
    /// absorb / move-into-column).
    pub fn add_window_to_column(&mut self, id: WindowId, column_idx: usize, tile_idx: usize) {
        let Some(col) = self.columns.get_mut(column_idx) else {
            self.add_window(id);
            return;
        };
        let tile_idx = tile_idx.min(col.tiles.len());
        col.tiles.insert(tile_idx, Tile::new(id));
        col.active_tile_idx = tile_idx;
        self.active_column_idx = column_idx;
    }

    /// Niri's `toggle-windowed-fullscreen`: the focused window is
    /// rendered covering the whole monitor (applied by the caller's
    /// geometry stage); the layout structure stays intact so toggling
    /// off restores its tile exactly.
    pub fn toggle_fullscreen(&mut self) -> bool {
        let Some(id) = self.focused_id() else {
            return false;
        };
        if self.fullscreen_id == Some(id) {
            self.fullscreen_id = None;
        } else {
            self.fullscreen_id = Some(id);
        }
        true
    }

    /// Toggle overview mode (niri's toggle-overview): the whole
    /// workspace scales down into a filmstrip; Esc (or the same bind)
    /// exits back into the focused column.
    pub fn toggle_overview(&mut self) -> bool {
        self.is_overview = !self.is_overview;
        true
    }

    /// Remove a window. Fixes up focus indices following niri's rules:
    /// - removing the focused column moves focus to the left neighbor
    ///   (or the prev-active column for a freshly created column),
    /// - removing a tile from the focused column keeps tile focus in
    ///   place (clamped).
    pub fn remove_window(&mut self, id: WindowId) -> bool {
        let Some((ci, ti)) = self.find(id) else {
            return false;
        };
        if self.fullscreen_id == Some(id) {
            self.fullscreen_id = None;
        }
        let col = &mut self.columns[ci];
        col.tiles.remove(ti);
        if col.tiles.is_empty() {
            self.columns.remove(ci);
            // Pick the new active column.
            let (new_idx, new_offset) = if let Some((prev_idx, prev_offset)) =
                self.activate_prev_column_on_removal
            {
                // Only valid if the removed column was the active one and
                // no focus change happened since creation.
                let candidate = prev_idx.min(self.columns.len().saturating_sub(1));
                (candidate, prev_offset)
            } else {
                (ci.saturating_sub(1), 0.0)
            };
            self.activate_prev_column_on_removal = None;
            self.active_column_idx = if self.columns.is_empty() {
                0
            } else {
                new_idx
            };
            self.view_offset = new_offset;
        } else {
            self.active_column_idx = ci;
            col.active_tile_idx = col.active_tile_idx.min(col.tiles.len() - 1);
        }
        true
    }

    /// Focus the window: sets active column and tile indices.
    pub fn focus_window(&mut self, id: WindowId) -> bool {
        let Some((ci, ti)) = self.find(id) else {
            return false;
        };
        self.active_column_idx = ci;
        self.columns[ci].active_tile_idx = ti;
        self.activate_prev_column_on_removal = None;
        true
    }

    /// Move focus one column left/right. No wraparound (niri default).
    pub fn focus_column(&mut self, dir: DirH) -> bool {
        let next = if dir == DirH::Left {
            self.active_column_idx.checked_sub(1)
        } else {
            Some(self.active_column_idx + 1).filter(|&i| i < self.columns.len())
        };
        match next {
            Some(i) if i < self.columns.len() => {
                self.active_column_idx = i;
                self.view_offset = 0.0;
                self.activate_prev_column_on_removal = None;
                true
            }
            _ => false,
        }
    }

    /// Move focus one tile up/down within the focused column.
    pub fn focus_tile(&mut self, dir: DirV) -> bool {
        let Some(col) = self.columns.get_mut(self.active_column_idx) else {
            return false;
        };
        let next = if dir == DirV::Up {
            col.active_tile_idx.checked_sub(1)
        } else {
            Some(col.active_tile_idx + 1).filter(|&i| i < col.tiles.len())
        };
        match next {
            Some(i) if i < col.tiles.len() => {
                col.active_tile_idx = i;
                true
            }
            _ => false,
        }
    }

    /// Swap the focused column with its neighbor.
    pub fn move_column(&mut self, dir: DirH) -> bool {
        if self.columns.len() < 2 {
            return false;
        }
        let i = self.active_column_idx;
        let j = if dir == DirH::Left {
            match i.checked_sub(1) {
                Some(j) => j,
                None => return false,
            }
        } else if i + 1 < self.columns.len() {
            i + 1
        } else {
            return false;
        };
        self.columns.swap(i, j);
        self.active_column_idx = j;
        true
    }

    /// Move the focused tile up/down inside its column. At the edges,
    /// the tile stays (niri's move-window-down moves it into the next
    /// column — that's `move_tile_across`).
    pub fn move_tile(&mut self, dir: DirV) -> bool {
        let Some(col) = self.columns.get_mut(self.active_column_idx) else {
            return false;
        };
        if col.tiles.len() < 2 {
            return false;
        }
        let i = col.active_tile_idx;
        let j = if dir == DirV::Up {
            match i.checked_sub(1) {
                Some(j) => j,
                None => return false,
            }
        } else if i + 1 < col.tiles.len() {
            i + 1
        } else {
            return false;
        };
        col.tiles.swap(i, j);
        col.active_tile_idx = j;
        true
    }

    /// Move the focused tile to the neighboring column (niri's
    /// move-window-to-column-left/right):
    /// - inside a multi-tile column: change position within the neighbor,
    /// - otherwise: become a standalone column at the neighbor position.
    pub fn move_tile_across(&mut self, dir: DirH) -> bool {
        let Some((ci, ti)) = self.find_focused() else {
            return false;
        };
        let neighbor = if dir == DirH::Left {
            ci.checked_sub(1)
        } else {
            Some(ci + 1).filter(|&i| i < self.columns.len())
        };
        let Some(ni) = neighbor else {
            return false;
        };

        let tile = self.columns[ci].tiles.remove(ti);
        if self.columns[ci].tiles.is_empty() {
            self.columns.remove(ci);
        } else {
            self.columns[ci].active_tile_idx =
                self.columns[ci].active_tile_idx.min(self.columns[ci].tiles.len() - 1);
        }

        if !self.columns[ni].tiles.is_empty() {
            // Insert into the neighbor column.
            let at = if dir == DirH::Left {
                self.columns[ni].tiles.len()
            } else {
                0
            };
            self.columns[ni].tiles.insert(at, tile);
            self.columns[ni].active_tile_idx = at;
        }
        // (The standalone-column branch cannot happen: a neighbor exists,
        // so we always merge into it.)
        self.active_column_idx = ni;
        true
    }

    /// Consume or expel the focused window one slot in `dir` (niri's
    /// consume-or-expel-window-left/right):
    ///
    /// - If the focused window is alone in its column, it is *consumed*
    ///   into the adjacent column (appended at the bottom, becomes the
    ///   active tile there).
    /// - If its column holds multiple windows, it is *expelled* into a
    ///   new standalone column next to the current one.
    ///
    /// Returns false if there is no adjacent column to consume into.
    pub fn consume_or_expel(&mut self, dir: DirH) -> bool {
        let Some((ci, ti)) = self.find_focused() else {
            return false;
        };

        if self.columns[ci].tiles.len() == 1 {
            // Consume: move into the adjacent column. Check that one
            // exists before removing our column.
            let has_neighbor = match dir {
                DirH::Left => ci > 0,
                DirH::Right => ci + 1 < self.columns.len(),
            };
            if !has_neighbor {
                return false;
            }
            let width = self.columns[ci].width;
            let col = self.columns.remove(ci);
            let tile = col.tiles.into_iter().next().unwrap();
            // Column indices >= ci shifted down by one.
            let t = match dir {
                DirH::Left => ci - 1,
                DirH::Right => ci,
            };
            self.columns[t].tiles.push(tile);
            self.columns[t].active_tile_idx = self.columns[t].tiles.len() - 1;
            // Preserve the consumed column's width on the target.
            if self.columns[t].width.is_none() {
                self.columns[t].width = width;
            }
            self.active_column_idx = t;
            self.view_offset = 0.0;
            self.activate_prev_column_on_removal = None;
        } else {
            // Expel: standalone column next to the current position.
            let tile = self.columns[ci].tiles.remove(ti);
            self.columns[ci].active_tile_idx =
                self.columns[ci].active_tile_idx.min(self.columns[ci].tiles.len() - 1);
            let col = Column {
                tiles: vec![tile],
                active_tile_idx: 0,
                width: self.columns[ci].width,
                is_full_width: false,
                is_maximized: false,
            };
            let at = match dir {
                DirH::Left => ci,
                DirH::Right => ci + 1,
            };
            self.columns.insert(at, col);
            self.active_column_idx = at;
            self.view_offset = 0.0;
            self.activate_prev_column_on_removal = None;
        }
        true
    }

    /// Niri's consume-window-into-column (Mod+Comma): take the top
    /// window of the column to the RIGHT of the focused one and append
    /// it to the bottom of the focused column.
    pub fn consume_window_into_column(&mut self) -> bool {
        if self.active_column_idx >= self.columns.len() {
            return false;
        }
        let ci = self.active_column_idx;
        // Needs a column to the right.
        let si = ci + 1;
        if si >= self.columns.len() {
            return false;
        }
        let tile = self.columns[si].tiles.remove(0);
        if self.columns[si].tiles.is_empty() {
            self.columns.remove(si);
        }
        self.columns[ci].tiles.push(tile);
        self.columns[ci].active_tile_idx = self.columns[ci].tiles.len() - 1;
        self.active_column_idx = ci;
        self.view_offset = 0.0;
        self.activate_prev_column_on_removal = None;
        true
    }

    /// Niri's expel-window-from-column (Mod+Period): move the BOTTOM
    /// window of the focused column out into a new standalone column
    /// inserted to its right. No-op when the column has a single
    /// window (niri refuses rather than creating an empty column).
    pub fn expel_window_from_column(&mut self) -> bool {
        if self.active_column_idx >= self.columns.len() {
            return false;
        }
        let ci = self.active_column_idx;
        if self.columns[ci].tiles.len() < 2 {
            return false;
        }
        let ti = self.columns[ci].tiles.len() - 1;
        let tile = self.columns[ci].tiles.remove(ti);
        let width = self.columns[ci].width;
        let col = Column {
            tiles: vec![tile],
            active_tile_idx: 0,
            width,
            is_full_width: false,
            is_maximized: false,
        };
        self.columns.insert(ci + 1, col);
        // Focus stays on the (still focused) source column, but its
        // active tile must be valid after the removal.
        self.columns[ci].active_tile_idx =
            self.columns[ci].active_tile_idx.min(self.columns[ci].tiles.len() - 1);
        self.active_column_idx = ci;
        self.view_offset = 0.0;
        self.activate_prev_column_on_removal = None;
        true
    }

    /// Niri's set-column-width: delta ("+100"/"-100"), fixed
    /// ("1000") or proportion ("50%"). Applied to the focused column.
    pub fn set_column_width(&mut self, spec: &SizeChange) -> bool {
        let Some(ci) = self.columns.len().checked_sub(0).map(|_| self.active_column_idx) else {
            return false;
        };
        let col = &mut self.columns[ci];
        match spec {
            SizeChange::Delta(px) => {
                let new_w = match col.width {
                    Some(ColumnWidth::Fixed(f)) => f + px,
                    _ => px.to_owned(),
                };
                col.width = Some(ColumnWidth::Fixed(new_w.max(100.0)));
            }
            SizeChange::Fixed(px) => {
                col.width = Some(ColumnWidth::Fixed(px.max(100.0)));
            }
            SizeChange::Proportion(p) => {
                col.width = Some(ColumnWidth::Proportion(p.clamp(0.01, 1.0)));
            }
        }
        col.is_full_width = false;
        true
    }

    /// Niri's preset-column-widths: a bare `set-column-width` cycles the
    /// focused column through the configured presets. The next preset
    /// is the first one after the current width's position (default
    /// width counts as "before the first preset").
    pub fn cycle_column_width(&mut self, presets: &[ColumnWidth]) -> bool {
        if presets.is_empty() {
            return false;
        }
        let Some(col) = self.columns.get_mut(self.active_column_idx) else {
            return false;
        };
        let next = match col.width {
            None => presets[0],
            Some(w) => {
                let idx = presets.iter().position(|&p| p == w);
                match idx {
                    Some(i) => presets[(i + 1) % presets.len()],
                    None => presets[0],
                }
            }
        };
        col.width = Some(next);
        col.is_full_width = false;
        true
    }

    /// Toggle the focused column between full width and its previous
    /// width.
    pub fn toggle_full_width(&mut self) -> bool {
        let Some(col) = self.columns.get_mut(self.active_column_idx) else {
            return false;
        };
        col.is_full_width = !col.is_full_width;
        true
    }

    /// Toggle the focused column between maximized (fills the monitor)
    /// and normal. On Windows, "maximized" means covering the full
    /// monitor rect (including taskbar) — implemented as full-width +
    /// edge-padding removal in geometry via Column.is_full_width plus
    /// is_maximized.
    pub fn toggle_maximized(&mut self) -> bool {
        let Some(col) = self.columns.get_mut(self.active_column_idx) else {
            return false;
        };
        col.is_maximized = !col.is_maximized;
        col.is_full_width = col.is_maximized;
        true
    }

    /// Niri's set-window-height on the focused tile: adjusts the tile's
    /// height weight or fixed height relative to the column.
    pub fn set_window_height(&mut self, spec: &SizeChange) -> bool {
        let Some(col) = self.columns.get_mut(self.active_column_idx) else {
            return false;
        };
        let ti = col.active_tile_idx.min(col.tiles.len().saturating_sub(1));
        let Some(tile) = col.tiles.get_mut(ti) else {
            return false;
        };
        match spec {
            SizeChange::Delta(d) => {
                tile.height_weight = (tile.height_weight + d / 100.0).max(0.1);
            }
            SizeChange::Fixed(px) => {
                // Store as weight relative to a ~800px reference column.
                tile.height_weight = (px / 400.0).max(0.05);
            }
            SizeChange::Proportion(p) => {
                tile.height_weight = p.max(0.05);
            }
        }
        true
    }

    /// Focus the first or last column (niri's focus-column-first/last).
    pub fn focus_column_edge(&mut self, edge: Edge) -> bool {
        if self.columns.is_empty() {
            return false;
        }
        let idx = match edge {
            Edge::First => 0,
            Edge::Last => self.columns.len() - 1,
        };
        self.focus_column_at(idx)
    }

    /// Focus the column at `index` (0-based, clamped to the last
    /// column) — the `focus-column-index` action behind the numeric
    /// key binds.
    pub fn focus_column_index(&mut self, index: usize) -> bool {
        match self.columns.len().checked_sub(1) {
            Some(last) => self.focus_column_at(index.min(last)),
            None => false,
        }
    }

    /// Focus the column at `idx` (already validated), resetting the
    /// view offset like any horizontal focus jump.
    fn focus_column_at(&mut self, idx: usize) -> bool {
        if idx != self.active_column_idx {
            self.active_column_idx = idx;
            self.view_offset = 0.0;
            self.activate_prev_column_on_removal = None;
            true
        } else {
            false
        }
    }

    /// Focus the window in the focused column at `index` (niri's
    /// focus-window-in-column).
    pub fn focus_window_in_column(&mut self, index: usize) -> bool {
        let Some(col) = self.columns.get_mut(self.active_column_idx) else {
            return false;
        };
        if index < col.tiles.len() {
            col.active_tile_idx = index;
            true
        } else {
            false
        }
    }

    fn find_focused(&self) -> Option<(usize, usize)> {
        let ci = self.active_column_idx;
        let ti = self.columns.get(ci)?.active_tile_idx;
        Some((ci, ti))
    }
}

/// Horizontal direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirH {
    Left,
    Right,
}

/// Vertical direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirV {
    Up,
    Down,
}

/// Horizontal edge for focus-column-first/last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    First,
    Last,
}

/// A parsed size-change argument (niri's set-column-width/
/// set-window-height syntax).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeChange {
    /// "+100" / "-100"
    Delta(f64),
    /// "1000"
    Fixed(f64),
    /// "50%"
    Proportion(f64),
}

impl SizeChange {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(pct) = s.strip_suffix('%') {
            let p: f64 = pct.trim().parse().ok()?;
            return Some(SizeChange::Proportion(p / 100.0));
        }
        if let Some(delta) = s.strip_prefix('+') {
            let d: f64 = delta.trim().parse().ok()?;
            return Some(SizeChange::Delta(d));
        }
        if let Some(delta) = s.strip_prefix('-') {
            let d: f64 = delta.trim().parse().ok()?;
            return Some(SizeChange::Delta(-d));
        }
        let f: f64 = s.parse().ok()?;
        Some(SizeChange::Fixed(f))
    }
}

/// Per-monitor layout: a stack of workspaces, one active.
#[derive(Debug, Clone)]
pub struct MonitorLayout {
    /// Device name from `win::monitor` (stable across handle changes).
    pub device: String,
    pub workspaces: Vec<Workspace>,
    pub active_workspace_idx: usize,
    /// Fast lookup: window -> owning workspace index.
    window_map: HashMap<WindowId, usize>,
}

impl MonitorLayout {
    pub fn new(device: String) -> Self {
        MonitorLayout {
            device,
            workspaces: vec![Workspace::new()],
            active_workspace_idx: 0,
            window_map: HashMap::new(),
        }
    }

    pub fn active_workspace(&self) -> &Workspace {
        &self.workspaces[self.active_workspace_idx]
    }

    pub fn active_workspace_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active_workspace_idx]
    }

    pub fn add_window(&mut self, id: WindowId) {
        let ws = self.active_workspace_idx;
        self.active_workspace_mut().add_window(id);
        self.window_map.insert(id, ws);
    }

    /// Add a window to a specific (1-based-index-resolved) workspace,
    /// growing the list on demand, and make that workspace active so
    /// focus follows the new window (window-rule open-on-workspace).
    pub fn add_window_to_workspace(&mut self, id: WindowId, idx: usize) {
        self.ensure_workspaces(idx + 1);
        self.workspaces[idx].add_window(id);
        self.window_map.insert(id, idx);
        self.active_workspace_idx = idx;
    }

    pub fn remove_window(&mut self, id: WindowId) -> bool {
        let Some(ws) = self.window_map.remove(&id) else {
            return false;
        };
        self.workspaces[ws].remove_window(id)
    }

    /// Which workspace holds this window?
    pub fn workspace_of(&self, id: WindowId) -> Option<usize> {
        self.window_map.get(&id).copied()
    }

    /// Grow the workspace list on demand (niri-style: workspaces come
    /// into existence when first referenced).
    pub fn ensure_workspaces(&mut self, count: usize) {
        while self.workspaces.len() < count {
            self.workspaces.push(Workspace::new());
        }
    }

    /// Switch the active workspace (niri focus-workspace), growing the
    /// list on demand. False if already active.
    pub fn switch_workspace(&mut self, idx: usize) -> bool {
        if idx == self.active_workspace_idx {
            return false;
        }
        self.ensure_workspaces(idx + 1);
        self.active_workspace_idx = idx;
        true
    }

    /// Niri move-window-to-workspace. With `focus`, the target
    /// workspace becomes active (the window follows).
    pub fn move_focused_window_to_workspace(
        &mut self,
        idx: usize,
        focus: bool,
    ) -> Option<WindowId> {
        let id = self.active_workspace().focused_id()?;
        if idx == self.active_workspace_idx {
            return None;
        }
        self.ensure_workspaces(idx + 1);
        self.active_workspace_mut().remove_window(id);
        self.workspaces[idx].add_window(id);
        self.window_map.insert(id, idx);
        if focus {
            self.active_workspace_idx = idx;
        }
        Some(id)
    }

    /// Niri move-column-to-workspace: the whole focused column moves as
    /// a single column on the target workspace. Returns the moved ids.
    pub fn move_focused_column_to_workspace(
        &mut self,
        idx: usize,
        focus: bool,
    ) -> Option<Vec<WindowId>> {
        if idx == self.active_workspace_idx {
            return None;
        }
        let ids: Vec<WindowId> = self
            .active_workspace()
            .columns
            .get(self.active_workspace().active_column_idx)?
            .tiles
            .iter()
            .map(|t| t.id)
            .collect();
        if ids.is_empty() {
            return None;
        }
        self.ensure_workspaces(idx + 1);
        for &id in &ids {
            self.active_workspace_mut().remove_window(id);
            self.window_map.insert(id, idx);
        }
        self.workspaces[idx].add_column(&ids);
        if focus {
            self.active_workspace_idx = idx;
        }
        Some(ids)
    }
}

/// The whole layout: one entry per monitor.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub monitors: Vec<MonitorLayout>,
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn monitor(&self, device: &str) -> Option<&MonitorLayout> {
        self.monitors.iter().find(|m| m.device == device)
    }

    pub fn monitor_mut(&mut self, device: &str) -> Option<&mut MonitorLayout> {
        self.monitors.iter_mut().find(|m| m.device == device)
    }

    /// Add a monitor with a fresh workspace.
    pub fn add_monitor(&mut self, device: &str) {
        if self.monitor(device).is_none() {
            self.monitors.push(MonitorLayout::new(device.to_string()));
        }
    }

    /// Remove a monitor; its windows will need rehoming by the caller.
    pub fn remove_monitor(&mut self, device: &str) -> Option<MonitorLayout> {
        let idx = self.monitors.iter().position(|m| m.device == device)?;
        Some(self.monitors.remove(idx))
    }

    /// The window focused on a given monitor, if any.
    pub fn focused_id(&self, device: &str) -> Option<WindowId> {
        self.monitor(device)?.active_workspace().focused_id()
    }

    /// Add a window to the active workspace of a monitor.
    pub fn add_window(&mut self, device: &str, id: WindowId) {
        self.add_monitor(device);
        self.monitor_mut(device).unwrap().add_window(id);
    }

    /// Remove a window from wherever it lives. Returns the device it was
    /// found on.
    pub fn remove_window(&mut self, id: WindowId) -> Option<String> {
        for m in &mut self.monitors {
            if m.remove_window(id) {
                return Some(m.device.clone());
            }
        }
        None
    }

    /// Focus a window: activates its workspace, column and tile.
    pub fn focus_window(&mut self, id: WindowId) -> bool {
        for m in &mut self.monitors {
            if let Some(ws) = m.workspace_of(id) {
                m.active_workspace_idx = ws;
                m.active_workspace_mut().focus_window(id);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: WindowId = 1;
    const B: WindowId = 2;
    const C: WindowId = 3;
    const D: WindowId = 4;

    #[test]
    fn add_and_focus() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window(B);
        ws.add_window(C);
        assert_eq!(ws.columns.len(), 3);
        assert_eq!(ws.focused_id(), Some(C));
        assert!(ws.focus_column(DirH::Left));
        assert_eq!(ws.focused_id(), Some(B));
    }

    #[test]
    fn focus_no_wraparound() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        assert!(!ws.focus_column(DirH::Left));
        assert!(!ws.focus_column(DirH::Right)); // single column
        ws.add_window(B);
        // add_window focuses the new column: B is already focused.
        assert_eq!(ws.focused_id(), Some(B));
        assert!(!ws.focus_column(DirH::Right));
        assert!(ws.focus_column(DirH::Left));
        assert_eq!(ws.focused_id(), Some(A));
        assert!(!ws.focus_column(DirH::Left));
    }

    #[test]
    fn remove_focused_column_returns_to_prev() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window(B); // B focused, prev = A
        assert_eq!(ws.focused_id(), Some(B));
        ws.remove_window(B);
        // Freshly created column removed: back to the previous column.
        assert_eq!(ws.focused_id(), Some(A));
    }

    #[test]
    fn remove_middle_column_falls_left() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window(B);
        ws.add_window(C);
        ws.focus_column(DirH::Left); // focus B
        assert_eq!(ws.focused_id(), Some(B));
        ws.remove_window(B);
        // B was focused but not "freshly created": neighbor to the left.
        assert_eq!(ws.focused_id(), Some(A));
    }

    #[test]
    fn tile_navigation_and_move() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window_to_column(B, 0, 1);
        ws.add_window_to_column(C, 0, 2);
        assert_eq!(ws.columns[0].tiles.len(), 3);
        assert_eq!(ws.focused_id(), Some(C));
        assert!(ws.focus_tile(DirV::Up));
        assert_eq!(ws.focused_id(), Some(B));
        assert!(ws.move_tile(DirV::Up));
        assert_eq!(ws.columns[0].tiles.iter().map(|t| t.id).collect::<Vec<_>>(), vec![B, A, C]);
        assert_eq!(ws.focused_id(), Some(B));
        // At top edge: no move.
        assert!(!ws.move_tile(DirV::Up));
    }

    #[test]
    fn move_column() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window(B);
        ws.add_window(C);
        // C focused (rightmost); swap with neighbor only.
        assert!(ws.move_column(DirH::Left));
        assert_eq!(
            ws.columns.iter().map(|c| c.focused_id()).collect::<Vec<_>>(),
            vec![Some(A), Some(C), Some(B)]
        );
        assert_eq!(ws.focused_id(), Some(C));
        assert!(ws.move_column(DirH::Left));
        assert_eq!(ws.focused_id(), Some(C));
        // Left edge: no move.
        assert!(!ws.move_column(DirH::Left));
        // Right edge: no move either (single-step swaps, no wrap).
        assert!(ws.move_column(DirH::Right));
        assert!(ws.move_column(DirH::Right));
        assert!(!ws.move_column(DirH::Right));
    }

    #[test]
    fn move_tile_across_merges() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window(B);
        ws.add_window_to_column(C, 0, 1); // A,C in col 0
        ws.focus_column(DirH::Right); // focus B (col 1)
        // Move B into column 0 (to its right).
        assert!(ws.move_tile_across(DirH::Left));
        assert_eq!(ws.columns.len(), 1);
        assert_eq!(
            ws.columns[0].tiles.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![A, C, B]
        );
        assert_eq!(ws.focused_id(), Some(B));
    }

    #[test]
    fn consume_into_and_expel_from_column() {
        // A | B, C  (A alone; right column stacked B over C)
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window(B);
        ws.add_window_to_column(C, 1, 1);
        // add_window_to_column moved focus to column 1; put it back.
        ws.active_column_idx = 0;
        ws.columns[0].active_tile_idx = 0;

        // consume: top window of the right column (B) joins our bottom;
        // C stays behind as the now-focused column 1.
        assert!(ws.consume_window_into_column());
        assert_eq!(ws.columns.len(), 2);
        assert_eq!(
            ws.columns[0].tiles.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![A, B]
        );
        assert_eq!(
            ws.columns[1].tiles.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![C]
        );
        assert_eq!(ws.focused_id(), Some(B));

        // consume again: C joins our column; only one column remains.
        assert!(ws.consume_window_into_column());
        assert_eq!(ws.columns.len(), 1);
        assert_eq!(
            ws.columns[0].tiles.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![A, B, C]
        );

        // expel: bottom window (C) leaves into a new column on the right.
        assert!(ws.expel_window_from_column());
        assert_eq!(ws.columns.len(), 2);
        assert_eq!(
            ws.columns[0].tiles.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![A, B]
        );
        assert_eq!(ws.focused_id(), Some(B));

        // Single-window column: expel refuses (niri behavior).
        ws.focus_column(DirH::Right);
        assert!(!ws.expel_window_from_column());
        // No right neighbor: consume refuses.
        assert!(!ws.consume_window_into_column());
    }

    #[test]
    fn consume_and_expel() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window(B);
        // B is alone in its column: consume-left merges it into A's
        // column, below the active tile.
        assert!(ws.consume_or_expel(DirH::Left));
        assert_eq!(ws.columns.len(), 1);
        assert_eq!(
            ws.columns[0].tiles.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![A, B]
        );
        assert_eq!(ws.focused_id(), Some(B));
        // Multi-tile column: expel always succeeds, even at the left
        // edge (standalone column at the current position).
        assert!(ws.consume_or_expel(DirH::Left));
        assert_eq!(ws.columns.len(), 2);
        assert_eq!(ws.focused_id(), Some(B));
        assert_eq!(ws.active_column_idx, 0);
        // B standalone at index 0: no left neighbor to consume into.
        assert!(!ws.consume_or_expel(DirH::Left));
        // Merge back right into A's column.
        assert!(ws.consume_or_expel(DirH::Right));
        assert_eq!(ws.columns.len(), 1);
        assert_eq!(ws.focused_id(), Some(B));
        // Expel right: standalone column after the current one.
        assert!(ws.consume_or_expel(DirH::Right));
        assert_eq!(ws.columns.len(), 2);
        assert_eq!(ws.active_column_idx, 1);
        assert_eq!(ws.focused_id(), Some(B));
        // B standalone at the right edge: no right neighbor.
        assert!(!ws.consume_or_expel(DirH::Right));
    }

    #[test]
    fn workspace_switch_and_move() {
        let mut ml = MonitorLayout::new("DISPLAY1".into());
        ml.add_window(A);
        ml.add_window(B);

        // Grow on demand + switch.
        assert!(ml.switch_workspace(2));
        assert_eq!(ml.workspaces.len(), 3);
        assert!(ml.active_workspace().is_empty());
        assert!(!ml.switch_workspace(2), "no-op when already active");

        // Move the focused window to workspace 1 (focus follows).
        assert_eq!(
            ml.move_focused_window_to_workspace(1, true),
            None,
            "empty workspace has nothing focused"
        );
        assert!(ml.switch_workspace(0));
        assert_eq!(
            ml.move_focused_window_to_workspace(1, true),
            Some(B)
        );
        assert_eq!(ml.active_workspace_idx, 1);
        assert_eq!(ml.active_workspace().focused_id(), Some(B));
        // A stayed behind on workspace 0.
        assert_eq!(ml.workspaces[0].focused_id(), Some(A));
        assert_eq!(ml.workspace_of(B), Some(1));

        // Column move takes every tile of the focused column.
        ml.move_focused_window_to_workspace(2, true);
        ml.add_window(C); // joins workspace 2 as a new column
        let moved = ml.move_focused_column_to_workspace(0, false).unwrap();
        assert_eq!(moved, vec![C]);
        assert_eq!(ml.active_workspace_idx, 2, "focus=false keeps workspace");
        assert_eq!(ml.workspace_of(C), Some(0));
        assert_eq!(ml.workspaces[0].columns.len(), 2, "A + C's column");
    }

    #[test]
    fn layout_add_remove_focus() {
        let mut layout = Layout::new();
        layout.add_window("DISPLAY1", A);
        layout.add_window("DISPLAY1", B);
        layout.add_window("DISPLAY2", C);
        assert_eq!(layout.focused_id("DISPLAY1"), Some(B));
        assert_eq!(layout.focused_id("DISPLAY2"), Some(C));
        assert!(layout.focus_window(A));
        assert_eq!(layout.focused_id("DISPLAY1"), Some(A));
        assert_eq!(layout.remove_window(A), Some("DISPLAY1".to_string()));
        assert_eq!(layout.focused_id("DISPLAY1"), Some(B));
        assert_eq!(layout.remove_window(C), Some("DISPLAY2".to_string()));
    }

    #[test]
    fn size_change_parsing() {
        assert_eq!(SizeChange::parse("+100"), Some(SizeChange::Delta(100.0)));
        assert_eq!(SizeChange::parse("-50"), Some(SizeChange::Delta(-50.0)));
        assert_eq!(SizeChange::parse("1000"), Some(SizeChange::Fixed(1000.0)));
        assert_eq!(SizeChange::parse("50%"), Some(SizeChange::Proportion(0.5)));
        assert_eq!(SizeChange::parse("junk"), None);
    }

    #[test]
    fn set_column_width_specs() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        assert!(ws.set_column_width(&SizeChange::Fixed(800.0)));
        assert_eq!(ws.columns[0].width, Some(ColumnWidth::Fixed(800.0)));
        assert!(ws.set_column_width(&SizeChange::Delta(-100.0)));
        assert_eq!(ws.columns[0].width, Some(ColumnWidth::Fixed(700.0)));
        assert!(ws.set_column_width(&SizeChange::Proportion(0.5)));
        assert_eq!(ws.columns[0].width, Some(ColumnWidth::Proportion(0.5)));
        // Full-width is reset by an explicit width.
        ws.toggle_full_width();
        assert!(ws.columns[0].is_full_width);
        assert!(ws.set_column_width(&SizeChange::Fixed(600.0)));
        assert!(!ws.columns[0].is_full_width);
    }

    #[test]
    fn maximize_toggles_state() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        assert!(!ws.columns[0].is_maximized);
        assert!(ws.toggle_maximized());
        assert!(ws.columns[0].is_maximized);
        assert!(ws.columns[0].is_full_width);
        assert!(ws.toggle_maximized());
        assert!(!ws.columns[0].is_maximized);
    }

    #[test]
    fn fullscreen_toggles_and_clears_on_removal() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window(B);
        assert!(ws.toggle_fullscreen());
        assert_eq!(ws.fullscreen_id, Some(B));
        assert!(ws.toggle_fullscreen());
        assert_eq!(ws.fullscreen_id, None);

        ws.toggle_fullscreen(); // B fullscreen again
        ws.remove_window(B);
        assert_eq!(ws.fullscreen_id, None, "state cleared with the window");
    }

    #[test]
    fn insert_column_at_positions_and_focuses() {
        let mut ws = Workspace::new();
        ws.add_window(A);
        ws.add_window(B);
        ws.add_window(C); // C focused
        // Drag A to the right of C.
        ws.remove_window(A);
        ws.insert_column_at(2, A);
        assert_eq!(
            ws.columns.iter().map(|c| c.focused_id()).collect::<Vec<_>>(),
            vec![Some(B), Some(C), Some(A)]
        );
        assert_eq!(ws.focused_id(), Some(A));
        // Out-of-range index is clamped to the end.
        ws.remove_window(A);
        ws.insert_column_at(99, A);
        assert_eq!(ws.columns.len(), 3);
        assert_eq!(ws.focused_id(), Some(A));
        // Drop into the middle of a column at a tile slot.
        ws.remove_window(A);
        ws.add_window_to_column(A, 0, 1); // between B and C's column
        assert_eq!(ws.columns.len(), 2);
        assert_eq!(
            ws.columns[0].tiles.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![B, A]
        );
        assert_eq!(ws.focused_id(), Some(A));
    }

    #[test]
    fn focus_column_edges() {
        let mut ws = Workspace::new();
        for i in 1..=4 {
            ws.add_window(i);
        }
        assert!(ws.focus_column_edge(Edge::First));
        assert_eq!(ws.focused_id(), Some(1));
        assert!(ws.focus_column_edge(Edge::Last));
        assert_eq!(ws.focused_id(), Some(4));
        // No-op returns false.
        assert!(!ws.focus_column_edge(Edge::Last));
    }

    #[test]
    fn focus_column_index() {
        let mut ws = Workspace::new();
        assert!(!ws.focus_column_index(0)); // empty workspace
        for i in 1..=3 {
            ws.add_window(i);
        }
        // add_window focuses the new column, so we're on the last one.
        assert!(!ws.focus_column_index(2)); // already focused
        assert_eq!(ws.focused_id(), Some(3));
        assert!(ws.focus_column_index(0));
        assert_eq!(ws.focused_id(), Some(1));
        assert!(ws.focus_column_index(1));
        assert_eq!(ws.focused_id(), Some(2));
        // Out of range clamps to the last column.
        assert!(ws.focus_column_index(8));
        assert_eq!(ws.focused_id(), Some(3));
        assert!(!ws.focus_column_index(8));
    }
}
