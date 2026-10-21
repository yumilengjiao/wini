//! Sizing-mode backend orchestration. Keep decoration reconciliation out of
//! action dispatch so moves, window rules and tab transitions use it too.

use super::AppState;
use windows::Win32::Foundation::HWND;

impl AppState {
        pub(super) fn sync_fullscreen_decorations(&mut self) {
                let fullscreen: std::collections::HashSet<_> = self
                        .layout
                        .monitors
                        .iter()
                        .flat_map(|ml| ml.workspaces.iter())
                        .filter_map(|ws| ws.fullscreen_id)
                        .collect();
                let mut changed = self
                        .decorations
                        .ids()
                        .into_iter()
                        .filter(|id| !fullscreen.contains(id))
                        .collect::<Vec<_>>();
                for &id in &changed {
                        self.decorations.restore(id);
                }
                let saved = self.decorations.ids();
                for id in fullscreen {
                        if !saved.contains(&id) {
                                changed.push(id);
                        }
                        self.decorations.make_borderless(HWND(id as *mut _));
                }
                for host in self.overview_hosts.values_mut() {
                        for &id in &changed {
                                host.refresh_frame_insets(id);
                        }
                }
        }
}
