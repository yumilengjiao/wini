//! Win32 backend: everything that talks to the operating system.
//!
//! Utility functions here are provided ahead of the modules that will use
//! them, so unused-code warnings are silenced inside this module tree.
#![allow(dead_code)]

pub mod api;
mod bar_geometry;
pub mod decorations;
pub mod events;
pub mod focus_border;
pub mod frame;
pub mod layering;
pub mod monitor;
pub mod msg_window;
pub mod overlay;
pub mod placement;
pub mod tab_indicator;
pub mod thumbnail;
pub mod tray;
pub mod window;
