//! KDL configuration, niri-flavored.
//!
//! Example config:
//!
//! ```kdl
//! input {
//!     keyboard-shortcuts {
//!         Mod="Alt"   // which physical key acts as the Mod key
//!     }
//!     // focus-follows-mouse   // hovering a window focuses it
//! }
//!
//! layout {
//!     gaps 8
//!     edge-padding 8
//!     default-column-width { proportion 0.25; }
//!     center-focused-column "on-overflow"   // never | on-overflow | always
//! }
//!
//! animations {
//!     // off
//!     // slowdown 3.0
//!     window-movement {
//!         spring damping-ratio=1.0 stiffness=800 epsilon=0.0001
//!     }
//!     view-offset {
//!         duration-ms 100
//!         curve "ease-out-expo"
//!     }
//! }
//!
//! binds {
//!     Mod+Left  { focus-column-left; }
//!     Mod+Right { focus-column-right; }
//!     Mod+H     { focus-column-left; }
//!     Mod+L     { focus-column-right; }
//!     Mod+J     { focus-window-down; }
//!     Mod+K     { focus-window-up; }
//!     Mod+Q     { close-window; }
//!     Mod+Shift+E { quit; }
//! }
//! ```
//!
//! Config lives at `%APPDATA%\yumi-wini\config.kdl` (overridable with
//! `YUMI_WINI_CONFIG`). Missing file = defaults.

// The input module consumes this in the next commits; silence interim
// dead-code warnings.
#![allow(dead_code)]

use std::path::PathBuf;

use crate::anim::{AnimKind, AnimParams, Curve, SpringParams};
use crate::layout::ColumnWidth;
use crate::layout::geometry::{CenterFocused, LayoutParams};

use kdl::{KdlDocument, KdlNode};

/// A parsed key binding: key combo -> action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    /// e.g. "Mod+Shift+Left", lowercase key names.
    pub combo: String,
    pub action: Action,
}

/// Everything the WM can do from a key press. Mirrors the niri action
/// names we intend to support (a growing subset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    CloseWindow,
    Spawn(String),
    FocusColumnLeft,
    FocusColumnRight,
    FocusColumnFirst,
    FocusColumnLast,
    /// Focus the Nth column of the workspace (1-based).
    FocusColumnIndex(u8),
    FocusWindowDown,
    FocusWindowUp,
    MoveColumnLeft,
    MoveColumnRight,
    MoveWindowDown,
    MoveWindowUp,
    MoveWindowToColumnLeft,
    MoveWindowToColumnRight,
    ConsumeOrExpelWindowLeft,
    ConsumeOrExpelWindowRight,
    SetColumnWidth(String),
    /// niri's switch-preset-column-width (Mod+R): cycle the focused
    /// column through layout.preset-column-widths.
    SwitchPresetColumnWidth,
    ToggleFullWidth,
    MaximizeColumn,
    ToggleWindowedFullscreen,
    SetWindowHeight(String),
    FocusWorkspace(u8),
    WorkspaceSwitch(u8),
    MoveWindowToWorkspace(u8),
    MoveColumnToWorkspace(u8),
    ToggleWindowFloating,
    ToggleOverview,
    /// Niri's do-screen-transition: cover the screen briefly while
    /// management is suspended (screenshot privacy).
    DoScreenTransition,
    Unknown(String),
}

/// Which physical key acts as "Mod".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModKey {
    Alt,
    Super,
    Ctrl,
}

/// Animation tuning, per animation kind (niri-style `animations` node).
/// Kinds we can't drive (window-open/close need shader-level compositing;
/// workspace-switch needs workspace slide rendering) are parsed and
/// validated so configs port over, but not animated yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationsConfig {
    /// Master switch (`animations { off; }` disables everything by
    /// forcing instant geometry).
    pub enabled: bool,
    /// Niri's global `slowdown` multiplier: values > 1 slow every
    /// animation down (durations scale by it).
    pub slowdown: f64,
    /// Window moves (x/y).
    pub window_movement: AnimKind,
    /// Window resizes (w/h).
    pub window_resize: AnimKind,
    /// Viewport scrolling (focus column left/right).
    pub view_offset: AnimKind,
    /// Parsed but not animated (open/close fade needs compositing).
    pub window_open: AnimKind,
    /// Parsed but not animated.
    pub window_close: AnimKind,
    /// Parsed but not animated (needs workspace slide rendering).
    pub workspace_switch: AnimKind,
}

impl Default for AnimationsConfig {
    fn default() -> Self {
        // Niri's defaults: springs for movement/resize/view-offset,
        // short easings for open/close.
        AnimationsConfig {
            enabled: true,
            slowdown: 1.0,
            window_movement: AnimKind::Spring(SpringParams::niri_default()),
            window_resize: AnimKind::Spring(SpringParams::niri_default()),
            view_offset: AnimKind::Spring(SpringParams::niri_default()),
            window_open: AnimKind::Easing {
                duration: std::time::Duration::from_millis(150),
                curve: Curve::EaseOutExpo,
            },
            window_close: AnimKind::Easing {
                duration: std::time::Duration::from_millis(150),
                curve: Curve::EaseOutQuad,
            },
            workspace_switch: AnimKind::Spring(SpringParams {
                damping_ratio: 1.0,
                stiffness: 1000.0,
                epsilon: 0.0001,
            }),
        }
    }
}

impl AnimationsConfig {
    fn params(&self, kind: AnimKind) -> AnimParams {
        AnimParams {
            kind: if self.enabled {
                kind
            } else {
                // `animations { off; }` forces instant geometry.
                AnimKind::instant()
            },
            slowdown: self.slowdown,
        }
    }

    /// Driving parameters for window moves (x/y).
    pub fn movement_params(&self) -> AnimParams {
        self.params(self.window_movement)
    }

    /// Driving parameters for window resizes (w/h).
    pub fn resize_params(&self) -> AnimParams {
        self.params(self.window_resize)
    }

