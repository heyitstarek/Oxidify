//! Desktop integration: single-instance guard, system tray, and media keys.
//! Everything here is best-effort: if the OS refuses a hook, the player
//! keeps working without it and logs the reason. Event receivers are global
//! and polled non-blockingly once per frame.

/// Commands arriving from outside the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopCommand {
    Show,
    TogglePlay,
    Next,
    Prev,
    Quit,
}

/// The first instance keeps this guard alive for the whole process.
pub fn acquire_single_instance() -> single_instance::SingleInstance {
    let guard = single_instance::SingleInstance::new("com.oxidify.desktop")
        .expect("single-instance guard");
    if !guard.is_single() {
        eprintln!("Oxidify is already running.");
        std::process::exit(0);
    }
    guard
}

pub struct DesktopHooks {
    tray: Option<tray_icon::TrayIcon>,
    _hotkeys: Option<global_hotkey::GlobalHotKeyManager>,
    hotkey_ids: Vec<(u32, DesktopCommand)>,
    menu_show: Option<tray_icon::menu::MenuId>,
    menu_toggle: Option<tray_icon::menu::MenuId>,
    menu_next: Option<tray_icon::menu::MenuId>,
    menu_prev: Option<tray_icon::menu::MenuId>,
    menu_quit: Option<tray_icon::menu::MenuId>,
}

impl DesktopHooks {
    pub fn new() -> Self {
        let mut hooks = Self {
            tray: None,
            _hotkeys: None,
            hotkey_ids: Vec::new(),
            menu_show: None,
            menu_toggle: None,
            menu_next: None,
            menu_prev: None,
            menu_quit: None,
        };
        hooks.init_tray();
        hooks.init_hotkeys();
        hooks
    }

    fn init_tray(&mut self) {
        use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
        let result: anyhow::Result<()> = (|| {
            let menu = Menu::new();
            let show = MenuItem::new("Show Oxidify", true, None);
            let toggle = MenuItem::new("Play / Pause", true, None);
            let next = MenuItem::new("Next track", true, None);
            let prev = MenuItem::new("Previous track", true, None);
            let quit = MenuItem::new("Quit", true, None);
            menu.append_items(&[
                &show,
                &PredefinedMenuItem::separator(),
                &toggle,
                &next,
                &prev,
                &PredefinedMenuItem::separator(),
                &quit,
            ])?;
            self.menu_show = Some(show.id().clone());
            self.menu_toggle = Some(toggle.id().clone());
            self.menu_next = Some(next.id().clone());
            self.menu_prev = Some(prev.id().clone());
            self.menu_quit = Some(quit.id().clone());

            let tray = tray_icon::TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("Oxidify")
                .with_icon(app_icon())
                .build()?;
            self.tray = Some(tray);
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("tray unavailable: {e:#}");
        }
    }

    fn init_hotkeys(&mut self) {
        use global_hotkey::hotkey::{Code, HotKey};
        let result: anyhow::Result<()> = (|| {
            let manager = global_hotkey::GlobalHotKeyManager::new()?;
            let bindings = [
                (Code::MediaPlayPause, DesktopCommand::TogglePlay),
                (Code::MediaTrackNext, DesktopCommand::Next),
                (Code::MediaTrackPrevious, DesktopCommand::Prev),
            ];
            for (code, cmd) in bindings {
                let hk = HotKey::new(None, code);
                manager.register(hk)?;
                self.hotkey_ids.push((hk.id(), cmd));
            }
            self._hotkeys = Some(manager);
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("media keys unavailable: {e:#}");
        }
    }

    /// Drain menu + hotkey events delivered since the last frame.
    pub fn poll(&mut self) -> Vec<DesktopCommand> {
        let mut out = Vec::new();
        while let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            let id = ev.id();
            if Some(id) == self.menu_quit.as_ref() {
                out.push(DesktopCommand::Quit);
            } else if Some(id) == self.menu_next.as_ref() {
                out.push(DesktopCommand::Next);
            } else if Some(id) == self.menu_prev.as_ref() {
                out.push(DesktopCommand::Prev);
            } else if Some(id) == self.menu_toggle.as_ref() {
                out.push(DesktopCommand::TogglePlay);
            } else if Some(id) == self.menu_show.as_ref() {
                out.push(DesktopCommand::Show);
            }
        }
        while let Ok(ev) = global_hotkey::GlobalHotKeyEvent::receiver().try_recv() {
            for (id, cmd) in &self.hotkey_ids {
                if *id == ev.id() {
                    out.push(*cmd);
                }
            }
        }
        out
    }

    pub fn notify_track(&self, title: &str, artists: &str) {
        if let Some(tray) = &self.tray {
            let tip = if artists.is_empty() {
                format!("Oxidify — {title}")
            } else {
                format!("Oxidify — {title} · {artists}")
            };
            let _ = tray.set_tooltip(Some(tip));
        }
    }
}

/// 32×32 amber play mark on a dark rounded square, generated at boot.
fn app_icon() -> tray_icon::Icon {
    const S: usize = 32;
    let mut rgba = vec![0u8; S * S * 4];
    for y in 0..S {
        for x in 0..S {
            let dx = (x as f32 - 15.5) / 15.5;
            let dy = (y as f32 - 15.5) / 15.5;
            let inside = dx * dx + dy * dy <= 1.0;
            // Play triangle pointing right.
            let tri = x >= 12 && x <= 22 && (y as i32 - 16).abs() * 2 <= (x as i32 - 11);
            let (rr, gg, bb, aa) = if tri {
                (245, 176, 77, 255)
            } else if inside {
                (24, 26, 34, 255)
            } else {
                (0, 0, 0, 0)
            };
            let i = (y * S + x) * 4;
            rgba[i] = rr;
            rgba[i + 1] = gg;
            rgba[i + 2] = bb;
            rgba[i + 3] = aa;
        }
    }
    tray_icon::Icon::from_rgba(rgba, S as u32, S as u32).expect("tray icon")
}
