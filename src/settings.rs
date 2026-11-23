//! Settings shared between the UI thread and the audio thread: the full
//! effect-chain `Settings`, the shared `Params`, and the `Cmd` messages.

use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};

/// Every adjustable value of the effect chain. The UI sends a fresh copy only
/// when something changed; the audio thread picks it up once per buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Settings {
    // cleanup / input
    pub(crate) in_gain_db: f32,
    pub(crate) gate_db: f32, // noise gate threshold, <= -79 means off
    pub(crate) gate_atk_ms: f32,
    pub(crate) gate_rel_ms: f32,
    pub(crate) hpf_hz: f32, // high-pass, <= 25 means off
    pub(crate) ns_amount: f32, // noise suppression strength, ~0 means off (set from the main window)
    // tone & dynamics
    pub(crate) eq_low_db: f32,
    pub(crate) eq_mid_db: f32,
    pub(crate) eq_high_db: f32,
    pub(crate) comp_thresh_db: f32,
    pub(crate) comp_ratio: f32, // <= 1.05 means off
    pub(crate) comp_makeup_db: f32,
    pub(crate) deess: f32, // 0..1
    // voice
    pub(crate) semitones: f32, // pitch shift, ~0 means off
    pub(crate) pitch_grain_ms: f32,
    pub(crate) robot_amount: f32, // 0..1 blend of ring modulation
    pub(crate) robot_hz: f32,
    pub(crate) robot_wave: u8, // 0 sine, 1 square, 2 triangle, 3 saw
    pub(crate) whisper: f32,   // 0..1
    pub(crate) harm1_semi: f32, // harmonizer voice 1 (semitones above/below you)
    pub(crate) harm1_mix: f32,
    pub(crate) harm2_semi: f32,
    pub(crate) harm2_mix: f32,
    pub(crate) growl_depth: f32,
    pub(crate) growl_hz: f32,
    // modulation
    pub(crate) vib_depth: f32,
    pub(crate) vib_hz: f32,
    pub(crate) trem_depth: f32,
    pub(crate) trem_hz: f32,
    pub(crate) chorus_mix: f32,
    pub(crate) chorus_hz: f32,
    pub(crate) chorus_depth: f32,
    pub(crate) chorus_flange: u8, // 0 chorus, 1 flanger
    pub(crate) dbl_mix: f32, // voice doubler
    pub(crate) dbl_ms: f32,
    // echo / reverb
    pub(crate) echo_mix: f32,
    pub(crate) echo_fb: f32,
    pub(crate) echo_ms: f32,
    pub(crate) echo_damp: f32,
    pub(crate) reverb_mix: f32,
    pub(crate) reverb_size: f32,
    // lo-fi / "cheap mic"
    pub(crate) drive: f32, // distortion 0..1
    pub(crate) dist_type: u8, // 0 soft, 1 hard, 2 fold
    pub(crate) bits: f32,   // bit depth, >= 16 means off
    pub(crate) rate_hz: f32, // sample-rate reduction, >= engine rate means off
    pub(crate) lpf_hz: f32, // low-pass, >= 19000 means off
    pub(crate) hiss: f32,   // noise floor 0..0.1
    // output
    pub(crate) out_gain: f32,
    pub(crate) muted: bool, // mutes the mic only; soundboard keeps playing
    pub(crate) sb_vol: f32, // soundboard volume
    pub(crate) mon_vol: f32, // monitor (headphones) volume
    pub(crate) limiter: bool, // smooth peak limiter instead of hard clipping
}