    /// Driving parameters for viewport scrolling. Not wired into the
    /// animator yet (scrolling animates windows directly) but kept
    /// for parity.
    #[allow(dead_code)]
    pub fn view_offset_params(&self) -> AnimParams {
        self.params(self.view_offset)
    }
}

/// Niri's `layout { focus-ring { ... } }`: the outline drawn around the
/// focused window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusRingConfig {
    pub enabled: bool,
    /// Ring thickness in pixels.
    pub width: i32,
    /// Active color as 0xRRGGBB.
    pub active_color: u32,
}

impl Default for FocusRingConfig {
    fn default() -> Self {
        FocusRingConfig {
            enabled: true,
            width: 4,
            active_color: 0x7D_AEA3,
        }
    }
}

/// A niri-style window rule: when a newly opened window matches, the
/// rule's `open-*` properties are applied to it.
/// Matching is case-insensitive substring on exe (niri's app-id
/// equivalent), title and/or class; all specified fields must match.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowRule {
    pub match_exe: Option<String>,
    pub match_title: Option<String>,
    pub match_class: Option<String>,
    pub open_floating: bool,
    /// 1-based workspace index.
    pub open_workspace: Option<u8>,
    pub open_maximized: bool,
    pub open_fullscreen: bool,
}

impl WindowRule {
    /// Does a window with these strings match this rule?
    pub fn matches(&self, exe: &str, title: &str, class: &str) -> bool {
        let sub = |hay: &str, needle: &Option<String>| match needle {
            Some(n) => hay.to_lowercase().contains(&n.to_lowercase()),
            None => true,
        };
        sub(exe, &self.match_exe) && sub(title, &self.match_title) && sub(class, &self.match_class)
    }
}

impl Config {
    /// First rule matching the given window, if any.
    pub fn match_rule(&self, exe: &str, title: &str, class: &str) -> Option<&WindowRule> {
        self.window_rules
            .iter()
            .find(|r| r.matches(exe, title, class))
    }
}

/// Full configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub mod_key: ModKey,
    /// `input { focus-follows-mouse; }` — hovering a window focuses it
    /// (niri's optional hover focus).
    pub focus_follows_mouse: bool,
    pub layout: LayoutParams,
    pub focus_ring: FocusRingConfig,
    pub animations: AnimationsConfig,
    pub binds: Vec<Bind>,
    pub window_rules: Vec<WindowRule>,
    /// `spawn-at-startup "cmd"` entries, in order.
    pub spawn_at_startup: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mod_key: ModKey::Alt,
            focus_follows_mouse: false,
            layout: LayoutParams::default(),
            focus_ring: FocusRingConfig::default(),
            animations: AnimationsConfig::default(),
            window_rules: Vec::new(),
            spawn_at_startup: Vec::new(),
            // niri's default binds (navigation subset we implement).
            binds: vec![
                bind("Mod+Left", Action::FocusColumnLeft),
                bind("Mod+Down", Action::FocusWindowDown),
                bind("Mod+Up", Action::FocusWindowUp),
                bind("Mod+Right", Action::FocusColumnRight),
                bind("Mod+H", Action::FocusColumnLeft),
                bind("Mod+J", Action::FocusWindowDown),
                bind("Mod+K", Action::FocusWindowUp),
                bind("Mod+L", Action::FocusColumnRight),
                bind("Mod+Shift+Left", Action::MoveColumnLeft),
                bind("Mod+Shift+Down", Action::MoveWindowDown),
                bind("Mod+Shift+Up", Action::MoveWindowUp),
                bind("Mod+Shift+Right", Action::MoveColumnRight),
                bind("Mod+Shift+H", Action::MoveColumnLeft),
                bind("Mod+Shift+J", Action::MoveWindowDown),
                bind("Mod+Shift+K", Action::MoveWindowUp),
                bind("Mod+Shift+L", Action::MoveColumnRight),
                bind("Mod+Ctrl+Left", Action::MoveWindowToColumnLeft),
                bind("Mod+Ctrl+Right", Action::MoveWindowToColumnRight),
                bind("Mod+Ctrl+H", Action::MoveWindowToColumnLeft),
                bind("Mod+Ctrl+L", Action::MoveWindowToColumnRight),
                bind("Mod+Ctrl+J", Action::ConsumeOrExpelWindowLeft),
                bind("Mod+Ctrl+K", Action::ConsumeOrExpelWindowRight),
                bind("Mod+Page_Down", Action::FocusColumnRight),
                bind("Mod+Page_Up", Action::FocusColumnLeft),
                // Mouse wheel: niri's defaults — wheel moves column focus
                // (which scrolls the view).
                bind("Mod+WheelScrollDown", Action::FocusColumnRight),
                bind("Mod+WheelScrollUp", Action::FocusColumnLeft),
                bind("Mod+Ctrl+WheelScrollDown", Action::MoveColumnRight),
                bind("Mod+Ctrl+WheelScrollUp", Action::MoveColumnLeft),
                bind("Mod+Home", Action::FocusColumnFirst),
                bind("Mod+End", Action::FocusColumnLast),
                bind("Mod+F", Action::MaximizeColumn),
                bind("Mod+Shift+F", Action::ToggleWindowedFullscreen),
                // Cycle preset column widths (niri: 1/3, 1/2, 2/3).
                bind("Mod+R", Action::SwitchPresetColumnWidth),
                bind("Mod+V", Action::ToggleWindowFloating),
                bind("Mod+Q", Action::CloseWindow),
                bind("Mod+Shift+Slash", Action::ToggleOverview),
                bind("Mod+Shift+E", Action::Quit),
            ],
        }
        .with_workspace_binds()
    }
}

fn bind(combo: &str, action: Action) -> Bind {
    Bind {
        combo: combo.to_string(),
        action,
    }
}

