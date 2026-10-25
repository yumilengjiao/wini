use super::FloatState;
use crate::config::{self, Action};
use crate::layout::ColumnWidth;
use crate::layout::geometry::{LayoutParams, Struts};
use crate::layout::height::HeightPreset;

const WORK: (f64, f64, f64, f64) = (-1600.0, -200.0, 1200.0, 900.0);

fn params() -> LayoutParams {
        LayoutParams {
                gaps: 40.0, // Unlike tiled slots, floats do not subtract these.
                struts: Struts {
                        left: 20.0,
                        right: 80.0,
                        top: 30.0,
                        bottom: 70.0,
                },
                preset_column_widths: vec![
                        ColumnWidth::Fixed(700.0),
                        ColumnWidth::Proportion(0.5),
                        ColumnWidth::Fixed(300.0),
                ],
                preset_window_heights: vec![
                        HeightPreset::Fixed(200.0),
                        HeightPreset::Proportion(0.5),
                        HeightPreset::Fixed(600.0),
                ],
                ..Default::default()
        }
}

fn float() -> FloatState {
        FloatState::new("D".into(), 2, (-1400.0, -100.0, 400.0, 300.0))
}

fn apply(
        fs: &mut FloatState,
        action: Action,
        p: &LayoutParams,
) -> bool {
        fs.apply_geometry_action(&action, p, WORK, (0.0, 0.0))
}

#[test]
fn floating_percentages_are_absolute_and_deltas_use_working_area() {
        let p = params(); // Usable size 1100 x 800.
        let mut fs = float();
        assert!(apply(&mut fs, Action::SetColumnWidth("50%".into()), &p));
        assert_eq!(fs.w, 550.0);
        assert!(!apply(&mut fs, Action::SetColumnWidth("50%".into()), &p));
        apply(&mut fs, Action::SetColumnWidth("+10%".into()), &p);
        assert_eq!(fs.w, 660.0);
        apply(&mut fs, Action::SetColumnWidth("-10%".into()), &p);
        assert_eq!(fs.w, 550.0);
        apply(&mut fs, Action::SetWindowHeight("50%".into()), &p);
        assert_eq!(fs.h, 400.0);
        apply(&mut fs, Action::SetWindowHeight("+10%".into()), &p);
        assert_eq!(fs.h, 480.0);
        apply(&mut fs, Action::SetWindowHeight("-10%".into()), &p);
        assert_eq!((fs.x, fs.y, fs.w, fs.h), (-1400.0, -100.0, 550.0, 400.0));
        assert_eq!((&*fs.device, fs.workspace_idx), ("D", 2));
}

#[test]
fn floating_presets_search_by_size_then_cycle_in_config_order_both_directions() {
        let p = params();
        let mut fs = float();
        for expected in [700.0, 550.0, 300.0, 700.0] {
                apply(&mut fs, Action::SwitchPresetColumnWidth, &p);
                assert_eq!(fs.w, expected);
        }
        for expected in [300.0, 550.0, 700.0] {
                apply(&mut fs, Action::SwitchPresetColumnWidthBack, &p);
                assert_eq!(fs.w, expected);
        }
        for expected in [400.0, 600.0, 200.0] {
                apply(&mut fs, Action::SwitchPresetWindowHeight, &p);
                assert_eq!(fs.h, expected);
        }
        apply(&mut fs, Action::SwitchPresetWindowHeightBack, &p);
        assert_eq!(fs.h, 600.0);
        let mut reverse = float();
        apply(&mut reverse, Action::SwitchPresetColumnWidthBack, &p);
        assert_eq!(reverse.w, 300.0); // Last configured smaller size, not a sorted list.
        apply(&mut reverse, Action::SwitchPresetWindowHeightBack, &p);
        assert_eq!(reverse.h, 200.0);
}

#[test]
fn preset_search_skips_one_pixel_rounding_differences_and_wraps() {
        let p = params();
        let mut fs = float();
        fs.h = 399.0;
        apply(&mut fs, Action::SwitchPresetWindowHeight, &p);
        assert_eq!(fs.h, 600.0);
        fs.h = 201.0;
        fs.preset_height = None;
        apply(&mut fs, Action::SwitchPresetWindowHeightBack, &p);
        assert_eq!(fs.h, 600.0); // No strictly smaller candidate outside tolerance.
        let mut other = float();
        apply(&mut other, Action::SetColumnWidth(String::new()), &p);
        assert_eq!(other.preset_width, Some(0));
        assert_eq!(fs.preset_height, Some(2)); // Per-window, per-axis state.
}

