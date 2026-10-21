# Configuration reference

wini is configured with a [KDL](https://kdl.dev) file, kept at
`%APPDATA%\wini\config.kdl` (override with the `WINI_CONFIG`
environment variable). The file is hot-reloaded: changes apply within
about a second; if the new file fails to parse, the previous config
stays in effect and a warning is logged.

Unknown nodes and fields are ignored (forward compatibility). A
missing config file means defaults.

See `config.example.kdl` in the repository root for a fully commented
example.

---

## `input`

```kdl
input {
    // Hovering a window focuses it (niri's focus-follows-mouse).
    focus-follows-mouse

    keyboard-shortcuts {
        // Which physical key acts as "Mod": "Alt" (default),
        // "Super" (the Windows key) or "Ctrl".
        Mod "Alt"
    }
}
```

## `layout`

```kdl
layout {
    // Gap between windows, in pixels (default 8).
    gaps 8

    // Outer spacing from the work-area edges, in physical pixels (default 0).
    // Each side is independent; `struts 3` is shorthand for all four.
    struts { left 0; right 0; top 0; bottom 0; }

    // Padding at the left/right edges of the monitor (default 8).
    edge-padding 8

    // How the focused column is kept in view (default "on-overflow"):
    //   "never"        - only scrolls when the column is off-screen
    //   "on-overflow"  - like never, plus view centers when it overflows
    //   "always"       - the focused column is always centered
    center-focused-column "on-overflow"

    // Width for new columns. Either a bare number (fixed pixels) or
    // a child node (default: proportion 0.25).
    default-column-width { proportion 0.25; }
    // default-column-width { fixed 1280; }
    // default-column-width 800

    // Widths cycled through by a bare `set-column-width;` action.
    preset-column-widths {
        proportion 0.33
        proportion 0.5
        fixed 1280
    }

    // Heights cycled by Mod+Ctrl+Shift+R; empty keeps 1/3, 1/2, 2/3 defaults.
    preset-window-heights {
        proportion 0.33333
        proportion 0.5
        proportion 0.66667
    }

    // Outline around the focused window.
    focus-ring {
        // off                 - disable the ring
        width 4               // thickness in pixels (default 4)
        active-color "#7daea3" // "#rrggbb" (default #7daea3)
    }
}
```

## Tabbed columns

`Mod+W` (`toggle-column-tabbed-display`) switches the focused column between
vertical stacking and tabs. Only its selected tab is visible; `Mod+J/K`
selects down/up, including in overview. Click the indicator segments to
select a tab; selection in overview keeps overview open. Window sizes,
height requests and tab mode are preserved when moving an entire column to
another workspace/monitor. Turning tabs off restores the stack.

```kdl
layout {
    tab-indicator {
        // off
        hide-when-single-tab
        // place-within-column // reserve indicator space within column
        position "left"       // left / right / top / bottom
        width 4
        gap 5
        length total-proportion=0.5
        gaps-between-tabs 2
        corner-radius 2
        active-color "#7daea3"
        inactive-color "#505050"
    }
}
```

Sizes are physical pixels, scaled with overview zoom. Default indicator is
left, width 4, gap 5, length 50%, solid colors. Outside indicators may overlap
neighbor windows when column gaps are small; `place-within-column` reserves
space. The overlay and click regions are clipped to each monitor's work
area. Gradients and urgent-color are not implemented yet. Tabs share one
height (full available height by default); preset/fixed height applies to
the group, and reset returns the group to full height. This is a Windows
management layer: tab switches use show/hide, not compositor crossfades.

## `animations`

Follows niri's animation syntax. Each animation kind is configured
separately; unset kinds keep their defaults (springs for
movement/resize/view-offset, short easings for open/close).

```kdl
animations {
    // off    - disable all animations (everything snaps)
    // slowdown 3.0   - slow every animation down by this factor

    // Spring physics: carries velocity through retargets.
    window-movement {
        spring damping-ratio=1.0 stiffness=800 epsilon=0.0001
    }

    // Timed easing instead: duration-ms + curve.
    view-offset {
        duration-ms 100
        curve "ease-out-expo"
    }

    // Custom cubic-bezier curve.
    window-open {
        duration-ms 150
        curve "cubic-bezier" 0.05 0.7 0.1 1.0
    }

    // Per-animation off: just this one snaps.
    // window-close { off }
}
```

**Kinds:** `window-movement`, `window-resize`, `view-offset`
(`horizontal-view-movement` also accepted), `workspace-switch`,
`window-open`, `window-close`. Movement, resize and view-offset drive
the real animations; open/close/workspace-switch are parsed and
validated so niri configs port over, but not animated yet.

**Spring parameters:** `damping-ratio` (1.0 = settles without
bouncing, < 1 bounces, > 1 sluggish), `stiffness` (higher = snappier),
`epsilon` (rest threshold). Defaults: `1.0 / 800 / 0.0001`.

**Curves:** `linear`, `ease-out-quad`, `ease-out-cubic`,
`ease-out-expo`, `ease-out-back` (optionally with an overshoot
amount), and `cubic-bezier` with four control-point values.
`duration-ms 0` disables that animation kind. Spring and easing
parameters must not be mixed in one kind.

## `binds`

Each node inside `binds` is named by a key combo; its children are
the actions to run (in order).

```kdl
binds {
    Mod+H { focus-column-left; }
    Mod+Shift+E { quit; }
    Mod+W { set-column-width "+100"; }
}
```

**Modifiers:** `Mod` (the configured mod key), `Shift`, `Ctrl`.
**Key names:** letters `A`-`Z` and digits as-is; `F1`-`F24`, `Left`,
`Right`, `Up`, `Down`, `Home`, `End`, `Page_Up`, `Page_Down`,
`Return`, `Escape`, `Backspace`, `Tab`, `Delete`, `Insert`, `space`,
`Print`, `Pause`, numpad keys (`KP_0`-`KP_9`, `KP_Add`, ...), plus
the mouse-scroll pseudo-keys `WheelScrollUp`, `WheelScrollDown`,
`WheelScrollLeft`, `WheelScrollRight`.

A config-provided `binds` section **replaces** the default binds.

### Actions

| Action | Meaning |
| --- | --- |
| `quit` | exit wini (restores all windows) |
| `close-window` | close the focused window (WM_CLOSE) |
| `spawn "cmd args"` | run a command (first token = exe) |
| `focus-column-left` / `focus-column-right` | move column focus |
| `focus-column-first` / `focus-column-last` | jump to the first/last column |
| `focus-column-index N` | focus the Nth column (clamped to the last) |
| `focus-window-down` / `focus-window-up` | move focus within the column |
| `move-column-left` / `move-column-right` | reorder the focused column |
| `move-column-to-first` / `move-column-to-last` | move the focused column to the first/last position (Mod+Ctrl+Home / End) |
| `move-window-down` / `move-window-up` | reorder within the column |
| `move-window-to-column-left` / `move-window-to-column-right` | move the window into the neighboring column |
| `focus-monitor-left` / `-right` / `-up` / `-down` | move focus to the adjacent monitor (Mod+Shift+arrows / HJKL) |
| `move-column-to-monitor-left` / `-right` / `-up` / `-down` | move the focused column to the adjacent monitor (unbound; multi-monitor) |
| `move-window-to-monitor-left` / `-right` / `-up` / `-down` | move the focused window to the adjacent monitor (unbound; multi-monitor) |
| `consume-or-expel-window-left` / `-right` | merge into / split out of the neighboring column |
| `set-column-width SPEC` | set focused column width (see below); bare = cycle `preset-column-widths` |
| `switch-preset-column-width` / `-back` | cycle preset widths wider-first / narrower-first (Mod+R / Mod+Shift+R) |
| `set-window-height SPEC` | set focused window height in physical pixels or a proportion of tiling-area height |
| `switch-preset-window-height` / `-back` | cycle height presets taller-first / shorter-first (forward: Mod+Ctrl+Shift+R; reverse unbound) |
| `reset-window-height` | return focused window to automatic height sharing (Mod+Ctrl+R) |
| `toggle-full-width` | column spans the full workspace width |
| `maximize-column` | fill the strut-inset tiling area (Mod+F), retaining window decorations |
| `maximize-window-to-edges` | toggle the focused window to fill the raw work area, ignoring gaps/struts and keeping taskbar/decorations (Mod+M); extract a stacked window, keep tabbed groups |
| `center-column` | center the focused column in the view (Mod+C) |
| `center-visible-columns` | center the fully visible column group, keeping focus and gaps (Mod+Ctrl+C) |
| `expand-column-to-available-width` | expand the active column into free space without displacing other fully visible columns (Mod+Ctrl+F); lone column uses reversible full-width mode |
| `toggle-column-tabbed-display` | switch the column between stacking and tabs (Mod+W); select via Mod+J/K or indicator clicks |
| `fullscreen-window` / `toggle-windowed-fullscreen` | manager-side borderless fullscreen over the whole monitor (Mod+Shift+F); this does not request exclusive fullscreen from the application |
| `toggle-window-floating` | float / unfloat the focused window |
| `switch-focus-between-floating-and-tiling` | switch focus layers on the current workspace, remembering the last float (Mod+Shift+V); empty destination layer / overview is a no-op |
| `toggle-overview` | zoomed-out overview of all columns |
| `do-screen-transition` | cover the screen ~1s while management pauses (screenshot workflows) |
| `focus-workspace N` | switch to workspace N |
| `focus-workspace-down` / `-up` | switch to the next / previous workspace (Mod+U / Mod+I) |
| `focus-workspace-previous` | toggle between the current and last workspace |
| `move-window-to-workspace N` | move the focused window |
| `move-window-to-workspace-down` / `-up` | move the focused window one workspace down / up |
| `move-column-to-workspace N` | move the focused column |
| `move-column-to-workspace-down` / `-up` | move the focused column one workspace down / up (Mod+Ctrl+U / I) |

**Width/height `SPEC`:** `"+100"` / `"-100"` adjust by a pixel delta,
`"800"` sets fixed pixels, `"50%"` sets an absolute proportion of the
view, and `"+10%"` / `"-10%"` adjust the proportion by a signed delta.
(Quotes are only needed for values with `+`/`-`/`%`.) Default binds:
Mod+Minus / Mod+Equal adjust column width by ±10%, Mod+Shift+Minus /
Mod+Shift+Equal adjust window height. Height percentage deltas are relative
to the tiling area's height (after struts), not a guessed reference size.
A fixed/preset-height window reserves its requested height; auto-height
siblings share the rest, honoring minimum heights. If minimums cannot fit,
the column extends below the work area rather than overlapping. Reset
returns the focused window to auto weight 1; a sole fixed-height window may
leave unused space below it. Floating `set-window-height` keeps its existing
behavior; preset/reset height actions currently apply to tiled windows only.

## Window sizing modes on Windows

- **Mod+F — maximize-column:** fills the tiling area and keeps configured
  struts. **Mod+M — maximize-window-to-edges:** fills the raw Windows work
  area (taskbar remains outside), ignoring struts/gaps. The visible window
  edge is aligned using measured invisible resize margins, not a fixed DPI
  guess; titlebar/CSD and DWM non-client rendering are preserved.
- If edge-maximizing a window in a vertically stacked column, that window
  becomes a standalone column to the right, as in niri. A tabbed group stays
  together and shows its selected tab. Toggle again to restore size hints;
  an extracted window remains a standalone column. Explicit width/height
  requests leave edge maximization. The action currently targets tiled
  windows; floating edge maximization is not implemented yet.
- **Mod+Shift+F — fullscreen-window:** a Windows manager-side borderless
  approximation (also available as `toggle-windowed-fullscreen`), covering
  the full monitor, including the taskbar area. It is not F11 injection,
  application-exclusive fullscreen, or a game display-mode request.
- All three modes use the same transport/overview geometry. Borderless
  fullscreen remembers only the decoration bits it changes and restores
  their exact previous values; it does not add a caption/resize frame to
  previously borderless custom-decorated windows.

## `window-rule`

Rules apply to newly opened (and adopted-at-startup) windows; the
first matching rule wins. `match` fields are case-insensitive
substrings; all listed fields must match. `app-id` is an alias for
the process exe name.

```kdl
window-rule {
    match app-id="firefox" title="download"
    open-floating
}
window-rule {
    match exe="notepad.exe"
    open-on-workspace 2
    open-maximized
    // open-fullscreen
}
```

| Property | Meaning |
| --- | --- |
| `open-floating` | start as a floating window |
| `open-on-workspace N` | place on workspace N (1-based) |
| `open-maximized` | start maximized (full-width column) |
| `open-fullscreen` | start windowed-fullscreen |

## `spawn-at-startup`

```kdl
spawn-at-startup "alacritty"
spawn-at-startup "notepad.exe"
```

Runs each command in order once the hooks are live; spawned windows
are adopted and tiled like any other.