impl Config {
    /// Append the numeric binds: Mod+N / Mod+Ctrl+N switch and move
    /// workspaces (niri defaults), and Mod+Shift+N focuses the Nth
    /// column (our extension; niri has no column-index focus).
    fn with_workspace_binds(mut self) -> Self {
        for n in 1..=9u8 {
            self.binds
                .push(bind(&format!("Mod+{n}"), Action::FocusWorkspace(n)));
            self.binds.push(bind(
                &format!("Mod+Ctrl+{n}"),
                Action::MoveColumnToWorkspace(n),
            ));
            self.binds
                .push(bind(&format!("Mod+Shift+{n}"), Action::FocusColumnIndex(n)));
        }
        self
    }
}

/// Locate the config file without reading it.
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("YUMI_WINI_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("yumi-wini").join("config.kdl")
}

/// Load and parse the config; falls back to defaults on any problem
/// (with a logged warning). Never fails hard — a broken config should
/// not brick the WM.
pub fn load() -> Config {
    match try_load() {
        Ok(cfg) => {
            log::info!("loaded config from {}", config_path().display());
            cfg
        }
        Err(err) => {
            if err.starts_with("missing") {
                log::info!(
                    "no config at {} — using defaults (drop one there to customize)",
                    config_path().display()
                );
            } else {
                log::warn!("config parse error ({err}); using defaults");
            }
            Config::default()
        }
    }
}

/// Load and parse the config, reporting errors instead of falling
/// back — used by the hot-reload path, which keeps the previous
/// config when the new one is broken.
pub fn try_load() -> Result<Config, String> {
    let path = config_path();
    let source = std::fs::read_to_string(&path).map_err(|e| format!("missing/unreadable: {e}"))?;
    parse(&source)
}

/// Parse KDL text into a Config. Unknown nodes/fields are ignored
/// (forward compatibility, like niri).
pub fn parse(source: &str) -> Result<Config, String> {
    let doc: KdlDocument = source.parse().map_err(|e| format!("KDL syntax: {e}"))?;

    let mut config = Config::default();

    let mut has_binds = false;
    for node in doc.nodes() {
        match node.name().value() {
            "input" => parse_input(node, &mut config),
            "layout" => parse_layout(node, &mut config),
            "animations" => parse_animations(node, &mut config),
            "binds" => {
                // A binds section replaces ALL default binds (niri
                // semantics) — but only when actually present: a
                // config that only tweaks, say, animations keeps the
                // defaults.
                if !has_binds {
                    config.binds.clear();
                    has_binds = true;
                }
                parse_binds(node, &mut config);
            }
            "window-rule" => parse_window_rule(node, &mut config),
            "spawn-at-startup" => {
                if let Some(cmd) = first_string_arg(node) {
                    config.spawn_at_startup.push(cmd);
                }
            }
            other => log::debug!("ignoring unknown config node {other:?}"),
        }
    }
    Ok(config)
}

fn parse_input(node: &KdlNode, config: &mut Config) {
    // `input { focus-follows-mouse; }` (a bare flag node).
    if let Some(doc) = node.children()
        && doc
            .nodes()
            .iter()
            .any(|n| n.name().value() == "focus-follows-mouse")
    {
        config.focus_follows_mouse = true;
    }
    let Some(doc) = node.children() else { return };
    for n in doc.nodes() {
        if n.name().value() == "keyboard-shortcuts" {
            // `Mod "Super"` inside keyboard-shortcuts is a child node
            // whose first argument names the modifier key.
            let mod_name = n
                .children()
                .and_then(|c| {
                    c.nodes()
                        .iter()
                        .find(|m| m.name().value() == "Mod")
                        .and_then(first_string_arg)
                })
                .unwrap_or_else(|| "Alt".to_string());
            config.mod_key = match mod_name.as_str() {
                "Super" | "Super_L" | "Logo" => ModKey::Super,
                "Ctrl" | "Control" => ModKey::Ctrl,
                _ => ModKey::Alt,
            };
        }
    }
}

fn parse_layout(node: &KdlNode, config: &mut Config) {
    let Some(doc) = node.children() else { return };
    for n in doc.nodes() {
        match n.name().value() {
            "gaps" => {
                if let Some(v) = first_float_arg(n) {
                    config.layout.gaps = v;
                }
            }
            "edge-padding" => {
                if let Some(v) = first_float_arg(n) {
                    config.layout.edge_padding = v;
                }
            }
            "center-focused-column" => {
                if let Some(v) = first_string_arg(n) {
                    config.layout.center_focused_column = match v.as_str() {
                        "never" => CenterFocused::Never,
                        "always" => CenterFocused::Always,
                        _ => CenterFocused::OnOverflow,
                    };
                }
            }
            "default-column-width" => {
                config.layout.default_column_width = parse_width_node(n);
            }
            "preset-column-widths" => {
                config.layout.preset_column_widths = parse_preset_widths(n);
            }
            "focus-ring" => parse_focus_ring(n, config),
            other => log::debug!("ignoring layout node {other:?}"),
        }
    }
}

/// `default-column-width { proportion 0.5; }` or `{ fixed 1280; }` or
/// a bare `default-column-width 800`.
fn parse_width_node(n: &KdlNode) -> ColumnWidth {
    if let Some(f) = first_float_arg(n) {
        return ColumnWidth::Fixed(f);
    }
    let Some(doc) = n.children() else {
        return ColumnWidth::Proportion(0.25);
    };
    for child in doc.nodes() {
        match child.name().value() {
            "proportion" => {
                if let Some(p) = first_float_arg(child) {
                    return ColumnWidth::Proportion(p.clamp(0.01, 100.0));
                }
            }
            "fixed" => {
                if let Some(f) = first_float_arg(child) {
                    return ColumnWidth::Fixed(f.clamp(1.0, 100_000.0));
                }
            }
            _ => {}
        }
    }
    ColumnWidth::Proportion(0.25)
}