#[test]
fn explicit_size_invalidates_only_its_axis_even_when_geometry_is_unchanged() {
        let p = params();
        let mut fs = float();
        apply(&mut fs, Action::SwitchPresetColumnWidth, &p);
        apply(&mut fs, Action::SwitchPresetWindowHeight, &p);
        assert!(!apply(&mut fs, Action::SetColumnWidth("700".into()), &p));
        assert_eq!((fs.preset_width, fs.preset_height), (None, Some(1)));
        apply(&mut fs, Action::SwitchPresetColumnWidth, &p);
        apply(&mut fs, Action::SetWindowHeight("+10".into()), &p);
        assert_eq!((fs.preset_width, fs.preset_height), (Some(0), None));
        assert_eq!(fs.h, 410.0);
}

#[test]
fn manual_move_keeps_presets_and_resize_clears_only_the_resized_axis() {
        let p = params();
        let mut fs = float();
        apply(&mut fs, Action::SwitchPresetColumnWidth, &p);
        apply(&mut fs, Action::SwitchPresetWindowHeight, &p);
        fs.adopt_interactive_rect((-1300.0, -50.0, 700.0, 400.0));
        assert_eq!((fs.preset_width, fs.preset_height), (Some(0), Some(1)));
        fs.adopt_interactive_rect((-1300.0, -50.0, 500.0, 400.0));
        assert_eq!((fs.preset_width, fs.preset_height), (None, Some(1)));
        fs.adopt_interactive_rect((-1300.0, -50.0, 500.0, 350.0));
        assert_eq!((fs.preset_width, fs.preset_height), (None, None));
        apply(&mut fs, Action::SwitchPresetWindowHeight, &p);
        assert_eq!(fs.h, 400.0);
}

#[test]
fn config_reload_resets_changed_preset_list_without_resizing() {
        let p = params();
        let mut fs = float();
        apply(&mut fs, Action::SwitchPresetColumnWidth, &p);
        apply(&mut fs, Action::SwitchPresetWindowHeight, &p);
        let mut new = p.clone();
        new.preset_column_widths = vec![ColumnWidth::Fixed(900.0)];
        fs.update_sizing_config(&p, &new);
        assert_eq!((fs.preset_width, fs.preset_height), (None, Some(1)));
        assert_eq!((fs.w, fs.h), (700.0, 400.0));
        apply(&mut fs, Action::SwitchPresetColumnWidthBack, &new);
        assert_eq!(fs.w, 900.0);
        let mut latest = new.clone();
        latest.preset_window_heights.clear();
        fs.update_sizing_config(&new, &latest);
        assert_eq!((fs.preset_width, fs.preset_height), (Some(0), None));
        assert!(!apply(&mut fs, Action::SwitchPresetWindowHeight, &latest));
        assert_eq!(fs.h, 400.0);
}

#[test]
fn minimum_sizes_clamp_geometry_but_do_not_stick_preset_cursor() {
        let mut p = params();
        p.preset_column_widths = vec![
                ColumnWidth::Fixed(100.0),
                ColumnWidth::Fixed(120.0),
                ColumnWidth::Fixed(700.0),
        ];
        let mut fs = float();
        fs.w = 50.0;
        for (idx, width) in [(0, 500.0), (1, 500.0), (2, 700.0)] {
                fs.apply_geometry_action(&Action::SwitchPresetColumnWidth, &p, WORK, (500.0, 60.1));
                assert_eq!(fs.w, width);
                assert_eq!(fs.preset_width, Some(idx));
        }
        fs.apply_geometry_action(
                &Action::SetWindowHeight("12.2".into()),
                &p,
                WORK,
                (500.0, 60.1),
        );
        assert_eq!(fs.h, 61.0);
        apply(&mut fs, Action::SetColumnWidth("1e308".into()), &p);
        assert_eq!(fs.w, 100_000.0);
}

