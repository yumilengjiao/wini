//! Win32 backend: everything that talks to the operating system.
//!
//! Utility functions here are provided ahead of the modules that will use
//! them, so unused-code warnings are silenced inside this module tree.
#![allow(dead_code)]

pub mod api;
pub mod events;
pub mod focus_border;
pub mod monitor;
pub mod msg_window;
pub mod overlay;
pub mod placement;
pub mod tray;
pub mod window;