/// `window-rule { match app-id="firefox" title="download"; open-floating true; }`
/// niri names accepted: app-id maps to the exe name; also `exe=`.
fn parse_window_rule(node: &KdlNode, config: &mut Config) {
    let mut rule = WindowRule::default();
    let Some(doc) = node.children() else { return };
    for n in doc.nodes() {
        match n.name().value() {
            "match" => {
                for e in n.entries() {
                    let Some(name) = e.name().map(|s| s.value().to_string()) else {
                        continue;
                    };
                    let Some(v) = e.value().as_string().map(|s| s.to_string()) else {
                        continue;
                    };
                    match name.as_str() {
                        "app-id" | "exe" => rule.match_exe = Some(v),
                        "title" => rule.match_title = Some(v),
                        "class" => rule.match_class = Some(v),
                        _ => log::debug!("ignoring window-rule match field {name:?}"),
                    }
                }
            }
            "open-floating" => rule.open_floating = first_bool_arg(n).unwrap_or(true),
            "open-on-workspace" => {
                rule.open_workspace = first_string_arg(n)
                    .and_then(|s| s.parse().ok())
                    .or_else(|| first_float_arg(n).map(|f| f as u8))
            }
            "open-maximized" => rule.open_maximized = first_bool_arg(n).unwrap_or(true),
            "open-fullscreen" => rule.open_fullscreen = first_bool_arg(n).unwrap_or(true),
            other => log::debug!("ignoring window-rule node {other:?}"),
        }
    }
    config.window_rules.push(rule);
}

/// `preset-column-widths { proportion 0.33; proportion 0.5; fixed 1280; }`
/// — a list of widths cycled through by a bare `set-column-width`.
fn parse_preset_widths(n: &KdlNode) -> Vec<ColumnWidth> {
    let mut out = Vec::new();
    let Some(doc) = n.children() else { return out };
    for child in doc.nodes() {
        let w = match child.name().value() {
            "proportion" => {
                first_float_arg(child).map(|p| ColumnWidth::Proportion(p.clamp(0.01, 100.0)))
            }
            "fixed" => first_float_arg(child).map(|f| ColumnWidth::Fixed(f.clamp(1.0, 100_000.0))),
            _ => None,
        };
        if let Some(w) = w {
            out.push(w);
        }
    }
    out
}

/// `focus-ring { off; width 4; active-color "#7daea3"; }` (naming
/// follows niri; a bare `focus-ring 4;` sets the width).
fn parse_focus_ring(n: &KdlNode, config: &mut Config) {
    if let Some(v) = first_float_arg(n) {
        config.focus_ring.width = v.max(1.0) as i32;
    }
    let Some(doc) = n.children() else { return };
    for c in doc.nodes() {
        match c.name().value() {
            "off" => config.focus_ring.enabled = false,
            "width" => {
                if let Some(v) = first_float_arg(c) {
                    config.focus_ring.width = v.max(1.0) as i32;
                }
            }
            "active-color" => {
                if let Some(s) = first_string_arg(c)
                    && let Some(rgb) = parse_hex_color(&s)
                {
                    config.focus_ring.active_color = rgb;
                }
            }
            _ => {}
        }
    }
}

