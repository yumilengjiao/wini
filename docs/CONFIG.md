# Configuration reference

yumi-wini is configured with a [KDL](https://kdl.dev) file, kept at
`%APPDATA%\yumi-wini\config.kdl` (override with the `YUMI_WINI_CONFIG`
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

    // Outline around the focused window.
    focus-ring {
        // off                 - disable the ring
        width 4               // thickness in pixels (default 4)
        active-color "#7daea3" // "#rrggbb" (default #7daea3)
    }
}
```

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
| `quit` | exit yumi-wini (restores all windows) |
| `close-window` | close the focused window (WM_CLOSE) |
| `spawn "cmd args"` | run a command (first token = exe) |
| `focus-column-left` / `focus-column-right` | move column focus |
| `focus-column-first` / `focus-column-last` | jump to the first/last column |
| `focus-column-index N` | focus the Nth column (clamped to the last) |
| `focus-window-down` / `focus-window-up` | move focus within the column |
| `move-column-left` / `move-column-right` | reorder the focused column |
| `move-window-down` / `move-window-up` | reorder within the column |
| `move-window-to-column-left` / `move-window-to-column-right` | move the window into the neighboring column |
| `consume-or-expel-window-left` / `-right` | merge into / split out of the neighboring column |
| `set-column-width SPEC` | set focused column width (see below); bare = cycle `preset-column-widths` |
| `set-window-height SPEC` | set focused window height (same SPEC syntax) |
| `toggle-full-width` | column spans the full workspace width |
| `maximize-column` | toggle column maximize |
| `toggle-windowed-fullscreen` | borderless fullscreen for one window |
| `toggle-window-floating` | float / unfloat the focused window |
| `toggle-overview` | zoomed-out overview of all columns |
| `do-screen-transition` | cover the screen ~1s while management pauses (screenshot workflows) |
| `focus-workspace N` | switch to workspace N |
| `move-window-to-workspace N` | move the focused window |
| `move-column-to-workspace N` | move the focused column |

**Width/height `SPEC`:** `"+100"` / `"-100"` adjust by a delta, `"800"`
sets fixed pixels, `"50%"` scales by a proportion. (Quotes are only
needed for values with `+`/`-`/`%`.)

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
