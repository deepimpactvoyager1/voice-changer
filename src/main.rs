//! Real-time voice changer + soundboard with a desktop UI (egui).
//!
//!   cargo run --release
//!
//! USE HEADPHONES (or a monitor output), otherwise speakers feed back into the mic.

// Hide the console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod config;
mod dsp;
mod settings;
mod soundboard;
mod ui;
mod hotkeys;
mod tray;

use eframe::egui;

use ui::App;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 820.0])
            .with_min_inner_size([520.0, 540.0])
            .with_icon(egui::IconData {
                rgba: include_bytes!("../assets/icon_256.rgba").to_vec(),
                width: 256,
                height: 256,
            }),
        ..Default::default()
    };
    eframe::run_native("Voice Changer", options, Box::new(|cc| Box::new(App::new(cc.egui_ctx.clone()))))
}
