//! Tab indicator orchestration and selection. Model/geometry lives in
//! layout::tabbed, painting in win::tab_indicator; this only joins them.

use super::AppState;
use crate::layout::tabbed::ColumnDisplay;
use crate::win::tab_indicator::TabMark;
use windows::Win32::Foundation::HWND;

impl AppState {
        pub(super) fn update_tab_indicators(&mut self) {
                if self.suspended || self.interacting_window.is_some() {
                        if let Some(overlay) = self.tab_overlay.as_mut() {
                                overlay.update(Vec::new());
                        }
                        crate::input::mouse::set_tab_regions(Vec::new());
                        return;
                }
                let cfg = &self.params.tab_indicator;
                let mut marks = Vec::new();
                for mon in &self.monitors {
                        let Some(ml) = self.layout.monitor(&mon.device) else {
                                continue;
                        };
                        let host = self.overview_hosts.get(&mon.device);
                        let zoom = self
                                .overviews
                                .get(&mon.device)
                                .map(|ov| ov.zoom())
                                .unwrap_or(1.0);
                        for (k, ws) in ml.workspaces.iter().enumerate() {
                                if host.is_none() && k != ml.active_workspace_idx {
                                        continue;
                                }
                                for col in &ws.columns {
                                        if col.display != ColumnDisplay::Tabbed
                                                || col.is_maximized
                                                || col.is_edge_maximized
                                                || ws.fullscreen_id.is_some()
                                        {
                                                continue;
                                        }
                                        let Some(active) = col.focused_id() else {
                                                continue;
                                        };
                                        let area = if let Some(host) = host {
                                                host.visible_rect(active)
                                        } else {
                                                if self.hidden_by_us.contains(&active) {
                                                        continue;
                                                }
                                                crate::win::frame::frame_rect(
                                                        HWND(active as *mut _),
                                                )
                                                .map(|(x, y, w, h)| {
                                                        (
                                                                x.round() as i32,
                                                                y.round() as i32,
                                                                w.round() as i32,
                                                                h.round() as i32,
                                                        )
                                                })
                                        };
                                        let Some(area) = area else {
                                                continue;
                                        };
                                        let rects = cfg.rects(area, col.tiles.len(), zoom);
                                        for (tile, rect) in col.tiles.iter().zip(rects) {
                                                let (x, y, w, h) = rect;
                                                let l = x.max(mon.work.left);
                                                let t = y.max(mon.work.top);
                                                let r = (x + w).min(mon.work.right);
                                                let b = (y + h).min(mon.work.bottom);
                                                if l >= r || t >= b {
                                                        continue;
                                                }
                                                marks.push(TabMark {
                                                        id: tile.id,
                                                        rect: (l, t, r - l, b - t),
                                                        color: if tile.id == active {
                                                                cfg.active_color
                                                        } else {
                                                                cfg.inactive_color
                                                        },
                                                        radius: (cfg.radius as f64 * zoom).round()
                                                                as i32,
                                                });
                                        }
                                }
                        }
                }
                if let Some(overlay) = self.tab_overlay.as_mut() {
                        overlay.update(marks);
                        crate::input::mouse::set_tab_regions(
                                overlay.marks()
                                        .iter()
                                        .map(|m| {
                                                (
                                                        m.rect.0,
                                                        m.rect.1,
                                                        m.rect.0 + m.rect.2,
                                                        m.rect.1 + m.rect.3,
                                                )
                                        })
                                        .collect(),
                        );
                }
        }

        pub(super) fn click_tab(
                &mut self,
                x: i32,
                y: i32,
        ) -> bool {
                let Some(id) = self.tab_overlay.as_ref().and_then(|o| {
                        o.marks()
                                .iter()
                                .find(|m| {
                                        x >= m.rect.0
                                                && x < m.rect.0 + m.rect.2
                                                && y >= m.rect.1
                                                && y < m.rect.1 + m.rect.3
                                })
                                .map(|m| m.id)
                }) else {
                        return false;
                };
                let Some(device) = self
                        .layout
                        .monitors
                        .iter()
                        .find_map(|ml| ml.workspace_of(id).map(|_| ml.device.clone()))
                else {
                        return false;
                };
                if !self.layout.focus_window(id) {
                        return false;
                }
                self.focused = Some(HWND(id as *mut _));
                self.update_focus_view(id);
                self.reflow();
                if self.overviews.contains_key(&device) {
                        let Some(ml) = self.layout.monitor(&device) else {
                                return true;
                        };
                        let idx = ml.active_workspace_idx;
                        self.overview_activate(&device, idx, false);
                } else {
                        self.sync_focus_to_os();
                }
                true
        }
}
