//! Apply workspace topology changes only when no camera/overview owns the old
//! indices. Floating ownership and transport view springs must share the remap.

use super::AppState;
use std::collections::HashSet;

impl AppState {
        /// Reflow/removal paths may append, but never compact before the new
        /// workspace-switch target has reached the transport.
        pub(super) fn reclaim_workspaces(
                &mut self,
                device: &str,
        ) {
                self.reconcile_workspace(device, false);
        }

        pub(super) fn maintain_workspace_tails(&mut self) {
                let devices: Vec<_> = self.monitors.iter().map(|m| m.device.clone()).collect();
                for device in devices {
                        self.layout.add_monitor(&device);
                        self.reconcile_workspace(&device, false);
                }
        }

        /// Retry on idle ticks, not just the next window removal. A removal
        /// during overview/switching must not leave empty workspaces forever.
        pub(super) fn compact_idle_workspaces(&mut self) {
                if self.suspended || self.interacting_window.is_some() {
                        return;
                }
                let devices: Vec<_> = self
                        .layout
                        .monitors
                        .iter()
                        .map(|ml| ml.device.clone())
                        .collect();
                let mut changed = false;
                for device in devices {
                        if !self.overviews.contains_key(&device)
                                && !self.transport.switching(&device)
                        {
                                changed |= self.reconcile_workspace(&device, true);
                        }
                }
                if changed {
                        self.reflow();
                }
        }

        fn reconcile_workspace(
                &mut self,
                device: &str,
                compact: bool,
        ) -> bool {
                let occupied: HashSet<_> = self
                        .floating
                        .values()
                        .filter(|fs| fs.device == device)
                        .map(|fs| fs.workspace_idx)
                        .collect();
                let Some(ml) = self.layout.monitor_mut(device) else {
                        return false;
                };
                let Some(remap) = ml.maintain_workspaces(&occupied, compact) else {
                        return false;
                };
                if remap.reindexed() {
                        for fs in self.floating.values_mut().filter(|fs| fs.device == device) {
                                fs.workspace_idx = remap.old_to_new[fs.workspace_idx]
                                        .expect("floating workspace is occupied");
                        }
                        self.transport.remap_workspaces(device, &remap);
                }
                log::debug!(
                        "workspaces[{device}]: count={} active={} compact={compact}",
                        ml.workspaces.len(),
                        ml.active_workspace_idx
                );
                true
        }
}
