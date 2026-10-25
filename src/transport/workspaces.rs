//! Remap a settled workspace camera without discarding independent tile and
//! horizontal view animation. The app defers this until overview/switch settle.

use super::Transport;
use crate::layout::workspaces::WorkspaceRemap;

impl Transport {
        pub fn remap_workspaces(
                &mut self,
                device: &str,
                remap: &WorkspaceRemap,
        ) {
                let Some(m) = self.monitors.get_mut(device) else {
                        return;
                };
                debug_assert!(
                        m.wsp.finished(),
                        "workspace camera must settle before reindexing"
                );
                m.active = remap.old_to_new[m.active].expect("active workspace retained");
                m.wsp.retarget(m.active as f64, self.switch);
                m.wsp.settle();
                m.view = std::mem::take(&mut m.view)
                        .into_iter()
                        .filter_map(|(old, spring)| {
                                remap.old_to_new
                                        .get(old)
                                        .copied()
                                        .flatten()
                                        .map(|new| (new, spring))
                        })
                        .collect();
        }
}
