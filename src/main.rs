//! wini: a scrollable-tiling window manager for Windows, inspired by Niri.
//!
//! This program acts as a window *management layer* on top of the existing
//! Windows window manager / DWM. It tracks application windows (HWNDs),
//! arranges them into a scrollable column-based layout, and drives focus
//! and animation — without replacing the OS compositor.

mod anim;
mod app;
mod layout;
mod config;
mod input;
mod logging;
mod win;

use std::process::ExitCode;

/// Hide the console window when wini owns it (launched from Explorer,
/// a shortcut or autostart): a window manager should start silently,
/// not pop a black console box. When launched from a terminal the
/// console is shared with the shell (more than one process attached),
/// so it stays visible for log reading and Ctrl+C.
fn hide_owned_console() {
    unsafe {
        use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
        let mut procs = [0u32; 8];
        let attached = GetConsoleProcessList(&mut procs);
        if attached == 1 {
            let hwnd = GetConsoleWindow();
            if !hwnd.0.is_null() {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

fn main() -> ExitCode {
    logging::init();
    hide_owned_console();

    // Per-monitor-v2 DPI awareness: without it Windows virtualizes all
    // coordinates we read/write (GetMonitorInfo, GetWindowRect,
    // SetWindowPos) per target window, causing rounding drift — windows
    // overlapping or spread too far apart, especially right after a new
    // window opens or a column resize. Must run before any HWND work.
    unsafe {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            .is_err()
        {
            log::warn!("failed to set per-monitor DPI awareness; geometry may drift");
        }
    }

    log::info!(
        "wini v{} starting (pid {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );

    match app::App::new() {
        Ok(mut app) => {
            if let Err(err) = app.run() {
                log::error!("fatal error: {err}");
                ExitCode::FAILURE
            } else {
                log::info!("wini exited cleanly");
                ExitCode::SUCCESS
            }
        }
        Err(err) => {
            log::error!("failed to start: {err}");
            ExitCode::FAILURE
        }
    }
}
