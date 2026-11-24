//! The egui application: `App` state, startup and engine restarts.
//! Tabs, widgets, persistence and the frame loop live in the sub-modules.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;

use crate::audio::{device_names, Engine};
use crate::config::config_path;
use crate::settings::{Params, Settings};
use crate::soundboard::{LoadMsg, Sound};

mod frame;
mod persist;
mod sound_tab;
mod sounds;
mod voice_tab;
mod widgets;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Custom,
    Soundboard,
}

#[derive(Default)]
struct RowActions {
    remove: Option<usize>,
    retry: Option<usize>,
}

pub(crate) struct App {
    ctx: egui::Context,
    params: Arc<Params>,
    engine: Option<Engine>,
    error: Option<String>,
    notice: Option<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    in_sel: String,  // "" = system default
    out_sel: String, // "" = system default
    mon_sel: String, // "" = off

    tab: Tab,

    // the voice (full effect chain) and saved presets
    custom: Settings,
    presets: Vec<(String, String)>,
    preset_name: String,

    // common
    ns_amount: f32,
    out_gain: f32,
    muted: bool,
    sb_vol: f32,
    mon_vol: f32,
    limiter: bool,
    last_sent: Settings,
    in_level: f32,
    out_level: f32,
    clip_frames: u32,

    // soundboard
    sounds: Vec<Sound>,
    groups: Vec<String>,
    new_group: String,
    next_id: usize,
    load_tx: Sender<LoadMsg>,
    load_rx: Receiver<LoadMsg>,

    // saved settings
    last_saved: String,
    last_check: Instant,
    noise_prof: Vec<f32>, // learned room-noise profile (empty = none)
    noise_rate: u32,      // sample rate it was learned at

    // hotkeys & tray
    hotkeys: Option<crate::hotkeys::Hotkeys>,
    tray: Option<crate::tray::Tray>,
    shared_tx: crate::hotkeys::SharedTx,
    hk: crate::hotkeys::HotkeyConfig,
    hk_status: std::collections::HashMap<String, crate::hotkeys::Status>,
    hk_dirty: bool,
    hide_to_tray: bool,
}

impl App {
    pub(crate) fn new(ctx: egui::Context) -> Self {
        let params = Arc::new(Params::new());
        let shared_tx: crate::hotkeys::SharedTx = Default::default();
        let hotkeys = crate::hotkeys::Hotkeys::new(ctx.clone());
        let tray = crate::tray::Tray::new(ctx.clone(), shared_tx.clone(), params.clone());
        let (load_tx, load_rx) = mpsc::channel();
        let mut app = Self {
            ctx,
            params,
            engine: None,
            error: None,
            notice: None,
            inputs: device_names(true),
            outputs: device_names(false),
            in_sel: String::new(),
            out_sel: String::new(),
            mon_sel: String::new(),
            tab: Tab::Custom,
            custom: Settings::neutral(),
            presets: Vec::new(),
            preset_name: String::new(),
            ns_amount: 0.0,
            out_gain: 1.0,
            muted: false,
            sb_vol: 1.0,
            mon_vol: 1.0,
            limiter: true,
            last_sent: Settings::neutral(),
            in_level: 0.0,
            out_level: 0.0,
            clip_frames: 0,
            sounds: Vec::new(),
            groups: Vec::new(),
            new_group: String::new(),
            next_id: 0,
            load_tx,
            load_rx,
            last_saved: String::new(),
            last_check: Instant::now(),
            noise_prof: Vec::new(),
            noise_rate: 0,
            hotkeys,
            tray,
            shared_tx,
            hk: Default::default(),
            hk_status: Default::default(),
            hk_dirty: true,
            hide_to_tray: true,
        };

        let mut specs = Vec::new();
        if let Ok(text) = std::fs::read_to_string(config_path()) {
            specs = app.load_config(&text);
        }
        app.start_with_fallback();
        app.add_sounds(specs);
        app
    }

    fn restart_engine(&mut self) {
        self.engine = None; // drop old streams first so devices are released
        match Engine::start(&self.in_sel, &self.out_sel, &self.mon_sel, self.params.clone()) {
            Ok(e) => {
                let rate = e.sample_rate;
                for snd in &mut self.sounds {
                    snd.convert_to(rate); // no-op unless the device's rate changed
                }
                self.engine = Some(e);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
                    
        }
        if let Ok(mut g) = self.shared_tx.lock() {
            *g = self.engine.as_ref().map(|e| e.tx.clone());
        }
        self.hk_dirty = true;
        self.push_noise_profile();
    }

    /// Starts audio; if a saved device is gone, falls back to the system defaults.
    fn start_with_fallback(&mut self) {
        self.restart_engine();
        if self.error.is_some() && !(self.in_sel.is_empty() && self.out_sel.is_empty() && self.mon_sel.is_empty()) {
            self.in_sel.clear();
            self.out_sel.clear();
            self.mon_sel.clear();
            self.restart_engine();
        }
    }

    /// Called after every engine start: a new Processor has no profile yet.
    fn push_noise_profile(&self) {
        self.params.ns_state.store(0, std::sync::atomic::Ordering::Relaxed);
        if let Some(e) = &self.engine {
            if !self.noise_prof.is_empty() && e.sample_rate == self.noise_rate {
                let _ = e.tx.send(crate::settings::Cmd::SetNoise(Arc::new(self.noise_prof.clone())));
            }
        }
    }
}
