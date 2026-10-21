//! Layout height-preset decoding, kept separate from the application/config
//! orchestration. Empty lists retain niri's default thirds/half/two-thirds.

use crate::layout::height::{self, HeightPreset};
use kdl::KdlNode;

pub(super) fn parse_height_presets(node: &KdlNode) -> Vec<HeightPreset> {
        let mut presets = Vec::new();
        if let Some(children) = node.children() {
                for child in children.nodes() {
                        let Some(value) = super::first_float_arg(child).filter(|v| v.is_finite())
                        else {
                                continue;
                        };
                        match child.name().value() {
                                "fixed" => presets
                                        .push(HeightPreset::Fixed(value.clamp(1.0, 100_000.0))),
                                "proportion" => presets
                                        .push(HeightPreset::Proportion(value.clamp(0.01, 100.0))),
                                _ => {},
                        }
                }
        }
        if presets.is_empty() {
                height::default_presets()
        } else {
                presets
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        #[test]
        fn presets_and_height_actions_parse() {
                let cfg = super::super::parse(r#"
                        layout { preset-window-heights { proportion 0.25; fixed 500; proportion 0.75; } }
                        binds {
                                Mod+Ctrl+Shift+R { switch-preset-window-height; }
                                Mod+Ctrl+R { reset-window-height; }
                                Mod+Ctrl+Shift+T { switch-preset-window-height-back; }
                        }
                "#).unwrap();
                assert_eq!(
                        cfg.layout.preset_window_heights,
                        vec![
                                HeightPreset::Proportion(0.25),
                                HeightPreset::Fixed(500.0),
                                HeightPreset::Proportion(0.75)
                        ]
                );
                for action in [
                        super::super::Action::SwitchPresetWindowHeight,
                        super::super::Action::SwitchPresetWindowHeightBack,
                        super::super::Action::ResetWindowHeight,
                ] {
                        assert!(cfg.binds.iter().any(|b| b.action == action));
                }
        }

        #[test]
        fn empty_list_keeps_defaults_and_invalid_values_are_clamped() {
                let empty = super::super::parse("layout { preset-window-heights {} }").unwrap();
                assert_eq!(
                        empty.layout.preset_window_heights,
                        height::default_presets()
                );
                let clamped = super::super::parse(
                        "layout { preset-window-heights { fixed -4; proportion 0; } }",
                )
                .unwrap();
                assert_eq!(
                        clamped.layout.preset_window_heights,
                        vec![HeightPreset::Fixed(1.0), HeightPreset::Proportion(0.01)]
                );
        }
}