impl Settings {
    /// Everything off: the voice passes through untouched.
    pub(crate) fn neutral() -> Self {
        Self {
            in_gain_db: 0.0,
            gate_db: -45.0,
            gate_atk_ms: 2.0,
            gate_rel_ms: 80.0,
            hpf_hz: 20.0,
            ns_amount: 0.0,
            eq_low_db: 0.0,
            eq_mid_db: 0.0,
            eq_high_db: 0.0,
            comp_thresh_db: -18.0,
            comp_ratio: 1.0,
            comp_makeup_db: 0.0,
            deess: 0.0,
            semitones: 0.0,
            pitch_grain_ms: 50.0,
            robot_amount: 0.0,
            robot_hz: 50.0,
            robot_wave: 0,
            whisper: 0.0,
            harm1_semi: 4.0,
            harm1_mix: 0.0,
            harm2_semi: 7.0,
            harm2_mix: 0.0,
            growl_depth: 0.0,
            growl_hz: 40.0,
            vib_depth: 0.0,
            vib_hz: 5.0,
            trem_depth: 0.0,
            trem_hz: 5.0,
            chorus_mix: 0.0,
            chorus_hz: 0.8,
            chorus_depth: 0.5,
            chorus_flange: 0,
            dbl_mix: 0.0,
            dbl_ms: 20.0,
            echo_mix: 0.0,
            echo_fb: 0.45,
            echo_ms: 300.0,
            echo_damp: 0.0,
            reverb_mix: 0.0,
            reverb_size: 0.5,
            drive: 0.0,
            dist_type: 0,
            bits: 16.0,
            rate_hz: 48000.0,
            lpf_hz: 20000.0,
            hiss: 0.0,
            out_gain: 1.0,
            muted: false,
            sb_vol: 1.0,
            mon_vol: 1.0,
            limiter: true,
        }
    }

    /// All float fields by name (used for saving / loading presets).
    fn f32_fields(&mut self) -> Vec<(&'static str, &mut f32)> {
        vec![
            ("in_gain_db", &mut self.in_gain_db),
            ("gate_db", &mut self.gate_db),
            ("gate_atk_ms", &mut self.gate_atk_ms),
            ("gate_rel_ms", &mut self.gate_rel_ms),
            ("hpf_hz", &mut self.hpf_hz),
            ("ns_amount", &mut self.ns_amount),
            ("eq_low_db", &mut self.eq_low_db),
            ("eq_mid_db", &mut self.eq_mid_db),
            ("eq_high_db", &mut self.eq_high_db),
            ("comp_thresh_db", &mut self.comp_thresh_db),
            ("comp_ratio", &mut self.comp_ratio),
            ("comp_makeup_db", &mut self.comp_makeup_db),
            ("deess", &mut self.deess),
            ("semitones", &mut self.semitones),
            ("pitch_grain_ms", &mut self.pitch_grain_ms),
            ("robot_amount", &mut self.robot_amount),
            ("robot_hz", &mut self.robot_hz),
            ("whisper", &mut self.whisper),
            ("harm1_semi", &mut self.harm1_semi),
            ("harm1_mix", &mut self.harm1_mix),
            ("harm2_semi", &mut self.harm2_semi),
            ("harm2_mix", &mut self.harm2_mix),
            ("growl_depth", &mut self.growl_depth),
            ("growl_hz", &mut self.growl_hz),
            ("vib_depth", &mut self.vib_depth),
            ("vib_hz", &mut self.vib_hz),
            ("trem_depth", &mut self.trem_depth),
            ("trem_hz", &mut self.trem_hz),
            ("chorus_mix", &mut self.chorus_mix),
            ("chorus_hz", &mut self.chorus_hz),
            ("chorus_depth", &mut self.chorus_depth),
            ("dbl_mix", &mut self.dbl_mix),
            ("dbl_ms", &mut self.dbl_ms),
            ("echo_mix", &mut self.echo_mix),
            ("echo_fb", &mut self.echo_fb),
            ("echo_ms", &mut self.echo_ms),
            ("echo_damp", &mut self.echo_damp),
            ("reverb_mix", &mut self.reverb_mix),
            ("reverb_size", &mut self.reverb_size),
            ("drive", &mut self.drive),
            ("bits", &mut self.bits),
            ("rate_hz", &mut self.rate_hz),
            ("lpf_hz", &mut self.lpf_hz),
            ("hiss", &mut self.hiss),
        ]
    }

