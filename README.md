# wini

A scrollable-tiling window manager for Windows, inspired by
[Niri](https://github.com/YaLTeR/niri).

> Reproduce the Niri experience, not the Niri architecture.

Windows already has a window manager and a compositor (DWM), so
wini does **not** replace them. It runs as a normal process on
top, tracking application windows (HWNDs) and arranging them into
Niri's scrollable, column-based layout — with keyboard-driven focus,
workspaces, floating windows, animations and an overview mode.

## Features

- **Scrollable tiling** — windows live in an infinite horizontal strip
  of columns; focusing a column scrolls it into view
  (`center-focused-column` behaves like niri's `never` /
  `on-overflow` / `always`)
- **Column stacks** — multiple windows can share a column
  (consume/expel, per-window height)
- **Workspaces** — per-monitor, grow on demand, with a switch
  indicator overlay
- **Floating windows** — toggle any window out of the tiling, move and
  resize it with the keyboard or the mouse
- **Overview** — zoom out to a filmstrip of all columns
  (`Mod+Shift+/`), click or navigate, `Esc` to exit
- **Focus ring** — configurable outline around the focused window
- **Animations** — per-kind duration/easing (window movement, open,
  view scroll), all optional
- **Mouse** — wheel as bindable `WheelScroll*` keys, optional
  `focus-follows-mouse`, drag-and-drop reordering of tiled windows
- **Window rules** — match `app-id` (exe) / `title` / `class` and
  open floating, on a workspace, maximized or fullscreen
- **Hot reload** — edit the config file, it applies within a second
  (a broken config keeps the last good one)
- **Graceful exit** — every window gets its original geometry,
  decorations and visibility back when wini quits

## Build & run

Requires Rust (stable) on Windows.

```
cargo build --release
```

Then run the exe **as your normal user** — it will adopt all existing
top-level windows and start tiling. Do not run it as a service or
before login; it needs an interactive desktop.

> ⚠️ The process really does take over window placement on your
> desktop. `Mod+Shift+E` (or Ctrl+C in the console) quits and restores
> everything.

Logs go to stderr; raise verbosity with `RUST_LOG=debug`.

## Configuration

The config lives at `%APPDATA%\wini\config.kdl` (override with
the `WINI_CONFIG` environment variable). Missing file = sane
defaults. See [docs/CONFIG.md](docs/CONFIG.md) for the full reference
and [config.example.kdl](config.example.kdl) for a commented starting
point.

The `Mod` key defaults to **Alt**.

## Default key bindings

| Keys | Action |
| --- | --- |
| `Mod+←` / `Mod+→` (`Mod+H` / `Mod+L`) | focus column left / right |
| `Mod+↑` / `Mod+↓` (`Mod+K` / `Mod+J`) | focus window up / down |
| `Mod+Home` / `Mod+End` | focus first / last column |
| `Mod+Shift+1..9` | focus the Nth column |
| `Mod+Shift+←/→/↑/↓` (`H/J/K/L`) | move column / window |
| `Mod+Ctrl+←/→` (`H` / `L`) | move window to the column left / right |
| `Mod+Ctrl+J` / `Mod+Ctrl+K` | consume / expel window into the column left / right |
| `Mod+Wheel` (`PageUp` / `PageDown`) | scroll columns (focus follows) |
| `Mod+Ctrl+Wheel` | move focused column |
| `Mod+F` | maximize column (toggle) |
| `Mod+Shift+F` | windowed fullscreen (toggle) |
| `Mod+V` | toggle floating |
| `Mod+1..9` | switch workspace |
| `Mod+Ctrl+1..9` | move focused column to workspace |
| `Mod+Q` | close window |
| `Mod+Shift+/` | toggle overview |
| `Mod+Shift+E` | quit |

Wheel and mouse: with the Mod key held, the wheel scrolls the
viewport. Optional `focus-follows-mouse` in the config.

## Project layout

```
src/
  win/       Win32 backend: window tracking, events, monitors, overlays
  layout/    the niri-semantics data model + viewport geometry
  input/     low-level keyboard/mouse hooks, bind matching
  config/    KDL config parsing (niri-flavored)
  anim/      easing + animated rects driven by a timer
  app.rs     the message loop gluing it all together
```

`niri/` contains a copy of the Niri source used as a *reference only*
(see AGENTS.md); it is not part of the build.