#[test]
fn invalid_sizes_empty_presets_and_tiled_only_actions_are_noops() {
        let mut p = params();
        p.preset_column_widths.clear();
        p.preset_window_heights.clear();
        let mut fs = float();
        for action in [
                Action::SwitchPresetColumnWidth,
                Action::SwitchPresetColumnWidthBack,
                Action::SwitchPresetWindowHeight,
                Action::SwitchPresetWindowHeightBack,
                Action::ResetWindowHeight,
                Action::MaximizeColumn,
                Action::ToggleColumnTabbedDisplay,
        ] {
                assert!(!apply(&mut fs, action, &p));
        }
        for spec in ["junk", "NaN", "inf", "1e999", "NaN%", "+inf%"] {
                assert!(!apply(&mut fs, Action::SetColumnWidth(spec.into()), &p));
                assert!(!apply(&mut fs, Action::SetWindowHeight(spec.into()), &p));
        }
        assert_eq!((fs.x, fs.y, fs.w, fs.h), (-1400.0, -100.0, 400.0, 300.0));
        fs.preset_height = Some(999);
        apply(&mut fs, Action::SwitchPresetWindowHeight, &params());
        assert_eq!(fs.h, 400.0); // Stale indices still fall back safely.
        p.preset_column_widths = vec![ColumnWidth::Fixed(f64::NAN), ColumnWidth::Fixed(600.0)];
        apply(&mut fs, Action::SwitchPresetColumnWidth, &p);
        assert_eq!(fs.w, 600.0);
}

#[test]
fn percentage_dimensions_follow_work_area_on_all_taskbar_edges_and_negative_outputs() {
        let p = LayoutParams::default();
        for work in [
                (50.0, 0.0, 950.0, 800.0),
                (0.0, 0.0, 950.0, 800.0),
                (0.0, 50.0, 1000.0, 750.0),
                (0.0, 0.0, 1000.0, 750.0),
                (-2000.0, -800.0, 2000.0, 1500.0),
        ] {
                let mut fs = float();
                fs.apply_geometry_action(
                        &Action::SetColumnWidth("50%".into()),
                        &p,
                        work,
                        (0.0, 0.0),
                );
                fs.apply_geometry_action(
                        &Action::SetWindowHeight("50%".into()),
                        &p,
                        work,
                        (0.0, 0.0),
                );
                assert_eq!(
                        (fs.w, fs.h),
                        ((work.2 * 0.5).round(), (work.3 * 0.5).round())
                );
                assert!(fs.x + fs.w > work.0 && fs.x < work.0 + work.2);
                assert!(fs.y + fs.h > work.1 && fs.y < work.1 + work.3);
        }
}

#[test]
fn tiny_work_areas_and_oversized_struts_never_reverse_visibility_clamp() {
        let mut fs = float();
        let mut p = params();
        p.struts = Struts {
                left: 1000.0,
                right: 1000.0,
                top: 1000.0,
                bottom: 1000.0,
        };
        let work = (-80.0, -40.0, 40.0, 30.0);
        fs.apply_geometry_action(&Action::SetColumnWidth("1".into()), &p, work, (0.0, 0.0));
        fs.apply_geometry_action(&Action::SetWindowHeight("1".into()), &p, work, (0.0, 0.0));
        assert_eq!((fs.w, fs.h), (1.0, 1.0));
        assert!(fs.x >= work.0 && fs.x + fs.w <= work.0 + work.2);
        assert!(fs.y >= work.1 && fs.y + fs.h <= work.1 + work.3);
}

#[test]
fn configured_actions_apply_to_floats_without_new_key_bindings() {
        let cfg = config::parse(
                r#"
layout {
    preset-column-widths { fixed 600; fixed 900; }
    preset-window-heights { fixed 400; fixed 700; }
}
binds {
    Mod+R { switch-preset-column-width; }
    Mod+Shift+R { switch-preset-column-width-back; }
    Mod+Ctrl+Shift+R { switch-preset-window-height; }
    Mod+Ctrl+Alt+R { switch-preset-window-height-back; }
}
"#,
        )
        .unwrap();
        assert_eq!(cfg.binds.len(), 4);
        let mut fs = float();
        for (bind, (w, h)) in cfg.binds.iter().zip([
                (600.0, 300.0),
                (900.0, 300.0),
                (900.0, 400.0),
                (900.0, 700.0),
        ]) {
                fs.apply_geometry_action(&bind.action, &cfg.layout, WORK, (0.0, 0.0));
                assert_eq!((fs.w, fs.h), (w, h));
        }
}
