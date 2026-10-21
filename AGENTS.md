# AGENTS.md

# Project Goal

This project aims to build a Windows window manager inspired by Niri.

The goal is to reproduce Niri's user experience and behavior as closely as possible, including:

- Scrollable / infinite window layout
- Smooth window transitions and animations
- Horizontal and vertical window navigation
- Column-based window layout
- Dynamic window sizing
- Window focus management
- Window grouping
- Workspace management
- Keyboard-driven window management
- Niri-like viewport scrolling

The project does NOT need to reproduce Niri's Wayland implementation.

The target platform is Windows.

The core principle is:

> Reproduce the Niri experience, not the Niri architecture.

---

# Niri Source Code

The repository may contain a local copy of the Niri source code.

The Niri source code exists only as reference material.

It may be used to study:

- Layout algorithms
- Workspace behavior
- Window placement logic
- Focus behavior
- Navigation behavior
- Animation behavior
- Scrolling behavior
- Column management
- Data structures
- State management

Do NOT directly copy Niri's Wayland-specific architecture into this project.

In particular, do not attempt to reproduce or port:

- Wayland protocol implementation
- Wayland compositor internals
- Smithay integration
- Linux-specific window management
- DRM/KMS backends
- Linux-specific input backends

Windows has a fundamentally different window management model.

The Niri source directory must be ignored by Git.

Do not modify files inside the Niri reference directory unless explicitly requested.

---

# Code Organization

- Split code by clear responsibilities (layout, animation state, overview,
  focus/borders, input, Win32 backend), rather than continually growing one file.
- Keep the application shell focused on orchestration; put independently
  testable geometry and state transitions in dedicated modules with their tests.
- Avoid arbitrary line-count splits or needless abstraction. A larger file is
  acceptable when separating it would obscure a cohesive responsibility.

---

# Core Architecture

This project runs on Windows.

Unlike Niri, this project cannot directly become the system compositor.

Windows already provides:

- The Windows window manager
- Desktop Window Manager (DWM)
- HWND-based application windows
- Existing application window ownership and lifecycle management

Therefore, this project should act as a window management layer on top of Windows.

Conceptually:

```text
Application Windows
        |
       HWND
        |
Windows Window Manager
        |
       DWM
        |
------------------------
        |
This Project
        |
+-------+-------+
|               |
Window Tracking Layout Engine
Focus Management Input Handling
Animation System Windows Backend