/// "#7daea3" (or "7daea3") -> 0x7DAEA3.
fn parse_hex_color(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// `animations { off; slowdown 2.0; window-movement { spring ...; } }`
///
/// Follows niri's syntax. Each per-animation node may contain:
/// - `off` — that animation lands instantly
/// - `spring damping-ratio=1.0 stiffness=800 epsilon=0.0001` (niri's
///   property syntax; the nested `spring { damping-ratio 1.0; }` form
///   is accepted too)
/// - `duration-ms 250` + `curve "ease-out-expo"`, with
///   `curve "cubic-bezier" 0.05 0.7 0.1 1.0` for custom curves
///   (`easing "..."` from our older configs is an alias for `curve`)
///
/// Spring and easing parameters must not be mixed (niri errors out;
/// we warn and keep the first). Unset fields keep their defaults
/// (niri's merge semantics).
fn parse_animations(node: &KdlNode, config: &mut Config) {
    // `animations "off";` as an argument, or `animations { off; }`
    // as a child node — either form disables everything.
    let off_arg = node
        .entries()
        .iter()
        .any(|e| e.name().is_none() && e.value().as_string() == Some("off"));
    let off_node = node
        .children()
        .is_some_and(|c| c.nodes().iter().any(|n| n.name().value() == "off"));
    if off_arg || off_node {
        config.animations.enabled = false;
    }
    let Some(doc) = node.children() else { return };
    for n in doc.nodes() {
        match n.name().value() {
            "slowdown" => {
                if let Some(v) = first_float_arg(n) {
                    config.animations.slowdown = v.max(0.0);
                }
            }
            "window-movement" => parse_anim_kind(n, &mut config.animations.window_movement),
            "window-resize" => parse_anim_kind(n, &mut config.animations.window_resize),
            "view-offset" | "horizontal-view-movement" => {
                parse_anim_kind(n, &mut config.animations.view_offset)
            }
            "window-open" => parse_anim_kind(n, &mut config.animations.window_open),
            "window-close" => parse_anim_kind(n, &mut config.animations.window_close),
            "workspace-switch" => parse_anim_kind(n, &mut config.animations.workspace_switch),
            // Kinds we have no rendering for; parsed-and-ignored so
            // niri configs port over without warnings.
            "config-error-open" | "config-error-close" | "screenshot-open" | "screenshot-close" => {
            }
            other => log::debug!("ignoring animations node {other:?}"),
        }
    }
}

/// Parse one per-animation node (`window-movement { ... }` etc.)
/// into `slot`, keeping the previous value for anything left unset.
fn parse_anim_kind(node: &KdlNode, slot: &mut AnimKind) {
    let Some(children) = node.children() else {
        return;
    };
    let mut off = false;
    let mut spring: Option<SpringParams> = None;
    let mut duration_ms: Option<u64> = None;
    let mut curve: Option<Curve> = None;

    for c in children.nodes() {
        match c.name().value() {
            "off" => off = true,
            "spring" => {
                if duration_ms.is_some() || curve.is_some() {
                    log::warn!(
                        "animations: cannot set both spring and easing \
                         parameters at once (ignoring spring)"
                    );
                    continue;
                }
                spring = Some(parse_spring(c));
            }
            "duration-ms" => {
                if spring.is_some() {
                    log::warn!(
                        "animations: cannot set both spring and easing \
                         parameters at once (ignoring duration-ms)"
                    );
                    continue;
                }
                if let Some(ms) = first_float_arg(c) {
                    duration_ms = Some(ms.max(0.0) as u64);
                }
            }
            "curve" | "easing" => {
                if spring.is_some() {
                    log::warn!(
                        "animations: cannot set both spring and easing \
                         parameters at once (ignoring curve)"
                    );
                    continue;
                }
                if let Some(cv) = parse_curve(c) {
                    curve = Some(cv);
                }
            }
            other => log::debug!("ignoring animation node {other:?}"),
        }
    }

    if off {
        *slot = AnimKind::instant();
    } else if let Some(sp) = spring {
        *slot = AnimKind::Spring(sp);
    } else if duration_ms.is_some() || curve.is_some() {
        // Niri's merge rules: unset fields keep the previous value,
        // except when the default is a spring and the user configured
        // an easing — then the fallback is 250ms / ease-out-cubic.
        let (def_dur, def_curve) = match *slot {
            AnimKind::Easing { duration, curve } => (duration, curve),
            _ => (std::time::Duration::from_millis(250), Curve::EaseOutCubic),
        };
        *slot = AnimKind::Easing {
            duration: duration_ms
                .map(std::time::Duration::from_millis)
                .unwrap_or(def_dur),
            curve: curve.unwrap_or(def_curve),
        };
    }
    // Nothing configured — keep the default.
}

/// `spring damping-ratio=1.0 stiffness=800 epsilon=0.0001` (niri's
/// property syntax) or `spring { damping-ratio 1.0; stiffness 800; }`.
fn parse_spring(node: &KdlNode) -> SpringParams {
    let mut p = SpringParams::niri_default();
    for e in node.entries() {
        let Some(name) = e.name() else { continue };
        let Some(v) = entry_float(e) else {
            log::warn!("spring: property {:?} is not a number", name.value());
            continue;
        };
        match name.value() {
            "damping-ratio" => p.damping_ratio = v.max(0.0),
            "stiffness" => p.stiffness = v.max(0.0),
            "epsilon" => p.epsilon = v.max(0.000001),
            other => log::warn!("spring: unknown property {other:?}"),
        }
    }
    if let Some(doc) = node.children() {
        for c in doc.nodes() {
            match c.name().value() {
                "damping-ratio" => {
                    if let Some(v) = first_float_arg(c) {
                        p.damping_ratio = v.max(0.0);
                    }
                }
                "stiffness" => {
                    if let Some(v) = first_float_arg(c) {
                        p.stiffness = v.max(0.0);
                    }
                }
                "epsilon" => {
                    if let Some(v) = first_float_arg(c) {
                        p.epsilon = v.max(0.000001);
                    }
                }
                other => log::debug!("ignoring spring node {other:?}"),
            }
        }
    }
    p
}

/// `curve "ease-out-expo"` / `curve "cubic-bezier" 0.05 0.7 0.1 1.0`.
/// `ease-out-back` optionally takes an overshoot amount (default
/// 1.70158); `linear` and the `ease-out-*` family need no arguments.
fn parse_curve(node: &KdlNode) -> Option<Curve> {
    let unnamed: Vec<&kdl::KdlEntry> = node
        .entries()
        .iter()
        .filter(|e| e.name().is_none())
        .collect();
    let name = unnamed.first().and_then(|e| e.value().as_string())?;
    let nums: Vec<f64> = unnamed[1..].iter().filter_map(|e| entry_float(e)).collect();
    match name {
        "linear" => Some(Curve::Linear),
        "ease-out-quad" => Some(Curve::EaseOutQuad),
        "ease-out-cubic" => Some(Curve::EaseOutCubic),
        "ease-out-expo" => Some(Curve::EaseOutExpo),
        "ease-out-back" => Some(Curve::EaseOutBack(nums.first().copied().unwrap_or(1.70158))),
        "cubic-bezier" => {
            if nums.len() >= 4 {
                Some(Curve::CubicBezier(nums[0], nums[1], nums[2], nums[3]))
            } else {
                log::warn!(
                    "curve \"cubic-bezier\" needs 4 control-point values, e.g. \
                     curve \"cubic-bezier\" 0.05 0.7 0.1 1.0"
                );
                None
            }
        }
        other => {
            log::warn!(
                "unknown animation curve {other:?}; expected one of: linear, \
                 ease-out-quad, ease-out-cubic, ease-out-expo, ease-out-back, cubic-bezier"
            );
            None
        }
    }
}

/// Numeric value of a KDL entry (integer or float), for named
/// properties like `damping-ratio=1.0`.
fn entry_float(e: &kdl::KdlEntry) -> Option<f64> {
    e.value()
        .as_integer()
        .map(|v| v as f64)
        .or_else(|| e.value().as_float())
}

fn parse_binds(node: &KdlNode, config: &mut Config) {
    let Some(doc) = node.children() else { return };
    for n in doc.nodes() {
        // Node name is the key combo, child nodes are actions.
        let combo = n.name().value().to_string();
        if let Some(actions) = n.children() {
            for action in actions.nodes() {
                if let Some(a) = parse_action(action) {
                    config.binds.push(Bind {
                        combo: combo.clone(),
                        action: a,
                    });
                }
            }
        }
    }
}

/// Map an action node like `focus-column-left;` or
/// `set-column-width "+10";` to an Action.
fn parse_action(node: &KdlNode) -> Option<Action> {
    let name = node.name().value();
    let arg = first_string_arg(node);
    let action = match name {
        "quit" => Action::Quit,
        "close-window" => Action::CloseWindow,
        "spawn" | "spawn-sh" => {
            let cmd = arg.unwrap_or_default();
            Action::Spawn(cmd)
        }
        "focus-column-left" => Action::FocusColumnLeft,
        "focus-column-right" => Action::FocusColumnRight,
        "focus-column-first" => Action::FocusColumnFirst,
        "focus-column-last" => Action::FocusColumnLast,
        "focus-column-index" => Action::FocusColumnIndex(first_u8_arg(node).unwrap_or(1)),
        "focus-window-down" => Action::FocusWindowDown,
        "focus-window-up" => Action::FocusWindowUp,
        "move-column-left" => Action::MoveColumnLeft,
        "move-column-right" => Action::MoveColumnRight,
        "move-window-down" => Action::MoveWindowDown,
        "move-window-up" => Action::MoveWindowUp,
        "move-window-to-column-left" => Action::MoveWindowToColumnLeft,
        "move-window-to-column-right" => Action::MoveWindowToColumnRight,
        "consume-or-expel-window-left" => Action::ConsumeOrExpelWindowLeft,
        "consume-or-expel-window-right" => Action::ConsumeOrExpelWindowRight,
        "set-column-width" => Action::SetColumnWidth(arg.unwrap_or_default()),
        "switch-preset-column-width" => Action::SwitchPresetColumnWidth,
        "set-window-height" => Action::SetWindowHeight(arg.unwrap_or_default()),
        "toggle-full-width" => Action::ToggleFullWidth,
        "maximize-column" => Action::MaximizeColumn,
        "toggle-windowed-fullscreen" => Action::ToggleWindowedFullscreen,
        "toggle-window-floating" => Action::ToggleWindowFloating,
        "toggle-overview" => Action::ToggleOverview,
        "do-screen-transition" => Action::DoScreenTransition,
        "focus-workspace" | "workspace-switch" => {
            Action::FocusWorkspace(first_u8_arg(node).unwrap_or(1))
        }
        "move-window-to-workspace" => {
            Action::MoveWindowToWorkspace(first_u8_arg(node).unwrap_or(1))
        }
        "move-column-to-workspace" => {
            Action::MoveColumnToWorkspace(first_u8_arg(node).unwrap_or(1))
        }
        other => {
            log::debug!("ignoring unknown action {other:?}");
            Action::Unknown(other.to_string())
        }
    };
    // Keep unknown actions in the bind table (they no-op) so the combo
    // still shadows defaults sensibly.
    Some(action)
}

// --- small KDL helpers -------------------------------------------------

fn first_string_arg(node: &KdlNode) -> Option<String> {
    // Arguments are entries without a property name.
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .find_map(|e| e.value().as_string().map(|s| s.to_string()))
}

fn first_float_arg(node: &KdlNode) -> Option<f64> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .find_map(|e| {
            e.value()
                .as_integer()
                .map(|v| v as f64)
                .or_else(|| e.value().as_float())
        })
}

