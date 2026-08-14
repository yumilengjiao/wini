//! System tray icon (Notification Area) with a context menu for the
//! common quick actions: opening / reloading the config, toggling
//! autostart, and quitting.
//!
//! The `tray-icon` crate delivers menu events through a global
//! crossbeam channel; we poll it from the animation timer (the app is
//! a Win32 message-loop program, not async), which keeps everything
//! on the main thread like every other input path.
//!
//! The icon is the project logo (assets/favicon.svg) pre-rasterized
//! to a 32x32 RGBA blob (tray icons on Windows are 16x16 at 100%
//! scale, 32x32 at 200%; the shell scales what we give it).

use tray_icon::menu::{CheckMenuItem, MenuEvent};
use tray_icon::{TrayIcon, TrayIconBuilder};

/// A tray-menu action the app should perform. Returned by
/// [`Tray::poll_events`], which the main timers call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Open the config file in the system default editor.
    OpenConfig,
    /// Reload the config right now (don't wait for the mtime poll).
    ReloadConfig,
    /// Flip the autostart (HKCU Run key) state.
    ToggleAutostart,
    /// Quit the program.
    Quit,
}

/// Read whether the autostart registry entry exists.
pub fn autostart_enabled() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ,
    };
    let mut buf = [0u16; 512];
    let mut cb = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            w!("yumi-wini"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut cb),
        )
        .is_ok()
    }
}

/// Create or remove the autostart registry entry. Returns the new
/// state on success.
pub fn set_autostart(on: bool) -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
    };
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else {
                return false;
            };
            let mut path: Vec<u16> = exe.as_os_str().to_string_lossy().encode_utf16().collect();
            // value must be null-terminated
            path.push(0);
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                w!("yumi-wini"),
                REG_SZ.0,
                Some(path.as_ptr().cast()),
                (path.len() * 2) as u32,
            )
            .is_ok()
        } else {
            // ERROR_FILE_NOT_FOUND is fine (nothing to remove).
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                w!("yumi-wini"),
            )
            .is_ok()
        }
    }
}

/// The tray icon and its menu. Keep alive for the process lifetime
/// (dropping removes the icon).
pub struct Tray {
    _icon: TrayIcon,
    /// Handle to the checkable autostart item (to reflect registry
    /// state after toggles).
    autostart_item: CheckMenuItem,
}

impl Tray {
    /// Create the tray icon. Must be called on the thread that runs
    /// the Win32 message loop (the tray window needs it).
    pub fn new() -> Option<Tray> {
        let rgba: &[u8] = include_bytes!("../../assets/tray-32.rgba");
        let icon = tray_icon::Icon::from_rgba(rgba.to_vec(), 32, 32).ok()?;

        let menu = tray_icon::menu::Menu::new();
        let header = tray_icon::menu::MenuItem::with_id(
            "header",
            format!("yumi-wini v{}", env!("CARGO_PKG_VERSION")),
            false,
            None,
        );
        let open =
            tray_icon::menu::MenuItem::with_id("open-config", "打开配置文件", true, None);
        let reload =
            tray_icon::menu::MenuItem::with_id("reload-config", "重载配置", true, None);
        let autostart = tray_icon::menu::CheckMenuItem::with_id(
            "toggle-autostart",
            "开机自启动",
            true,
            autostart_enabled(),
            None,
        );
        let quit = tray_icon::menu::MenuItem::with_id("quit", "退出", true, None);

        menu.append(&header).ok()?;
        menu.append(&open).ok()?;
        menu.append(&reload).ok()?;
        menu.append(&tray_icon::menu::PredefinedMenuItem::separator())
            .ok()?;
        menu.append(&autostart).ok()?;
        menu.append(&tray_icon::menu::PredefinedMenuItem::separator())
            .ok()?;
        menu.append(&quit).ok()?;

        let icon = TrayIconBuilder::new()
            .with_tooltip(format!(
                "yumi-wini v{} — 右键打开菜单",
                env!("CARGO_PKG_VERSION")
            ))
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(true)
            .build()
            .ok()?;

        Some(Tray {
            _icon: icon,
            autostart_item: autostart,
        })
    }

    /// Non-blocking poll of the global menu-event channel. Call from
    /// a main-thread timer.
    pub fn poll_events(&mut self) -> Vec<TrayAction> {
        let mut out = Vec::new();
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let action = match ev.id.0.as_str() {
                "open-config" => Some(TrayAction::OpenConfig),
                "reload-config" => Some(TrayAction::ReloadConfig),
                "toggle-autostart" => Some(TrayAction::ToggleAutostart),
                "quit" => Some(TrayAction::Quit),
                _ => None,
            };
            if let Some(a) = action {
                out.push(a);
            }
        }
        out
    }

    /// Reflect a new autostart state on the checkable menu item.
    pub fn set_autostart_checked(&self, on: bool) {
        self.autostart_item.set_checked(on);
    }
}
