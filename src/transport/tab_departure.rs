//! A background tab must emerge from behind its source, not cover that source
//! at full size before moving. Track the retained column, so selecting another
//! source tab mid-flight keeps the same occlusion without reviving hidden HWNDs.

use super::tab_state::TabSelection;
use crate::anim::AnimatedRect;
use crate::layout::{Column, ColumnId, MonitorLayout, WindowId, Workspace};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub(super) struct TabDepartures {
        sources: HashMap<WindowId, ColumnId>,
}

impl TabDepartures {
        /// Retire old departures BEFORE geometry is retargeted. A later resize
        /// must not revive a completed source-on-top constraint.
        pub(super) fn sync(
                &mut self,
                monitor: &MonitorLayout,
                tabs: &HashMap<ColumnId, TabSelection>,
                columns: &HashMap<WindowId, ColumnId>,
                rects: &HashMap<WindowId, AnimatedRect>,
        ) {
                self.sources.retain(|id, source| {
                        rects.get(id).is_some_and(|r| !r.finished())
                                && monitor.workspaces.iter().any(|ws| {
                                        // Another structural move owns the layer now.
                                        source_column(ws, *id, *source).is_some()
                                                && ws.find(*id).is_some_and(|(ci, _)| {
                                                        columns.get(id) == Some(&ws.columns[ci].id)
                                                })
                                })
                });
                let old_columns: HashSet<_> = columns.values().copied().collect();
                for ws in &monitor.workspaces {
                        for col in &ws.columns {
                                // Only extraction to a new column. Merges into
                                // an existing column already have their own order.
                                if old_columns.contains(&col.id) {
                                        continue;
                                }
                                for tile in &col.tiles {
                                        let Some(&source) = columns.get(&tile.id) else {
                                                continue;
                                        };
                                        if tabs.get(&source)
                                                .is_some_and(|old| old.hidden_member(tile.id))
                                                && source_column(ws, tile.id, source).is_some()
                                        {
                                                self.sources.insert(tile.id, source);
                                        }
                                }
                        }
                }
        }

        /// Keep the ordering through the last submitted frame and its immediate
        /// focus repair. At rest this never requests raises by itself; sync
        /// retires it before any new motion. Only already-rendered source faces
        /// are returned (including a source expanded to a stack mid-departure).
        pub(super) fn fronts(
                &self,
                ws: &Workspace,
        ) -> HashSet<WindowId> {
                self.sources
                        .iter()
                        .filter_map(|(&id, &source)| source_column(ws, id, source))
                        .flat_map(Column::rendered_ids)
                        .collect()
        }

        pub(super) fn moving_fronts(
                &self,
                monitor: &MonitorLayout,
                rects: &HashMap<WindowId, AnimatedRect>,
        ) -> HashSet<WindowId> {
                self.sources
                        .iter()
                        .filter(|(id, _)| rects.get(id).is_some_and(|r| !r.finished()))
                        .flat_map(|(&id, &source)| {
                                monitor.workspaces
                                        .iter()
                                        .filter_map(move |ws| source_column(ws, id, source))
                        })
                        .flat_map(Column::rendered_ids)
                        .collect()
        }

        pub(super) fn remove_window(
                &mut self,
                id: WindowId,
        ) {
                self.sources.remove(&id);
        }
}

fn source_column(
        ws: &Workspace,
        departing: WindowId,
        source: ColumnId,
) -> Option<&Column> {
        // Real activation of the departing window takes precedence. Moves to
        // another workspace/monitor or re-consumption also end the constraint.
        if ws.focused_id() == Some(departing) {
                return None;
        }
        let (ci, _) = ws.find(departing)?;
        if ws.columns[ci].id == source {
                return None;
        }
        ws.columns.iter().find(|col| col.id == source)
}