    /// "name=value;name=value;..." (no spaces, no newlines).
    pub(crate) fn serialize(&self) -> String {
        let mut copy = *self;
        let mut parts: Vec<String> = Vec::new();
        for (name, value) in copy.f32_fields() {
            parts.push(format!("{}={}", name, value));
        }
        parts.push(format!("robot_wave={}", self.robot_wave));
        parts.push(format!("dist_type={}", self.dist_type));
        parts.push(format!("chorus_flange={}", self.chorus_flange));
        parts.join(";")
    }

    /// Applies a string made by `serialize`. Unknown or invalid entries are ignored.
    pub(crate) fn apply(&mut self, text: &str) {
        for part in text.split(';') {
            let Some((key, val)) = part.split_once('=') else { continue };
            match key {
                "robot_wave" => {
                    if let Ok(n) = val.parse::<u8>() {
                        self.robot_wave = n.min(3);
                    }
                }
                "dist_type" => {
                    if let Ok(n) = val.parse::<u8>() {
                        self.dist_type = n.min(2);
                    }
                }
                "chorus_flange" => {
                    if let Ok(n) = val.parse::<u8>() {
                        self.chorus_flange = n.min(1);
                    }
                }
                _ => {
                    if let Ok(n) = val.parse::<f32>() {
                        if n.is_finite() {
                            for (name, slot) in self.f32_fields() {
                                if name == key {
                                    let (lo, hi) = range_of(key);
                                    *slot = n.clamp(lo, hi);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The allowed range of every float setting (same as the sliders). Used to
/// keep imported presets and saved files from loading absurd values.
fn range_of(name: &str) -> (f32, f32) {
    match name {
        "in_gain_db" => (-20.0, 30.0),
        "gate_db" => (-80.0, -20.0),
        "gate_atk_ms" => (0.5, 50.0),
        "gate_rel_ms" => (10.0, 500.0),
        "hpf_hz" => (20.0, 600.0),
        "eq_low_db" | "eq_mid_db" | "eq_high_db" => (-12.0, 12.0),
        "comp_thresh_db" => (-50.0, 0.0),
        "comp_ratio" => (1.0, 20.0),
        "comp_makeup_db" => (0.0, 24.0),
        "semitones" | "harm1_semi" | "harm2_semi" => (-24.0, 24.0),
        "pitch_grain_ms" => (20.0, 100.0),
        "robot_hz" => (10.0, 200.0),
        "vib_hz" => (0.5, 12.0),
        "trem_hz" => (0.5, 20.0),
        "chorus_hz" => (0.1, 5.0),
        "echo_fb" => (0.0, 0.9),
        "echo_ms" => (50.0, 800.0),
        "growl_hz" => (10.0, 120.0),
        "dbl_ms" => (10.0, 40.0),
        "bits" => (4.0, 16.0),
        "rate_hz" => (2000.0, 48000.0),
        "lpf_hz" => (1000.0, 20000.0),
        "hiss" => (0.0, 0.1),
        _ => (0.0, 1.0), // every other setting is a 0..1 amount
    }
}

pub(crate) struct Params {
    pub(crate) settings: Mutex<Settings>,
    // Peak levels as f32 bits. Non-negative floats order like their bit
    // patterns, so fetch_max is a peak-hold; the UI resets it with swap(0).
    pub(crate) in_peak: AtomicU32,
    pub(crate) out_peak: AtomicU32,
}

impl Params {
    pub(crate) fn new() -> Self {
        Self {
            settings: Mutex::new(Settings::neutral()),
            in_peak: AtomicU32::new(0),
            out_peak: AtomicU32::new(0),
        }
    }
}

pub(crate) enum Cmd {
    Play { data: Arc<Vec<i16>>, gain: f32, step: f64, looping: bool, start: f64, end: f64, vc: bool },
    StopAll,
}
