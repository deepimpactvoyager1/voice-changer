//! System tray. Menu/click events run on background threads so the tray works while the window is hidden.
use crate::hotkeys::SharedTx;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};

pub static QUIT: AtomicBool = AtomicBool::new(false);
pub fn quit_requested() -> bool { QUIT.load(Ordering::Relaxed) }
/// Real quit (bypasses "close hides to tray").
pub fn request_quit(ctx: &egui::Context) {
    QUIT.store(true, Ordering::Relaxed);
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    ctx.request_repaint();
}

pub fn show_window(ctx: &egui::Context) {
    #[cfg(windows)]
    win::show_main_window();
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, &mut pid);
        // our titled, un-owned top-level window (skips the hidden helper windows)
        if pid == GetCurrentProcessId() && GetWindowTextLengthW(h) > 0 && GetWindow(h, GW_OWNER) == 0 {
            *(lp as *mut HWND) = h;
            return 0;
        }
        1
    }

    /// eframe may not run update() while hidden, so viewport commands alone can be lost.
    pub fn show_main_window() {
        unsafe {
            let mut h: HWND = 0;
            EnumWindows(Some(cb), &mut h as *mut HWND as LPARAM);
            if h != 0 {
                ShowWindow(h, SW_RESTORE);
                SetForegroundWindow(h);
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::{hotkeys::send_cmd, settings::{Cmd, Params}};
    use std::sync::Arc;
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    /// Create on the main thread; dropping it removes the icon (done in App::on_exit).
    pub struct Tray { _icon: TrayIcon, mute: CheckMenuItem }

    impl Tray {
        pub fn new(ctx: egui::Context, tx: SharedTx, params: Arc<Params>) -> Option<Self> {
            let icon = Icon::from_rgba(include_bytes!("../assets/icon_32.rgba").to_vec(), 32, 32).ok()?;
            let show = MenuItem::new("Show window", true, None);
            let mute = CheckMenuItem::new("Mute mic", true, params.is_muted(), None);
            let stop = MenuItem::new("Stop all sounds", true, None);
            let quit = MenuItem::new("Quit", true, None);
            let menu = Menu::new();
            menu.append_items(&[&show, &PredefinedMenuItem::separator(), &mute, &stop, &PredefinedMenuItem::separator(), &quit]).ok()?;
            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Voice Changer")
                .with_icon(icon)
                .build().ok()?;

            let (sid, mid, tid, qid) = (show.id().clone(), mute.id().clone(), stop.id().clone(), quit.id().clone());
            let c = ctx.clone();
            std::thread::Builder::new().name("tray-menu".into()).spawn(move || {
                while let Ok(ev) = MenuEvent::receiver().recv() {
                    if ev.id == sid { show_window(&c); }
                    else if ev.id == mid { params.toggle_mute(); c.request_repaint(); }
                    else if ev.id == tid { send_cmd(&tx, Cmd::StopAll); }
                    else if ev.id == qid {
                        // show first so the UI thread is awake to process Close; watchdog guarantees exit
                        show_window(&c);
                        request_quit(&c);
                        std::thread::spawn(|| { std::thread::sleep(std::time::Duration::from_secs(3)); std::process::exit(0); });
                    }
                }
            }).ok()?;
            let c = ctx.clone();
            std::thread::Builder::new().name("tray-click".into()).spawn(move || {
                while let Ok(ev) = TrayIconEvent::receiver().recv() {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        show_window(&c);
                    }
                }
            }).ok()?;
            Some(Self { _icon: tray, mute })
        }

        pub fn sync_mute(&self, muted: bool) {
            if self.mute.is_checked() != muted { self.mute.set_checked(muted); }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub struct Tray;
    impl Tray {
        pub fn new(_ctx: egui::Context, _tx: SharedTx, _p: std::sync::Arc<crate::settings::Params>) -> Option<Self> { None }
        pub fn sync_mute(&self, _m: bool) {}
    }
}
pub use imp::Tray;