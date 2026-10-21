//! Niri-style tab-indicator configuration (solid-color Windows subset).

use crate::layout::tabbed::{TabIndicatorConfig, TabPosition};
use kdl::KdlNode;

pub(super) fn parse(
        node: &KdlNode,
        cfg: &mut TabIndicatorConfig,
) {
        let Some(doc) = node.children() else {
                return;
        };
        for n in doc.nodes() {
                let number = super::first_float_arg(n).filter(|v| v.is_finite());
                match n.name().value() {
                        "off" => cfg.enabled = false,
                        "on" => cfg.enabled = true,
                        "hide-when-single-tab" => {
                                cfg.hide_when_single_tab = super::first_bool_arg(n).unwrap_or(true)
                        },
                        "place-within-column" => {
                                cfg.place_within_column = super::first_bool_arg(n).unwrap_or(true)
                        },
                        "position" => {
                                if let Some(p) = super::first_string_arg(n) {
                                        cfg.position = match p.as_str() {
                                                "left" => TabPosition::Left,
                                                "right" => TabPosition::Right,
                                                "top" => TabPosition::Top,
                                                "bottom" => TabPosition::Bottom,
                                                _ => {
                                                        log::warn!(
                                                                "unknown tab-indicator position {p:?}"
                                                        );
                                                        continue;
                                                },
                                        };
                                }
                        },
                        "width" => {
                                if let Some(v) = number {
                                        cfg.width = v.clamp(1.0, 1000.0);
                                }
                        },
                        "gap" => {
                                if let Some(v) = number {
                                        cfg.gap = v.clamp(-1000.0, 1000.0);
                                }
                        },
                        "gaps-between-tabs" => {
                                if let Some(v) = number {
                                        cfg.gaps_between_tabs = v.clamp(0.0, 1000.0);
                                }
                        },
                        "corner-radius" => {
                                if let Some(v) = number {
                                        cfg.radius = v.clamp(0.0, 1000.0) as i32;
                                }
                        },
                        "length" => {
                                if let Some(v) = n
                                        .get("total-proportion")
                                        .and_then(|e| {
                                                e.as_float().or_else(|| {
                                                        e.as_integer().map(|i| i as f64)
                                                })
                                        })
                                        .filter(|v| v.is_finite())
                                {
                                        cfg.total_proportion = v.clamp(0.0, 2.0);
                                }
                        },
                        "active-color" | "inactive-color" => {
                                if let Some(color) = super::first_string_arg(n)
                                        .and_then(|s| {
                                                u32::from_str_radix(s.trim_start_matches('#'), 16)
                                                        .ok()
                                        })
                                        .filter(|&c| c <= 0xffffff)
                                {
                                        if n.name().value() == "active-color" {
                                                cfg.active_color = color;
                                        } else {
                                                cfg.inactive_color = color;
                                        }
                                }
                        },
                        name => log::debug!("unsupported tab-indicator option {name:?}"),
                }
        }
}

#[cfg(test)]
mod tests {
        use super::*;
        #[test]
        fn recommended_green_indicator_config_parses_without_changing_layout_gaps() {
                let cfg = super::super::parse(
                        r##"
                        layout {
                            gaps 1
                            tab-indicator {
                                position "left"
                                place-within-column
                                width 10
                                gap 12
                                length total-proportion=0.6
                                gaps-between-tabs 5
                                corner-radius 5
                                active-color "#7daea3"
                                inactive-color "#355f55"
                            }
                        }
                "##,
                )
                .unwrap();
                let t = &cfg.layout.tab_indicator;
                assert_eq!(cfg.layout.gaps, 1.0);
                assert!(t.place_within_column);
                assert_eq!((t.width, t.gap, t.gaps_between_tabs), (10.0, 12.0, 5.0));
                assert_eq!((t.active_color, t.inactive_color), (0x7daea3, 0x355f55));
        }

        #[test]
        fn tabbed_action_and_indicator_options_parse() {
                let source = r##"
                        layout {
                            tab-indicator {
                                hide-when-single-tab
                                place-within-column #true
                                position "top"
                                width 5
                                gap 6
                                gaps-between-tabs 2
                                length total-proportion=0.75
                                corner-radius 3
                                active-color "#00ff00"
                                inactive-color "#123456"
                            }
                        }
                        binds { Mod+W { toggle-column-tabbed-display; } }
                "##;
                let _: kdl::KdlDocument = source.parse().unwrap_or_else(|e| panic!("{e:?}"));
                let cfg = super::super::parse(source).unwrap();
                let t = &cfg.layout.tab_indicator;
                assert!(t.hide_when_single_tab && t.place_within_column);
                assert_eq!(t.position, TabPosition::Top);
                assert_eq!((t.width, t.gap, t.total_proportion), (5.0, 6.0, 0.75));
                assert_eq!(
                        (t.radius, t.active_color, t.inactive_color),
                        (3, 0x00ff00, 0x123456)
                );
                assert!(cfg
                        .binds
                        .iter()
                        .any(|b| b.action == super::super::Action::ToggleColumnTabbedDisplay));
        }
}