fn first_bool_arg(node: &KdlNode) -> Option<bool> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .find_map(|e| e.value().as_bool())
}

/// First numeric argument as u8 — KDL stores `focus-column-index 3;`
/// as an integer, not a string, so `first_string_arg` misses it.
fn first_u8_arg(node: &KdlNode) -> Option<u8> {
    first_float_arg(node).map(|f| f.max(0.0) as u8)
}

#[allow(dead_code)]
fn has_property(node: &KdlNode, name: &str) -> bool {
    node.entries()
        .iter()
        .any(|e| e.name().map(|n| n.value() == name).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let cfg = Config::default();
        assert!(matches!(cfg.mod_key, ModKey::Alt));
        assert!(!cfg.focus_follows_mouse);
        assert_eq!(cfg.layout.gaps, 8.0);
        assert!(
            cfg.binds
                .iter()
                .any(|b| b.combo == "Mod+H" && b.action == Action::FocusColumnLeft)
        );
        // Niri's wheel binds are in the defaults.
        assert!(
            cfg.binds
                .iter()
                .any(|b| b.combo == "Mod+WheelScrollDown" && b.action == Action::FocusColumnRight)
        );
    }

    #[test]
    fn focus_follows_mouse_flag() {
        let cfg = parse("input { focus-follows-mouse; }").unwrap();
        assert!(cfg.focus_follows_mouse);
        // Absent by default.
        assert!(!parse("layout { gaps 8; }").unwrap().focus_follows_mouse);
        // Wheel binds parse like any key.
        let cfg = parse("binds { Mod+WheelScrollDown { focus-column-right; } }").unwrap();
        assert!(cfg.binds.iter().any(|b| b.combo == "Mod+WheelScrollDown"));
    }

    #[test]
    fn parse_full_example() {
        let src = r#"
            input {
                keyboard-shortcuts {
                    Mod "Super"
                }
            }
            layout {
                gaps 12
                edge-padding 4
                center-focused-column "always"
                default-column-width { fixed 1000; }
            }
            binds {
                Mod+H { focus-column-left; }
                Mod+L { focus-column-right; }
                Mod+W { set-column-width "+100"; }
            }
        "#;
        let cfg = parse(src).unwrap();
        assert!(matches!(cfg.mod_key, ModKey::Super));
        assert_eq!(cfg.layout.gaps, 12.0);
        assert_eq!(cfg.layout.edge_padding, 4.0);
        assert_eq!(cfg.layout.center_focused_column, CenterFocused::Always);
        assert_eq!(cfg.layout.default_column_width, ColumnWidth::Fixed(1000.0));
        assert_eq!(cfg.binds.len(), 3);
        assert!(
            cfg.binds
                .iter()
                .any(|b| b.action == Action::SetColumnWidth("+100".into()))
        );
    }

    #[test]
    fn parse_errors_are_soft() {
        assert!(parse("this is ((( not kdl").is_err());
        // Unknown nodes are fine.
        let cfg = parse("favorites { color \"blue\"; }").unwrap();
        assert_eq!(cfg.layout.gaps, 8.0);
    }

    #[test]
    fn config_without_binds_keeps_default_binds() {
        // A config that only tweaks animations must not wipe the
        // default key bindings (niri semantics: only an explicit
        // `binds` section replaces them).
        let cfg = parse("animations { slowdown 6.0 }").unwrap();
        assert!(!cfg.binds.is_empty(), "default binds survive");

        // An explicit binds section still replaces everything.
        let cfg = parse("binds { Mod+Q { quit; } }").unwrap();
        assert_eq!(cfg.binds.len(), 1);
        assert_eq!(cfg.binds[0].combo, "Mod+Q");
    }

    #[test]
    fn animations_config() {
        // Easing form: duration-ms + curve (niri syntax), plus the
        // `easing "name"` alias from our older configs.
        let cfg = parse(
            "animations {
                window-movement { duration-ms 500; curve \"ease-out-expo\"; }
                view-offset { duration-ms 0; }
            }",
        )
        .unwrap();
        assert!(cfg.animations.enabled);
        assert_eq!(
            cfg.animations.window_movement,
            AnimKind::Easing {
                duration: std::time::Duration::from_millis(500),
                curve: Curve::EaseOutExpo,
            }
        );
        assert_eq!(
            cfg.animations.view_offset,
            AnimKind::Easing {
                duration: std::time::Duration::ZERO,
                curve: Curve::EaseOutCubic, // spring default -> easing fallback
            }
        );
        // Defaults intact for untouched kinds.
        assert_eq!(
            cfg.animations.window_open,
            AnimKind::Easing {
                duration: std::time::Duration::from_millis(150),
                curve: Curve::EaseOutExpo,
            }
        );

        let cfg = parse("animations { off; }").unwrap();
        assert!(!cfg.animations.enabled);
        // Master `off` maps to instant, regardless of the kind.
        assert_eq!(cfg.animations.movement_params().kind, AnimKind::instant());

        // Spring form with niri's property syntax.
        let cfg = parse(
            "animations {
                window-resize { spring damping-ratio=0.8 stiffness=400 epsilon=0.001 }
            }",
        )
        .unwrap();
        assert_eq!(
            cfg.animations.window_resize,
            AnimKind::Spring(SpringParams {
                damping_ratio: 0.8,
                stiffness: 400.0,
                epsilon: 0.001,
            })
        );

        // Nested spring form and slowdown.
        let cfg = parse(
            "animations {
                slowdown 2.0
                workspace-switch { spring { damping-ratio 1.0; stiffness 1000; epsilon 0.0001; } }
            }",
        )
        .unwrap();
        assert_eq!(cfg.animations.slowdown, 2.0);
        assert_eq!(
            cfg.animations.workspace_switch,
            AnimKind::Spring(SpringParams {
                damping_ratio: 1.0,
                stiffness: 1000.0,
                epsilon: 0.0001,
            })
        );

        // Custom cubic-bezier curve.
        let cfg = parse(
            "animations {
                window-open { duration-ms 150; curve \"cubic-bezier\" 0.05 0.7 0.1 1.0; }
            }",
        )
        .unwrap();
        assert_eq!(
            cfg.animations.window_open,
            AnimKind::Easing {
                duration: std::time::Duration::from_millis(150),
                curve: Curve::CubicBezier(0.05, 0.7, 0.1, 1.0),
            }
        );

        // Per-animation `off` disables just that one.
        let cfg = parse("animations { window-movement { off; } }").unwrap();
        assert!(cfg.animations.enabled);
        assert_eq!(cfg.animations.window_movement, AnimKind::instant());

        // Unknown curve keeps the old value (soft failure).
        let cfg = parse("animations { window-open { curve \"bogus\"; } }").unwrap();
        assert_eq!(
            cfg.animations.window_open,
            AnimKind::Easing {
                duration: std::time::Duration::from_millis(150),
                curve: Curve::EaseOutExpo,
            }
        );
    }

    #[test]
    fn window_rule_parsing_and_matching() {
        let cfg = parse(
            r#"
            window-rule {
                match app-id="Firefox" title="Download"
                open-floating
            }
            window-rule {
                match exe="notepad.exe"
                open-on-workspace 2
                open-maximized
            }
            "#,
        )
        .unwrap();
        assert_eq!(cfg.window_rules.len(), 2);

        // Substring, case-insensitive.
        let r = cfg
            .match_rule("firefox.exe", "Downloads", "Mozilla")
            .unwrap();
        assert!(r.open_floating);
        // Title mismatch: no hit on rule 0; falls through to rule 1.
        assert!(
            cfg.match_rule("firefox.exe", "New Tab", "Mozilla")
                .is_none()
        );
        let r = cfg
            .match_rule("notepad.exe", "Untitled", "Notepad")
            .unwrap();
        assert_eq!(r.open_workspace, Some(2));
        assert!(r.open_maximized);

        // Bare `open-floating;` implies true; `#false` disables.
        let cfg = parse("window-rule { match exe=\"a\"; open-floating; }").unwrap();
        assert!(cfg.window_rules[0].open_floating);
        let cfg = parse("window-rule { match exe=\"a\"; open-floating #false; }").unwrap();
        assert!(!cfg.window_rules[0].open_floating);
    }

    #[test]
    fn preset_column_widths_config() {
        let cfg = parse(
            "layout { preset-column-widths { proportion 0.33; proportion 0.5; fixed 1280; } }",
        )
        .unwrap();
        assert_eq!(
            cfg.layout.preset_column_widths,
            vec![
                ColumnWidth::Proportion(0.33),
                ColumnWidth::Proportion(0.5),
                ColumnWidth::Fixed(1280.0)
            ]
        );

        // Cycling: default -> first -> ... -> wraps.
        let presets = cfg.layout.preset_column_widths.clone();
        let mut ws = crate::layout::Workspace::new();
        ws.add_window(1);
        assert!(ws.cycle_column_width(&presets));
        assert_eq!(ws.columns[0].width, Some(ColumnWidth::Proportion(0.33)));
        assert!(ws.cycle_column_width(&presets));
        assert_eq!(ws.columns[0].width, Some(ColumnWidth::Proportion(0.5)));
        assert!(ws.cycle_column_width(&presets));
        assert_eq!(ws.columns[0].width, Some(ColumnWidth::Fixed(1280.0)));
        assert!(ws.cycle_column_width(&presets));
        assert_eq!(ws.columns[0].width, Some(ColumnWidth::Proportion(0.33)));

        // Empty presets: no-op.
        assert!(!ws.cycle_column_width(&[]));
    }

    #[test]
    fn spawn_at_startup_config() {
        let cfg = parse(
            r#"spawn-at-startup "alacritty"
            spawn-at-startup "notepad.exe""#,
        )
        .unwrap();
        assert_eq!(cfg.spawn_at_startup, vec!["alacritty", "notepad.exe"]);
        assert!(Config::default().spawn_at_startup.is_empty());
    }

    #[test]
    fn focus_ring_config() {
        let cfg = parse(r##"layout { focus-ring { width 2; active-color "#ff0000"; } }"##).unwrap();
        assert!(cfg.focus_ring.enabled);
        assert_eq!(cfg.focus_ring.width, 2);
        assert_eq!(cfg.focus_ring.active_color, 0xFF_0000);

        let cfg = parse("layout { focus-ring { off; } }").unwrap();
        assert!(!cfg.focus_ring.enabled);

        // Bad colors keep the old value (soft failure).
        let cfg = parse(r##"layout { focus-ring { active-color "nope"; } }"##).unwrap();
        assert_eq!(
            cfg.focus_ring.active_color,
            FocusRingConfig::default().active_color
        );
    }

    #[test]
    fn focus_column_index_config() {
        // Default bind: Mod+Shift+N focuses the Nth column.
        let cfg = Config::default();
        assert!(
            cfg.binds
                .iter()
                .any(|b| b.combo == "Mod+Shift+3" && b.action == Action::FocusColumnIndex(3))
        );
        assert!(
            cfg.binds
                .iter()
                .any(|b| b.combo == "Mod+3" && b.action == Action::FocusWorkspace(3))
        );

        // `focus-column-index 2;` parses (integer arg, not string).
        let cfg = parse("binds { Mod+M { focus-column-index 2; } }").unwrap();
        assert!(
            cfg.binds
                .iter()
                .any(|b| b.action == Action::FocusColumnIndex(2))
        );

        // Bare node defaults to 1.
        let cfg = parse("binds { Mod+M { focus-column-index; } }").unwrap();
        assert!(
            cfg.binds
                .iter()
                .any(|b| b.action == Action::FocusColumnIndex(1))
        );

        // Integer args work for workspace actions too (KDL ints are
        // not strings).
        let cfg = parse("binds { Mod+W { focus-workspace 4; } }").unwrap();
        assert!(
            cfg.binds
                .iter()
                .any(|b| b.action == Action::FocusWorkspace(4))
        );
    }

    #[test]
    fn do_screen_transition_config() {
        // Parses as an action; not bound by default (niri doesn't bind
        // it either — users wire it to their screenshot tool).
        let cfg = parse("binds { Mod+P { do-screen-transition; } }").unwrap();
        assert!(
            cfg.binds
                .iter()
                .any(|b| b.action == Action::DoScreenTransition)
        );
        assert!(
            !Config::default()
                .binds
                .iter()
                .any(|b| b.action == Action::DoScreenTransition)
        );
    }

    #[test]
    fn example_config_parses() {
        // The shipped example must stay valid against the real parser.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.example.kdl");
        let Ok(src) = std::fs::read_to_string(&path) else {
            panic!("missing config.example.kdl next to Cargo.toml");
        };
        let cfg = parse(&src).expect("config.example.kdl must parse");
        assert!(!cfg.binds.is_empty());
        assert_eq!(cfg.window_rules.len(), 2);
        assert_eq!(cfg.spawn_at_startup, vec!["alacritty"]);
        assert_eq!(cfg.layout.preset_column_widths.len(), 3);
    }

    /// The snippets in docs/CONFIG.md and config.example.kdl must keep
    /// parsing (guards against both doc drift and KDL dialect traps).
    #[test]
    fn doc_snippets_parse() {
        let snippets: Vec<(&str, &str)> = vec![
            (
                "focus-ring",
                r##"layout {
    focus-ring {
        // off
        width 4               // thickness
        active-color "#7daea3"
    }
}
"##,
            ),
            (
                "animations",
                r##"animations {
    window-movement { duration-ms 250; easing "ease-out-cubic"; }
    window-open    { duration-ms 150; easing "ease-out-cubic"; }
    view-offset    { duration-ms 250; easing "ease-out-expo"; }
}
"##,
            ),
            (
                "input",
                r##"input {
    focus-follows-mouse
    keyboard-shortcuts {
        Mod "Alt"
    }
}
"##,
            ),
            (
                "layout",
                r##"layout {
    gaps 8
    edge-padding 8
    center-focused-column "on-overflow"
    default-column-width { proportion 0.25; }
    preset-column-widths {
        proportion 0.33
        proportion 0.5
        fixed 1280
    }
}
"##,
            ),
            (
                "window-rule",
                r##"window-rule {
    match app-id="firefox" title="download"
    open-floating
}
"##,
            ),
        ];
        for (name, doc) in snippets {
            if let Err(e) = doc.parse::<KdlDocument>() {
                panic!("snippet {name} fails: {e}");
            }
        }
    }

    /// kdl 6.x quirk we must not trip in docs/examples: a comment on
    /// the same line after a child block's `}` breaks parsing of the
    /// sibling nodes that follow. Keep trailing comments off `}` lines.
    #[test]
    fn trailing_comment_after_child_block() {
        assert!(
            "binds {
    a { b; } // c
    d { e; }
}"
            .parse::<KdlDocument>()
            .is_err()
        );
        assert!(
            "binds {
    a { b; }
    d { e; }
}"
            .parse::<KdlDocument>()
            .is_ok()
        );
    }

    #[test]
    fn proportion_clamped() {
        let cfg = parse("layout { default-column-width { proportion 5000; } }").unwrap();
        assert_eq!(
            cfg.layout.default_column_width,
            ColumnWidth::Proportion(100.0)
        );
    }
}
