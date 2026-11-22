//! Real-time voice changer + soundboard with a desktop UI (egui).
//!
//!   cargo run --release
//!
//! USE HEADPHONES (or a monitor output), otherwise speakers feed back into the mic.

// Hide the console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::f32::consts::PI;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, SampleRate, Stream, StreamConfig};
use eframe::egui;
use ringbuf::{traits::*, HeapCons, HeapProd, HeapRb};

// ---------------------------------------------------------------------------
// Settings shared between the UI thread and the audio thread
// ---------------------------------------------------------------------------

/// Every adjustable value of the effect chain. The UI sends a fresh copy only
/// when something changed; the audio thread picks it up once per buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    // cleanup / input
    in_gain_db: f32,
    gate_db: f32, // noise gate threshold, <= -79 means off
    gate_atk_ms: f32,
    gate_rel_ms: f32,
    hpf_hz: f32, // high-pass, <= 25 means off
    ns_amount: f32, // noise suppression strength, ~0 means off (set from the main window)
    // tone & dynamics
    eq_low_db: f32,
    eq_mid_db: f32,
    eq_high_db: f32,
    comp_thresh_db: f32,
    comp_ratio: f32, // <= 1.05 means off
    comp_makeup_db: f32,
    deess: f32, // 0..1
    // voice
    semitones: f32, // pitch shift, ~0 means off
    pitch_grain_ms: f32,
    robot_amount: f32, // 0..1 blend of ring modulation
    robot_hz: f32,
    robot_wave: u8, // 0 sine, 1 square, 2 triangle, 3 saw
    whisper: f32,   // 0..1
    // modulation
    vib_depth: f32,
    vib_hz: f32,
    trem_depth: f32,
    trem_hz: f32,
    chorus_mix: f32,
    chorus_hz: f32,
    chorus_depth: f32,
    chorus_flange: u8, // 0 chorus, 1 flanger
    // echo / reverb
    echo_mix: f32,
    echo_fb: f32,
    echo_ms: f32,
    echo_damp: f32,
    reverb_mix: f32,
    reverb_size: f32,
    // lo-fi / "cheap mic"
    drive: f32, // distortion 0..1
    dist_type: u8, // 0 soft, 1 hard, 2 fold
    bits: f32,   // bit depth, >= 16 means off
    rate_hz: f32, // sample-rate reduction, >= engine rate means off
    lpf_hz: f32, // low-pass, >= 19000 means off
    hiss: f32,   // noise floor 0..0.1
    // output
    out_gain: f32,
    muted: bool, // mutes the mic only; soundboard keeps playing
    sb_vol: f32, // soundboard volume
    mon_vol: f32, // monitor (headphones) volume
}

impl Settings {
    /// Everything off: the voice passes through untouched.
    fn neutral() -> Self {
        Self {
            in_gain_db: 0.0,
            gate_db: -80.0,
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
            vib_depth: 0.0,
            vib_hz: 5.0,
            trem_depth: 0.0,
            trem_hz: 5.0,
            chorus_mix: 0.0,
            chorus_hz: 0.8,
            chorus_depth: 0.5,
            chorus_flange: 0,
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
            ("vib_depth", &mut self.vib_depth),
            ("vib_hz", &mut self.vib_hz),
            ("trem_depth", &mut self.trem_depth),
            ("trem_hz", &mut self.trem_hz),
            ("chorus_mix", &mut self.chorus_mix),
            ("chorus_hz", &mut self.chorus_hz),
            ("chorus_depth", &mut self.chorus_depth),
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
    fn serialize(&self) -> String {
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
    fn apply(&mut self, text: &str) {
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
                                    *slot = n;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

struct Params {
    settings: Mutex<Settings>,
    // Peak levels as f32 bits. Non-negative floats order like their bit
    // patterns, so fetch_max is a peak-hold; the UI resets it with swap(0).
    in_peak: AtomicU32,
    out_peak: AtomicU32,
}

impl Params {
    fn new() -> Self {
        Self {
            settings: Mutex::new(Settings::neutral()),
            in_peak: AtomicU32::new(0),
            out_peak: AtomicU32::new(0),
        }
    }
}

enum Cmd {
    Play { data: Arc<Vec<i16>>, gain: f32, step: f64, looping: bool },
    StopAll,
}

// ---------------------------------------------------------------------------
// DSP
// ---------------------------------------------------------------------------

/// Second-order filter (Butterworth low/high-pass, peaking EQ, shelves).
#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn set_norm(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    fn set(&mut self, lowpass: bool, fc: f32, sr: f32) {
        let w0 = 2.0 * PI * fc.clamp(10.0, sr * 0.45) / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * 0.7071);
        let (b0, b1, b2) = if lowpass {
            ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0)
        } else {
            ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0)
        };
        self.set_norm(b0, b1, b2, 1.0 + alpha, -2.0 * cos, 1.0 - alpha);
    }

    fn peak(&mut self, fc: f32, gain_db: f32, q: f32, sr: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * fc.clamp(10.0, sr * 0.45) / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        self.set_norm(1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a);
    }

    fn shelf(&mut self, high: bool, fc: f32, gain_db: f32, sr: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * fc.clamp(10.0, sr * 0.45) / sr;
        let (sin, cos) = w0.sin_cos();
        let beta = 2.0 * a.sqrt() * (sin * 0.5 * 2f32.sqrt());
        if high {
            self.set_norm(
                a * ((a + 1.0) + (a - 1.0) * cos + beta),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - beta),
                (a + 1.0) - (a - 1.0) * cos + beta,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - beta,
            );
        } else {
            self.set_norm(
                a * ((a + 1.0) - (a - 1.0) * cos + beta),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - beta),
                (a + 1.0) + (a - 1.0) * cos + beta,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - beta,
            );
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Reads a fractionally delayed sample from a circular buffer. `write` is the
/// index of the NEXT write, so the newest stored sample sits at `write - 1`.
fn read_frac(buf: &[f32], write: usize, delay: f32) -> f32 {
    let len = buf.len();
    let pos = (write as f32 - 1.0 - delay).rem_euclid(len as f32);
    let fl = pos.floor();
    let i0 = fl as usize % len;
    let i1 = (i0 + 1) % len;
    let frac = pos - fl;
    buf[i0] * (1.0 - frac) + buf[i1] * frac
}

/// Flush denormal floats to zero on this thread. Denormals make filter and
/// echo tails up to ~100x slower on some CPUs, which steals time from the
/// whole PC (this is a classic cause of audio apps making other apps lag).
#[cfg(target_arch = "x86_64")]
#[allow(deprecated, unused_unsafe)]
fn set_ftz() {
    unsafe {
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        _mm_setcsr(_mm_getcsr() | 0x8040); // FTZ (bit 15) + DAZ (bit 6)
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn set_ftz() {}

fn enable_ftz() {
    set_ftz();
}

// ----- FFT + spectral noise suppressor ------------------------------------

const NS_N: usize = 512; // FFT size
const NS_HOP: usize = 256; // 50 % overlap -> about 10 ms of delay at 48 kHz

struct Fft {
    rev: Vec<usize>,
    wr: Vec<f32>,
    wi: Vec<f32>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let bits = n.trailing_zeros();
        let rev: Vec<usize> = (0..n).map(|i| i.reverse_bits() >> (usize::BITS - bits)).collect();
        let wr: Vec<f32> = (0..n / 2).map(|k| (2.0 * PI * k as f32 / n as f32).cos()).collect();
        let wi: Vec<f32> = (0..n / 2).map(|k| -(2.0 * PI * k as f32 / n as f32).sin()).collect();
        Self { rev, wr, wi }
    }

    /// In-place radix-2 FFT. `inverse` also scales by 1/n.
    fn run(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.rev.len();
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut start = 0;
            while start < n {
                for k in 0..half {
                    let wr = self.wr[k * step];
                    let wi = if inverse { -self.wi[k * step] } else { self.wi[k * step] };
                    let a = start + k;
                    let b = a + half;
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
                start += len;
            }
            len <<= 1;
        }
        if inverse {
            let s = 1.0 / n as f32;
            for v in re.iter_mut() {
                *v *= s;
            }
            for v in im.iter_mut() {
                *v *= s;
            }
        }
    }
}

/// Spectral noise suppressor. It tracks the steady background noise in every
/// frequency band (fans, hiss, hum, room tone) and turns those bands down
/// whenever your voice is not louder than the noise there. It never mutes
/// completely: the gain floor stops the "cut out" effect of hard gates.
struct NoiseSuppressor {
    fft: Fft,
    win: Vec<f32>,
    ring: Vec<f32>,  // last NS_N input samples (circular)
    pos: usize,      // next write index in `ring`
    count: usize,    // samples since the last frame
    acc: Vec<f32>,   // overlap-add accumulator
    ready: Vec<f32>, // finished samples currently being played out
    re: Vec<f32>,
    im: Vec<f32>,
    ps: Vec<f32>,    // smoothed power per bin
    noise: Vec<f32>, // noise power estimate per bin
    graw: Vec<f32>,  // gain before smoothing
    gain: Vec<f32>,  // smoothed gain actually applied
    primed: bool,
}

impl NoiseSuppressor {
    fn new() -> Self {
        let half = NS_N / 2 + 1;
        Self {
            fft: Fft::new(NS_N),
            win: (0..NS_N).map(|i| (PI * (i as f32 + 0.5) / NS_N as f32).sin()).collect(),
            ring: vec![0.0; NS_N],
            pos: 0,
            count: 0,
            acc: vec![0.0; NS_N],
            ready: vec![0.0; NS_HOP],
            re: vec![0.0; NS_N],
            im: vec![0.0; NS_N],
            ps: vec![0.0; half],
            noise: vec![0.0; half],
            graw: vec![1.0; half],
            gain: vec![1.0; half],
            primed: false,
        }
    }

    fn reset(&mut self) {
        self.ring.fill(0.0);
        self.acc.fill(0.0);
        self.ready.fill(0.0);
        self.ps.fill(0.0);
        self.gain.fill(1.0);
        self.pos = 0;
        self.count = 0;
        self.primed = false;
    }

    fn process(&mut self, x: f32, alpha: f32, floor: f32) -> f32 {
        self.ring[self.pos] = x;
        self.pos = (self.pos + 1) % NS_N;
        let out = self.ready[self.count];
        self.count += 1;
        if self.count == NS_HOP {
            self.count = 0;
            self.frame(alpha, floor);
        }
        out
    }

    fn frame(&mut self, alpha: f32, floor: f32) {
        let half = NS_N / 2;
        for i in 0..NS_N {
            let idx = (self.pos + i) % NS_N; // oldest sample is at `pos`
            self.re[i] = self.ring[idx] * self.win[i];
            self.im[i] = 0.0;
        }
        self.fft.run(&mut self.re, &mut self.im, false);

        let mut total = 0.0f32;
        for k in 0..=half {
            let p = self.re[k] * self.re[k] + self.im[k] * self.im[k];
            self.ps[k] = 0.6 * self.ps[k] + 0.4 * p;
            total += self.ps[k];
            if !self.primed {
                self.noise[k] = self.ps[k];
            }
            // Follow the noise when the band looks like noise; creep upwards
            // slowly when it is much louder (speech), so speech is not learned.
            let calm = self.ps[k] < 2.5 * self.noise[k];
            self.noise[k] = if calm {
                self.noise[k] + (self.ps[k] - self.noise[k]) * 0.05
            } else {
                self.noise[k] * 1.0015
            };
            let xi = (self.ps[k] / self.noise[k].max(1e-12) - 1.0).max(0.0);
            self.graw[k] = (xi / (xi + alpha)).max(floor);
        }
        // After digital silence, learn the noise again from scratch.
        self.primed = total > 1e-5;

        for k in 0..=half {
            let l = self.graw[k.saturating_sub(1)];
            let r = self.graw[(k + 1).min(half)];
            let gs = 0.25 * l + 0.5 * self.graw[k] + 0.25 * r;
            let gp = self.gain[k];
            self.gain[k] = if gs > gp { gp + (gs - gp) * 0.7 } else { gp + (gs - gp) * 0.25 };
        }

        for k in 0..=half {
            let g = self.gain[k];
            self.re[k] *= g;
            self.im[k] *= g;
            if k > 0 && k < half {
                self.re[NS_N - k] *= g;
                self.im[NS_N - k] *= g;
            }
        }
        self.fft.run(&mut self.re, &mut self.im, true);

        for i in 0..NS_N {
            self.acc[i] += self.re[i] * self.win[i];
        }
        self.ready.copy_from_slice(&self.acc[..NS_HOP]);
        self.acc.copy_within(NS_HOP.., 0);
        for v in self.acc[NS_N - NS_HOP..].iter_mut() {
            *v = 0.0;
        }
    }
}

// ----- the whole chain ------------------------------------------------------

struct Voice {
    data: Arc<Vec<i16>>,
    pos: f64,
    step: f64,
    gain: f32,
    looping: bool,
}

const MAX_VOICES: usize = 16;
const RV_DAMP: f32 = 0.3;

struct Processor {
    params: Arc<Params>,
    rx: Receiver<Cmd>,
    sr: f32,
    st: Settings,
    st_dirty: bool,
    ftz_done: bool,

    // values derived from `st` (recomputed only when settings change)
    in_gain: f32,
    gate_thresh: f32,
    gate_atk: f32,
    gate_rel: f32,
    hpf_on: bool,
    lpf_on: bool,
    eq_on: bool,
    ns_on: bool,
    ns_alpha: f32,
    ns_floor: f32,
    comp_on: bool,
    comp_thresh: f32,
    comp_pow: f32,
    comp_makeup: f32,
    deess_on: bool,
    pitch_on: bool,
    pitch_step: f32,
    pwindow: f32,
    robot_inc: f32,
    whisper_on: bool,
    vib_on: bool,
    vib_inc: f32,
    vib_base: f32,
    vib_depth: f32,
    trem_on: bool,
    trem_inc: f32,
    chorus_on: bool,
    chorus_inc: f32,
    chorus_base: f32,
    chorus_depth: f32,
    chorus_fb: f32,
    echo_on: bool,
    echo_delay: usize,
    echo_coef: f32,
    reverb_on: bool,
    rv_fb: f32,
    drive_k: f32,
    levels: f32,    // bit-crusher levels, 0 = off
    hold_step: f32, // sample-rate reducer step, 0 = off

    // fixed coefficients
    env_rel: f32,
    comp_atk: f32,
    comp_rel: f32,
    de_a: f32,
    de_rel: f32,
    w_atk: f32,
    w_rel: f32,

    // filter / effect state
    ns: NoiseSuppressor,
    hpf: Biquad,
    lpf: Biquad,
    eq_low: Biquad,
    eq_mid: Biquad,
    eq_high: Biquad,
    env: f32,
    gate_g: f32,
    comp_env: f32,
    de_lo: f32,
    de_he: f32,
    de_le: f32,
    pbuf: Vec<f32>,
    pwrite: usize,
    pphase: f32,
    rphase: f32,
    w_env: f32,
    w_prev: f32,
    vbuf: Vec<f32>,
    vwrite: usize,
    vphase: f32,
    cbuf: Vec<f32>,
    cwrite: usize,
    cphase: f32,
    tphase: f32,
    ebuf: Vec<f32>,
    eidx: usize,
    edamp: f32,
    rv_comb: Vec<Vec<f32>>,
    rv_ci: [usize; 4],
    rv_cf: [f32; 4],
    rv_ap: Vec<Vec<f32>>,
    rv_ai: [usize; 2],
    hold_acc: f32,
    hold_val: f32,
    rng: u32,
    voices: Vec<Voice>,
}

impl Processor {
    fn new(params: Arc<Params>, sr: f32, rx: Receiver<Cmd>) -> Self {
        let tc = |secs: f32| (-1.0 / (secs * sr)).exp();
        let scaled = |l: usize| (((l as f32) * sr / 44100.0) as usize).max(1);
        Self {
            params,
            rx,
            sr,
            st: Settings::neutral(),
            st_dirty: true,
            ftz_done: false,
            in_gain: 1.0,
            gate_thresh: 0.0,
            gate_atk: 0.01,
            gate_rel: 0.001,
            hpf_on: false,
            lpf_on: false,
            eq_on: false,
            ns_on: false,
            ns_alpha: 1.0,
            ns_floor: 0.1,
            comp_on: false,
            comp_thresh: 0.1,
            comp_pow: 0.0,
            comp_makeup: 1.0,
            deess_on: false,
            pitch_on: false,
            pitch_step: 0.0,
            pwindow: sr * 0.05,
            robot_inc: 0.0,
            whisper_on: false,
            vib_on: false,
            vib_inc: 0.0,
            vib_base: 0.0,
            vib_depth: 0.0,
            trem_on: false,
            trem_inc: 0.0,
            chorus_on: false,
            chorus_inc: 0.0,
            chorus_base: 0.0,
            chorus_depth: 0.0,
            chorus_fb: 0.0,
            echo_on: false,
            echo_delay: 1,
            echo_coef: 1.0,
            reverb_on: false,
            rv_fb: 0.8,
            drive_k: 1.0,
            levels: 0.0,
            hold_step: 0.0,
            env_rel: tc(0.03),
            comp_atk: 1.0 - tc(0.005),
            comp_rel: 1.0 - tc(0.1),
            de_a: 1.0 - (-2.0 * PI * 4500.0 / sr).exp(),
            de_rel: tc(0.02),
            w_atk: 1.0 - tc(0.005),
            w_rel: 1.0 - tc(0.05),
            ns: NoiseSuppressor::new(),
            hpf: Biquad::default(),
            lpf: Biquad::default(),
            eq_low: Biquad::default(),
            eq_mid: Biquad::default(),
            eq_high: Biquad::default(),
            env: 0.0,
            gate_g: 1.0,
            comp_env: 0.0,
            de_lo: 0.0,
            de_he: 0.0,
            de_le: 0.0,
            pbuf: vec![0.0; (sr * 0.1) as usize + 8], // room for 100 ms grains
            pwrite: 0,
            pphase: 0.0,
            rphase: 0.0,
            w_env: 0.0,
            w_prev: 0.0,
            vbuf: vec![0.0; (sr * 0.03) as usize + 8],
            vwrite: 0,
            vphase: 0.0,
            cbuf: vec![0.0; (sr * 0.04) as usize + 8],
            cwrite: 0,
            cphase: 0.0,
            tphase: 0.0,
            ebuf: vec![0.0; sr as usize + 1], // up to 1 s of echo
            eidx: 0,
            edamp: 0.0,
            rv_comb: [1116, 1188, 1277, 1356].iter().map(|&l| vec![0.0; scaled(l)]).collect(),
            rv_ci: [0; 4],
            rv_cf: [0.0; 4],
            rv_ap: [556, 441].iter().map(|&l| vec![0.0; scaled(l)]).collect(),
            rv_ai: [0; 2],
            hold_acc: 0.0,
            hold_val: 0.0,
            rng: 0x1234_5678,
            voices: Vec::with_capacity(MAX_VOICES),
        }
    }

    /// Call once at the start of every audio buffer.
    fn begin_block(&mut self) {
        if !self.ftz_done {
            enable_ftz();
            self.ftz_done = true;
        }
        if let Ok(g) = self.params.settings.try_lock() {
            if *g != self.st {
                self.st = *g;
                self.st_dirty = true;
            }
        }
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Cmd::Play { data, gain, step, looping } => {
                    if self.voices.len() >= MAX_VOICES {
                        self.voices.remove(0);
                    }
                    self.voices.push(Voice { data, pos: 0.0, step, gain, looping });
                }
                Cmd::StopAll => self.voices.clear(),
            }
        }
        if self.st_dirty {
            self.st_dirty = false;
            self.update_derived();
        }
    }

    fn update_derived(&mut self) {
        let s = self.st;
        let sr = self.sr;
        let coef = |ms: f32| 1.0 - (-1.0 / (ms.max(0.05) * 0.001 * sr)).exp();

        self.in_gain = 10f32.powf(s.in_gain_db / 20.0);
        self.gate_thresh = if s.gate_db <= -79.0 { 0.0 } else { 10f32.powf(s.gate_db / 20.0) };
        self.gate_atk = coef(s.gate_atk_ms);
        self.gate_rel = coef(s.gate_rel_ms);

        self.hpf_on = s.hpf_hz > 25.0;
        if self.hpf_on {
            self.hpf.set(false, s.hpf_hz, sr);
        }
        self.lpf_on = s.lpf_hz < 19000.0f32.min(sr * 0.45);
        if self.lpf_on {
            self.lpf.set(true, s.lpf_hz, sr);
        }

        let ns_on = s.ns_amount > 0.01;
        if ns_on && !self.ns_on {
            self.ns.reset();
        }
        self.ns_on = ns_on;
        let amt = s.ns_amount.clamp(0.0, 1.0);
        self.ns_alpha = 1.0 + 4.0 * amt;
        self.ns_floor = 10f32.powf(-(8.0 + 22.0 * amt) / 20.0);

        self.eq_on = s.eq_low_db.abs() > 0.1 || s.eq_mid_db.abs() > 0.1 || s.eq_high_db.abs() > 0.1;
        if self.eq_on {
            self.eq_low.shelf(false, 120.0, s.eq_low_db, sr);
            self.eq_mid.peak(1000.0, s.eq_mid_db, 0.8, sr);
            self.eq_high.shelf(true, 6000.0, s.eq_high_db, sr);
        }

        self.comp_on = s.comp_ratio > 1.05;
        self.comp_thresh = 10f32.powf(s.comp_thresh_db / 20.0);
        self.comp_pow = 1.0 - 1.0 / s.comp_ratio.max(1.0);
        self.comp_makeup = 10f32.powf(s.comp_makeup_db / 20.0);
        self.deess_on = s.deess > 0.01;

        self.pitch_on = s.semitones.abs() > 0.05;
        self.pwindow = (sr * s.pitch_grain_ms.clamp(10.0, 100.0) * 0.001).min(self.pbuf.len() as f32 - 8.0);
        let ratio = 2f32.powf(s.semitones / 12.0);
        self.pitch_step = (1.0 - ratio) / self.pwindow;

        self.robot_inc = 2.0 * PI * s.robot_hz / sr;
        self.whisper_on = s.whisper > 0.01;

        let vib_on = s.vib_depth > 0.01;
        if vib_on && !self.vib_on {
            self.vbuf.fill(0.0);
        }
        self.vib_on = vib_on;
        self.vib_inc = 2.0 * PI * s.vib_hz / sr;
        self.vib_base = 0.005 * sr;
        self.vib_depth = s.vib_depth.clamp(0.0, 1.0) * 0.004 * sr;

        self.trem_on = s.trem_depth > 0.01;
        self.trem_inc = 2.0 * PI * s.trem_hz / sr;

        let chorus_on = s.chorus_mix > 0.01;
        if chorus_on && !self.chorus_on {
            self.cbuf.fill(0.0);
        }
        self.chorus_on = chorus_on;
        let flange = s.chorus_flange == 1;
        self.chorus_inc = 2.0 * PI * s.chorus_hz / sr;
        self.chorus_base = if flange { 0.003 * sr } else { 0.014 * sr };
        self.chorus_depth = s.chorus_depth.clamp(0.0, 1.0) * if flange { 0.0025 * sr } else { 0.006 * sr };
        self.chorus_fb = if flange { 0.55 } else { 0.0 };

        // Echo is skipped entirely while off; clear the tail when it switches on.
        let echo_on = s.echo_mix > 0.001;
        if echo_on && !self.echo_on {
            self.ebuf.fill(0.0);
            self.edamp = 0.0;
        }
        self.echo_on = echo_on;
        self.echo_delay = ((s.echo_ms * 0.001 * sr) as usize).clamp(1, self.ebuf.len() - 1);
        self.echo_coef = 1.0 - 0.9 * s.echo_damp.clamp(0.0, 1.0);

        let reverb_on = s.reverb_mix > 0.01;
        if reverb_on && !self.reverb_on {
            for b in self.rv_comb.iter_mut() {
                b.fill(0.0);
            }
            for b in self.rv_ap.iter_mut() {
                b.fill(0.0);
            }
            self.rv_cf = [0.0; 4];
        }
        self.reverb_on = reverb_on;
        self.rv_fb = 0.7 + 0.25 * s.reverb_size.clamp(0.0, 1.0);

        self.drive_k = 1.0 + s.drive * 19.0;
        self.levels = if s.bits < 15.5 { 2f32.powf(s.bits.round().max(2.0) - 1.0) } else { 0.0 };
        self.hold_step = if s.rate_hz < sr * 0.98 { sr / s.rate_hz.max(500.0) } else { 0.0 };
    }

    /// Call once at the end of every audio buffer.
    fn end_block(&mut self) {
        self.voices.retain(|v| v.looping || (v.pos as usize) < v.data.len());
    }

    fn noise(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn process(&mut self, input: f32) -> f32 {
        let mut x = input * self.in_gain;

        // 1. noise suppression (before everything else so effects don't boost noise)
        if self.ns_on {
            x = self.ns.process(x, self.ns_alpha, self.ns_floor);
        }

        // 2. noise gate
        if self.gate_thresh > 0.0 {
            let a = x.abs();
            self.env = if a > self.env { a } else { self.env * self.env_rel };
            let target = if self.env > self.gate_thresh { 1.0 } else { 0.0 };
            let c = if target > self.gate_g { self.gate_atk } else { self.gate_rel };
            self.gate_g += (target - self.gate_g) * c;
            x *= self.gate_g;
        }

        // 3. high-pass (rumble / low-end removal)
        if self.hpf_on {
            x = self.hpf.process(x);
        }

        // 4. EQ
        if self.eq_on {
            x = self.eq_low.process(x);
            x = self.eq_mid.process(x);
            x = self.eq_high.process(x);
        }

        // 5. de-esser (turns down the high band only when it dominates)
        if self.deess_on {
            self.de_lo += (x - self.de_lo) * self.de_a;
            let lo = self.de_lo;
            let hi = x - lo;
            self.de_he = hi.abs().max(self.de_he * self.de_rel);
            self.de_le = lo.abs().max(self.de_le * self.de_rel);
            let limit = 0.9 * self.de_le + 0.002;
            let g = if self.de_he > limit { limit / self.de_he } else { 1.0 };
            x = lo + hi * (1.0 - self.st.deess * (1.0 - g));
        }

        // 6. compressor
        if self.comp_on {
            let a = x.abs();
            let c = if a > self.comp_env { self.comp_atk } else { self.comp_rel };
            self.comp_env += (a - self.comp_env) * c;
            let g = if self.comp_env > self.comp_thresh {
                (self.comp_thresh / self.comp_env).powf(self.comp_pow)
            } else {
                1.0
            };
            x *= g * self.comp_makeup;
        }

        // 7. pitch shift (delay line is always fed so toggling is click-free)
        self.pbuf[self.pwrite] = x;
        self.pwrite = (self.pwrite + 1) % self.pbuf.len();
        if self.pitch_on {
            self.pphase = (self.pphase + self.pitch_step).rem_euclid(1.0);
            let pb = (self.pphase + 0.5).rem_euclid(1.0);
            // Hann crossfade: sin^2(pi*p) = (1 - cos(2*pi*p)) / 2, and the other tap is 1 - that.
            let wa = 0.5 - 0.5 * (2.0 * PI * self.pphase).cos();
            let wb = 1.0 - wa;
            x = read_frac(&self.pbuf, self.pwrite, self.pphase * self.pwindow) * wa
                + read_frac(&self.pbuf, self.pwrite, pb * self.pwindow) * wb;
        }

        // 8. vibrato (wobbling pitch)
        if self.vib_on {
            self.vbuf[self.vwrite] = x;
            self.vwrite = (self.vwrite + 1) % self.vbuf.len();
            self.vphase += self.vib_inc;
            if self.vphase >= 2.0 * PI {
                self.vphase -= 2.0 * PI;
            }
            let d = self.vib_base + self.vib_depth * self.vphase.sin();
            x = read_frac(&self.vbuf, self.vwrite, d);
        }

        // 9. robot (ring modulation, blended)
        if self.st.robot_amount > 0.001 {
            self.rphase += self.robot_inc;
            if self.rphase >= 2.0 * PI {
                self.rphase -= 2.0 * PI;
            }
            let ph = self.rphase * (0.5 / PI);
            let carrier = match self.st.robot_wave {
                0 => self.rphase.sin(),
                1 => {
                    if ph < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                2 => 4.0 * (ph - 0.5).abs() - 1.0,
                _ => 2.0 * ph - 1.0,
            };
            let a = self.st.robot_amount;
            x *= 1.0 - a + a * carrier;
        }

        // 10. whisper (noise that follows your volume)
        if self.whisper_on {
            let a = x.abs();
            let c = if a > self.w_env { self.w_atk } else { self.w_rel };
            self.w_env += (a - self.w_env) * c;
            let n = self.noise();
            let hn = n - self.w_prev; // first difference = hissier noise
            self.w_prev = n;
            let w = self.st.whisper;
            x = x * (1.0 - w) + hn * self.w_env * 1.2 * w;
        }

        // 11. chorus / flanger
        if self.chorus_on {
            self.cphase += self.chorus_inc;
            if self.cphase >= 2.0 * PI {
                self.cphase -= 2.0 * PI;
            }
            let (s, c) = self.cphase.sin_cos();
            let t1 = read_frac(&self.cbuf, self.cwrite, self.chorus_base + self.chorus_depth * s);
            let t2 = read_frac(&self.cbuf, self.cwrite, self.chorus_base + self.chorus_depth * c);
            let wet = 0.5 * (t1 + t2);
            self.cbuf[self.cwrite] = x + wet * self.chorus_fb;
            self.cwrite = (self.cwrite + 1) % self.cbuf.len();
            let m = self.st.chorus_mix;
            x = x * (1.0 - 0.5 * m) + wet * 0.7 * m;
        }

        // 12. tremolo
        if self.trem_on {
            self.tphase += self.trem_inc;
            if self.tphase >= 2.0 * PI {
                self.tphase -= 2.0 * PI;
            }
            x *= 1.0 - self.st.trem_depth * (0.5 - 0.5 * self.tphase.sin());
        }

        // 13. echo (skipped completely while the mix is 0)
        if self.echo_on {
            let elen = self.ebuf.len();
            let delayed = self.ebuf[(self.eidx + elen - self.echo_delay) % elen];
            self.edamp += (delayed - self.edamp) * self.echo_coef;
            self.ebuf[self.eidx] = x + self.edamp * self.st.echo_fb;
            self.eidx = (self.eidx + 1) % elen;
            x += self.edamp * self.st.echo_mix;
        }

        // 14. reverb (4 combs + 2 all-pass filters)
        if self.reverb_on {
            let inp = x * 0.12;
            let mut acc = 0.0f32;
            for k in 0..4 {
                let i = self.rv_ci[k];
                let o = self.rv_comb[k][i];
                self.rv_cf[k] = o * (1.0 - RV_DAMP) + self.rv_cf[k] * RV_DAMP;
                self.rv_comb[k][i] = inp + self.rv_cf[k] * self.rv_fb;
                self.rv_ci[k] = if i + 1 >= self.rv_comb[k].len() { 0 } else { i + 1 };
                acc += o;
            }
            for k in 0..2 {
                let i = self.rv_ai[k];
                let b = self.rv_ap[k][i];
                let o = b - acc;
                self.rv_ap[k][i] = acc + b * 0.5;
                self.rv_ai[k] = if i + 1 >= self.rv_ap[k].len() { 0 } else { i + 1 };
                acc = o;
            }
            x += acc * self.st.reverb_mix;
        }

        // 15. distortion
        if self.st.drive > 0.01 {
            let k = self.drive_k;
            x = match self.st.dist_type {
                1 => (x * k).clamp(-1.0, 1.0),
                2 => (x * k * 0.5 * PI).sin(),
                _ => (x * k).tanh() / k.tanh(),
            };
        }

        // 16. bit crusher
        if self.levels > 0.0 {
            x = (x.clamp(-1.0, 1.0) * self.levels).round() / self.levels;
        }

        // 17. sample-rate reducer (sample & hold)
        if self.hold_step > 0.0 {
            self.hold_acc += 1.0;
            if self.hold_acc >= self.hold_step {
                self.hold_acc -= self.hold_step;
                self.hold_val = x;
            }
            x = self.hold_val;
        }

        // 18. low-pass ("telephone" / muffled)
        if self.lpf_on {
            x = self.lpf.process(x);
        }

        // 19. hiss
        if self.st.hiss > 0.0 {
            x += self.noise() * self.st.hiss;
        }

        let voice = if self.st.muted { 0.0 } else { x * self.st.out_gain };

        // soundboard (16-bit samples) is mixed in after the effects
        let mut sb = 0.0f32;
        for v in self.voices.iter_mut() {
            let len = v.data.len();
            let i = v.pos as usize;
            if i < len {
                let a = v.data[i] as f32;
                let b = if i + 1 < len { v.data[i + 1] as f32 } else { a };
                let frac = (v.pos - i as f64) as f32;
                sb += (a + (b - a) * frac) * v.gain;
                v.pos += v.step;
                if v.looping && v.pos >= len as f64 {
                    v.pos -= len as f64;
                }
            }
        }

        (voice + sb * (self.st.sb_vol / 32768.0)).clamp(-1.0, 1.0)
    }
}
/// Decodes a file to mono 16-bit samples (half the memory of f32), optionally
/// resampling to `target_rate` so only one copy is ever kept.
fn decode_file(path: &Path, target_rate: Option<u32>) -> Result<(Vec<i16>, u32), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("unsupported file ({e})"))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no audio track found")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;

    let mut mono: Vec<i16> = Vec::new();
    let mut rate = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(_)) | Err(Error::ResetRequired) => break, // end of stream
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                rate = spec.rate;
                let ch = spec.channels.count().max(1);
                let mut sb = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                sb.copy_interleaved_ref(decoded);
                for frame in sb.samples().chunks(ch) {
                    let m = frame.iter().sum::<f32>() / ch as f32;
                    mono.push((m.clamp(-1.0, 1.0) * 32767.0) as i16);
                }
            }
            Err(Error::DecodeError(_)) => continue, // skip a corrupt frame
            Err(e) => return Err(e.to_string()),
        }
    }
    if mono.is_empty() || rate == 0 {
        return Err("file contains no audio".into());
    }
    mono.shrink_to_fit();
    if let Some(t) = target_rate {
        if t != rate {
            mono = resample_i16(&mono, rate, t);
            rate = t;
        }
    }
    Ok((mono, rate))
}

fn resample_i16(data: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || data.is_empty() {
        return data.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let n = (data.len() as f64 / ratio) as usize;
    let last = data.len() - 1;
    (0..n)
        .map(|i| {
            let p = i as f64 * ratio;
            let i0 = (p as usize).min(last);
            let frac = (p - i0 as f64) as f32;
            let a = data[i0] as f32;
            let b = data[(i0 + 1).min(last)] as f32;
            (a + (b - a) * frac).round() as i16
        })
        .collect()
}


// ---------------------------------------------------------------------------
// Sounds (a single 16-bit mono copy each, kept at the engine's sample rate)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq)]
enum Status {
    Loading,
    Ready,
    Failed(String),
}

struct Sound {
    id: usize,
    path: PathBuf,
    name: String,
    data: Arc<Vec<i16>>,
    rate: u32,
    volume: f32,
    speed: f32,
    looping: bool,
    status: Status,
}

impl Sound {
    fn duration(&self) -> f32 {
        self.data.len() as f32 / self.rate.max(1) as f32
    }

    /// Converts the stored copy to a new sample rate (only when it differs).
    fn convert_to(&mut self, rate: u32) {
        if rate != self.rate {
            self.data = Arc::new(resample_i16(&self.data, self.rate, rate));
            self.rate = rate;
        }
    }
}

type LoadMsg = (usize, Result<(Vec<i16>, u32), String>);

// ---------------------------------------------------------------------------
// Saved settings (so your sounds, devices and sliders survive a restart)
// ---------------------------------------------------------------------------

fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    match base {
        Some(b) => b.join("VoiceChanger").join("config.txt"),
        None => PathBuf::from("voice_changer_config.txt"),
    }
}

fn save_config(text: &str) -> std::io::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

// ---------------------------------------------------------------------------
// Audio engine (owns the cpal streams; dropping it stops audio)
// ---------------------------------------------------------------------------

struct Engine {
    _input: Stream,
    _output: Stream,
    _monitor: Option<Stream>,
    sample_rate: u32,
    tx: Sender<Cmd>,
    warning: Option<String>,
}

fn list_devices(input: bool) -> Vec<Device> {
    let host = cpal::default_host();
    if input {
        host.input_devices().map(|d| d.collect()).unwrap_or_default()
    } else {
        host.output_devices().map(|d| d.collect()).unwrap_or_default()
    }
}

fn device_names(input: bool) -> Vec<String> {
    list_devices(input).iter().filter_map(|d| d.name().ok()).collect()
}

fn find_device(name: &str, input: bool) -> Result<Device, String> {
    if name.is_empty() {
        let host = cpal::default_host();
        return if input { host.default_input_device() } else { host.default_output_device() }
            .ok_or_else(|| "no default audio device found".to_string());
    }
    list_devices(input)
        .into_iter()
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        .ok_or_else(|| format!("device '{name}' not found"))
}

fn build_output(
    device: &Device,
    sr: SampleRate,
    mut cons: HeapCons<f32>,
    params: Option<Arc<Params>>,
) -> Result<Stream, String> {
    let cfg = device.default_output_config().map_err(|e| e.to_string())?;
    if cfg.sample_format() != SampleFormat::F32 {
        return Err("only f32 audio devices are supported".into());
    }
    let ch = cfg.channels() as usize;
    let scfg = StreamConfig { channels: cfg.channels(), sample_rate: sr, buffer_size: BufferSize::Default };
    let stream = device
        .build_output_stream(
            &scfg,
            move |data: &mut [f32], _| {
                let mut peak = 0.0f32;
                for frame in data.chunks_mut(ch) {
                    let s = cons.try_pop().unwrap_or(0.0);
                    peak = peak.max(s.abs());
                    frame.iter_mut().for_each(|o| *o = s);
                }
                if let Some(p) = &params {
                    p.out_peak.fetch_max(peak.to_bits(), Relaxed);
                }
            },
            |e| eprintln!("output error: {e}"),
            None,
        )
        .map_err(|e| format!("{e} (sample rate {} Hz may be unsupported by this device)", sr.0))?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

fn prefilled_ring(sample_rate: u32) -> (HeapProd<f32>, HeapCons<f32>) {
    let rb = HeapRb::<f32>::new(sample_rate as usize / 2);
    let (mut prod, cons) = rb.split();
    for _ in 0..(sample_rate as usize / 50) {
        let _ = prod.try_push(0.0); // ~20 ms head start against underruns
    }
    (prod, cons)
}

impl Engine {
    fn start(in_name: &str, out_name: &str, mon_name: &str, params: Arc<Params>) -> Result<Engine, String> {
        let input = find_device(in_name, true)?;
        let output = find_device(out_name, false)?;

        let in_cfg = input.default_input_config().map_err(|e| e.to_string())?;
        if in_cfg.sample_format() != SampleFormat::F32 {
            return Err("only f32 audio devices are supported".into());
        }
        let sample_rate = in_cfg.sample_rate();
        let in_ch = in_cfg.channels() as usize;
        let in_stream_cfg: StreamConfig = in_cfg.into();

        // main output (e.g. a virtual cable that Discord listens to)
        let (prod, cons) = prefilled_ring(sample_rate.0);
        let output_stream = build_output(&output, sample_rate, cons, Some(params.clone()))
            .map_err(|e| format!("output: {e}"))?;

        // optional monitor output (your headphones) -- failures are non-fatal
        let mut warning = None;
        let mut mon_prod: Option<HeapProd<f32>> = None;
        let mut monitor_stream = None;
        if !mon_name.is_empty() {
            let (mp, mc) = prefilled_ring(sample_rate.0);
            let res = find_device(mon_name, false).and_then(|d| build_output(&d, sample_rate, mc, None));
            match res {
                Ok(s) => {
                    monitor_stream = Some(s);
                    mon_prod = Some(mp);
                }
                Err(e) => warning = Some(format!("Monitor unavailable: {e}")),
            }
        }

        let (tx, rx) = mpsc::channel();
        let mut proc = Processor::new(params.clone(), sample_rate.0 as f32, rx);
        let mut prod = prod;
        let p_in = params;
        let input_stream = input
            .build_input_stream(
                &in_stream_cfg,
                move |data: &[f32], _| {
                    proc.begin_block();
                    let mon_gain = proc.st.mon_vol;
                    let mut peak = 0.0f32;
                    for frame in data.chunks(in_ch) {
                        peak = peak.max(frame[0].abs());
                        let y = proc.process(frame[0]);
                        let _ = prod.try_push(y);
                        if let Some(m) = mon_prod.as_mut() {
                            let _ = m.try_push(y * mon_gain);
                        }
                    }
                    proc.end_block();
                    p_in.in_peak.fetch_max(peak.to_bits(), Relaxed);
                },
                |e| eprintln!("input error: {e}"),
                None,
            )
            .map_err(|e| format!("input: {e}"))?;
        input_stream.play().map_err(|e| e.to_string())?;

        Ok(Engine {
            _input: input_stream,
            _output: output_stream,
            _monitor: monitor_stream,
            sample_rate: sample_rate.0,
            tx,
            warning,
        })
    }
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Quick,
    Custom,
    Soundboard,
}

const Q_NORMAL: u8 = 0;
const Q_PITCH: u8 = 1;
const Q_ROBOT: u8 = 2;
const Q_ECHO: u8 = 3;

/// A slider that resets to `def` when double-clicked. `step` <= 0 means smooth.
fn sl(
    ui: &mut egui::Ui,
    v: &mut f32,
    def: f32,
    range: std::ops::RangeInclusive<f32>,
    text: &str,
    suffix: &str,
    log: bool,
    step: f64,
) {
    let mut s = egui::Slider::new(v, range).text(text).logarithmic(log);
    if !suffix.is_empty() {
        s = s.suffix(suffix);
    }
    if step > 0.0 {
        s = s.step_by(step);
    }
    if ui.add(s).double_clicked() {
        *v = def;
    }
}

fn combo_u8(ui: &mut egui::Ui, id: &str, label: &str, v: &mut u8, items: &[&str]) {
    ui.horizontal(|ui| {
        ui.label(label);
        let shown = items[(*v as usize).min(items.len() - 1)];
        egui::ComboBox::from_id_source(id).selected_text(shown).show_ui(ui, |ui| {
            for (i, item) in items.iter().enumerate() {
                ui.selectable_value(v, i as u8, *item);
            }
        });
    });
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).weak().small());
}

struct App {
    ctx: egui::Context,
    params: Arc<Params>,
    engine: Option<Engine>,
    error: Option<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    in_sel: String,  // "" = system default
    out_sel: String, // "" = system default
    mon_sel: String, // "" = off

    tab: Tab,
    voice_tab: Tab, // which tab (Quick or Custom) currently drives the voice

    // quick tab
    q_mode: u8,
    q_semis: f32,
    q_robot_hz: f32,
    q_echo_mix: f32,
    q_echo_fb: f32,

    // custom tab (full chain) and saved presets
    custom: Settings,
    presets: Vec<(String, String)>,
    preset_name: String,

    // common
    ns_amount: f32,
    out_gain: f32,
    muted: bool,
    sb_vol: f32,
    mon_vol: f32,
    last_sent: Settings,
    in_level: f32,
    out_level: f32,

    // soundboard
    sounds: Vec<Sound>,
    next_id: usize,
    load_tx: Sender<LoadMsg>,
    load_rx: Receiver<LoadMsg>,

    // saved settings
    last_saved: String,
    last_check: Instant,
}

impl App {
    fn new(ctx: egui::Context) -> Self {
        let (load_tx, load_rx) = mpsc::channel();
        let mut app = Self {
            ctx,
            params: Arc::new(Params::new()),
            engine: None,
            error: None,
            inputs: device_names(true),
            outputs: device_names(false),
            in_sel: String::new(),
            out_sel: String::new(),
            mon_sel: String::new(),
            tab: Tab::Quick,
            voice_tab: Tab::Quick,
            q_mode: Q_PITCH,
            q_semis: 4.0,
            q_robot_hz: 50.0,
            q_echo_mix: 0.5,
            q_echo_fb: 0.45,
            custom: Settings::neutral(),
            presets: Vec::new(),
            preset_name: String::new(),
            ns_amount: 0.0,
            out_gain: 1.0,
            muted: false,
            sb_vol: 1.0,
            mon_vol: 1.0,
            last_sent: Settings::neutral(),
            in_level: 0.0,
            out_level: 0.0,
            sounds: Vec::new(),
            next_id: 0,
            load_tx,
            load_rx,
            last_saved: String::new(),
            last_check: Instant::now(),
        };

        let mut specs = Vec::new();
        if let Ok(text) = std::fs::read_to_string(config_path()) {
            specs = app.load_config(&text);
        }

        app.restart_engine();
        // A saved device may have been unplugged: fall back to the defaults.
        if app.error.is_some() && !(app.in_sel.is_empty() && app.out_sel.is_empty() && app.mon_sel.is_empty()) {
            app.in_sel.clear();
            app.out_sel.clear();
            app.mon_sel.clear();
            app.restart_engine();
        }
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
    }

    // ----- saving / loading -----

    fn config_text(&self) -> String {
        let mut t = String::from("version=1\n");
        t.push_str(&format!("in_sel={}\n", self.in_sel));
        t.push_str(&format!("out_sel={}\n", self.out_sel));
        t.push_str(&format!("mon_sel={}\n", self.mon_sel));
        t.push_str(&format!("ns_amount={}\n", self.ns_amount));
        t.push_str(&format!("out_gain={}\n", self.out_gain));
        t.push_str(&format!("muted={}\n", if self.muted { 1 } else { 0 }));
        t.push_str(&format!("sb_vol={}\n", self.sb_vol));
        t.push_str(&format!("mon_vol={}\n", self.mon_vol));
        t.push_str(&format!("voice_tab={}\n", if self.voice_tab == Tab::Custom { 1 } else { 0 }));
        t.push_str(&format!("q_mode={}\n", self.q_mode));
        t.push_str(&format!("q_semis={}\n", self.q_semis));
        t.push_str(&format!("q_robot_hz={}\n", self.q_robot_hz));
        t.push_str(&format!("q_echo_mix={}\n", self.q_echo_mix));
        t.push_str(&format!("q_echo_fb={}\n", self.q_echo_fb));
        t.push_str(&format!("custom={}\n", self.custom.serialize()));
        for (name, data) in &self.presets {
            t.push_str(&format!("preset={}\t{}\n", name, data));
        }
        for s in &self.sounds {
            t.push_str(&format!(
                "sound={}\t{}\t{}\t{}\n",
                s.path.to_string_lossy(),
                s.volume,
                if s.looping { 1 } else { 0 },
                s.speed
            ));
        }
        t
    }

    /// Reads a saved config; returns the sounds to (re)load.
    fn load_config(&mut self, text: &str) -> Vec<(PathBuf, f32, bool, f32)> {
        let num = |v: &str| v.trim().parse::<f32>().ok().filter(|n| n.is_finite());
        let mut sounds = Vec::new();
        for line in text.lines() {
            let Some((key, val)) = line.split_once('=') else { continue };
            match key {
                "in_sel" => self.in_sel = val.to_string(),
                "out_sel" => self.out_sel = val.to_string(),
                "mon_sel" => self.mon_sel = val.to_string(),
                "ns_amount" => {
                    if let Some(n) = num(val) {
                        self.ns_amount = n.clamp(0.0, 1.0);
                    }
                }
                "out_gain" => {
                    if let Some(n) = num(val) {
                        self.out_gain = n.clamp(0.0, 3.0);
                    }
                }
                "muted" => self.muted = val.trim() == "1",
                "sb_vol" => {
                    if let Some(n) = num(val) {
                        self.sb_vol = n.clamp(0.0, 2.0);
                    }
                }
                "mon_vol" => {
                    if let Some(n) = num(val) {
                        self.mon_vol = n.clamp(0.0, 2.0);
                    }
                }
                "voice_tab" => {
                    let custom = val.trim() == "1";
                    self.voice_tab = if custom { Tab::Custom } else { Tab::Quick };
                    self.tab = self.voice_tab;
                }
                "q_mode" => {
                    if let Ok(n) = val.trim().parse::<u8>() {
                        self.q_mode = n.min(Q_ECHO);
                    }
                }
                "q_semis" => {
                    if let Some(n) = num(val) {
                        self.q_semis = n.clamp(-12.0, 12.0);
                    }
                }
                "q_robot_hz" => {
                    if let Some(n) = num(val) {
                        self.q_robot_hz = n.clamp(10.0, 200.0);
                    }
                }
                "q_echo_mix" => {
                    if let Some(n) = num(val) {
                        self.q_echo_mix = n.clamp(0.0, 1.0);
                    }
                }
                "q_echo_fb" => {
                    if let Some(n) = num(val) {
                        self.q_echo_fb = n.clamp(0.0, 0.9);
                    }
                }
                "custom" => {
                    let mut s = Settings::neutral();
                    s.apply(val);
                    self.custom = s;
                }
                "preset" => {
                    if let Some((name, data)) = val.split_once('\t') {
                        self.presets.push((name.to_string(), data.to_string()));
                    }
                }
                "sound" => {
                    let mut it = val.split('\t');
                    let path = it.next().unwrap_or("");
                    if path.is_empty() {
                        continue;
                    }
                    let volume = it.next().and_then(num).unwrap_or(1.0).clamp(0.0, 2.0);
                    let looping = it.next().map(|v| v.trim() == "1").unwrap_or(false);
                    let speed = it.next().and_then(num).unwrap_or(1.0).clamp(0.25, 4.0);
                    sounds.push((PathBuf::from(path), volume, looping, speed));
                }
                _ => {}
            }
        }
        sounds
    }

    fn quick_settings(&self) -> Settings {
        let mut s = Settings::neutral();
        match self.q_mode {
            Q_PITCH => s.semitones = self.q_semis,
            Q_ROBOT => {
                s.robot_amount = 1.0;
                s.robot_hz = self.q_robot_hz;
            }
            Q_ECHO => {
                s.echo_mix = self.q_echo_mix;
                s.echo_fb = self.q_echo_fb;
            }
            _ => {}
        }
        s
    }

    // ----- soundboard -----

    fn spawn_decode(&self, jobs: Vec<(usize, PathBuf)>) {
        if jobs.is_empty() {
            return;
        }
        let target = self.engine.as_ref().map(|e| e.sample_rate);
        let tx = self.load_tx.clone();
        let ctx = self.ctx.clone();
        // One background thread decodes the files one after another, so many
        // sounds never hog every CPU core at once.
        std::thread::spawn(move || {
            for (id, path) in jobs {
                let res = decode_file(&path, target).map_err(|e| e);
                let _ = tx.send((id, res));
                ctx.request_repaint();
            }
        });
    }

    fn add_sounds(&mut self, specs: Vec<(PathBuf, f32, bool, f32)>) {
        let rate = self.engine.as_ref().map(|e| e.sample_rate).unwrap_or(48000);
        let mut jobs = Vec::new();
        for (path, volume, looping, speed) in specs {
            let id = self.next_id;
            self.next_id += 1;
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "sound".into());
            self.sounds.push(Sound {
                id,
                path: path.clone(),
                name,
                data: Arc::new(Vec::new()),
                rate,
                volume,
                speed,
                looping,
                status: Status::Loading,
            });
            jobs.push((id, path));
        }
        self.spawn_decode(jobs);
    }

    fn play_sound(&mut self, i: usize) {
        if let Some(e) = &self.engine {
            let snd = &mut self.sounds[i];
            if snd.status != Status::Ready || snd.data.is_empty() {
                return;
            }
            snd.convert_to(e.sample_rate);
            let _ = e.tx.send(Cmd::Play {
                data: snd.data.clone(),
                gain: snd.volume,
                step: snd.speed as f64,
                looping: snd.looping,
            });
        }
    }

    fn stop_sounds(&mut self) {
        if let Some(e) = &self.engine {
            let _ = e.tx.send(Cmd::StopAll);
        }
    }

    // ----- tabs -----

    fn ui_quick(&mut self, ui: &mut egui::Ui) {
        ui.label("Pick one effect. For combining several, use the Custom tab.");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.q_mode, Q_NORMAL, "Normal");
            ui.selectable_value(&mut self.q_mode, Q_PITCH, "Pitch");
            ui.selectable_value(&mut self.q_mode, Q_ROBOT, "Robot");
            ui.selectable_value(&mut self.q_mode, Q_ECHO, "Echo");
        });
        ui.add_space(4.0);
        match self.q_mode {
            Q_PITCH => {
                sl(ui, &mut self.q_semis, 0.0, -12.0..=12.0, "semitones", "", false, 0.5);
                ui.horizontal(|ui| {
                    if ui.button("Deep (-6)").clicked() {
                        self.q_semis = -6.0;
                    }
                    if ui.button("Normal (0)").clicked() {
                        self.q_semis = 0.0;
                    }
                    if ui.button("Chipmunk (+7)").clicked() {
                        self.q_semis = 7.0;
                    }
                });
            }
            Q_ROBOT => {
                sl(ui, &mut self.q_robot_hz, 50.0, 10.0..=200.0, "mod freq (Hz)", "", false, 0.0);
            }
            Q_ECHO => {
                sl(ui, &mut self.q_echo_mix, 0.5, 0.0..=1.0, "mix", "", false, 0.0);
                sl(ui, &mut self.q_echo_fb, 0.45, 0.0..=0.9, "feedback", "", false, 0.0);
            }
            _ => {
                ui.label(egui::RichText::new("Your voice passes through unchanged.").weak());
            }
        }
    }

    fn ui_custom(&mut self, ui: &mut egui::Ui) {
        ui.label("Every effect runs at the same time. Each slider's rest position means \"off\". Double-click a slider to reset it.");
        ui.horizontal_wrapped(|ui| {
            ui.label("Presets:");
            let c = Settings::neutral();
            if ui.button("Reset").clicked() {
                self.custom = c;
            }
            if ui.button("Clean").clicked() {
                self.custom = Settings { in_gain_db: 3.0, gate_db: -55.0, hpf_hz: 90.0, ..c };
            }
            if ui.button("Studio").clicked() {
                self.custom = Settings {
                    hpf_hz: 80.0,
                    eq_low_db: 2.0,
                    eq_high_db: 2.5,
                    comp_thresh_db: -20.0,
                    comp_ratio: 3.0,
                    comp_makeup_db: 4.0,
                    deess: 0.4,
                    ..c
                };
            }
            if ui.button("Cheap mic").clicked() {
                self.custom = Settings {
                    in_gain_db: 6.0,
                    hpf_hz: 250.0,
                    lpf_hz: 3800.0,
                    bits: 9.0,
                    rate_hz: 14000.0,
                    drive: 0.12,
                    hiss: 0.02,
                    ..c
                };
            }
            if ui.button("Telephone").clicked() {
                self.custom = Settings { hpf_hz: 300.0, lpf_hz: 3400.0, bits: 12.0, rate_hz: 8000.0, ..c };
            }
            if ui.button("Demon").clicked() {
                self.custom = Settings {
                    semitones: -7.0,
                    drive: 0.2,
                    echo_mix: 0.25,
                    echo_ms: 120.0,
                    lpf_hz: 6000.0,
                    ..c
                };
            }
            if ui.button("Chipmunk").clicked() {
                self.custom = Settings { semitones: 8.0, ..c };
            }
            if ui.button("Cathedral").clicked() {
                self.custom = Settings { reverb_mix: 0.5, reverb_size: 0.9, ..c };
            }
            if ui.button("Ghost").clicked() {
                self.custom = Settings {
                    whisper: 0.5,
                    reverb_mix: 0.4,
                    reverb_size: 0.8,
                    vib_depth: 0.2,
                    ..c
                };
            }
        });

        // user presets
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.preset_name).hint_text("preset name").desired_width(160.0));
            if ui.button("Save current as preset").clicked() {
                let name: String = self
                    .preset_name
                    .chars()
                    .filter(|c| !c.is_control() && *c != '=' && *c != ';')
                    .collect::<String>()
                    .trim()
                    .to_string();
                if !name.is_empty() {
                    let data = self.custom.serialize();
                    if let Some(p) = self.presets.iter_mut().find(|p| p.0 == name) {
                        p.1 = data;
                    } else {
                        self.presets.push((name, data));
                    }
                }
            }
        });
        let mut load: Option<usize> = None;
        let mut del: Option<usize> = None;
        if !self.presets.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label("My presets:");
                for (i, p) in self.presets.iter().enumerate() {
                    if ui.button(p.0.as_str()).clicked() {
                        load = Some(i);
                    }
                    if ui.small_button("x").on_hover_text("Delete this preset").clicked() {
                        del = Some(i);
                    }
                }
            });
        }
        if let Some(i) = load {
            let mut s = Settings::neutral();
            s.apply(&self.presets[i].1);
            self.custom = s;
        }
        if let Some(i) = del {
            self.presets.remove(i);
        }
        ui.add_space(6.0);

        let s = &mut self.custom;
        egui::CollapsingHeader::new("Input & cleanup").default_open(true).show(ui, |ui| {
            sl(ui, &mut s.in_gain_db, 0.0, -20.0..=30.0, "input gain", " dB", false, 0.0);
            sl(ui, &mut s.gate_db, -80.0, -80.0..=-20.0, "noise gate", " dB", false, 0.0);
            sl(ui, &mut s.gate_atk_ms, 2.0, 0.5..=50.0, "gate attack", " ms", true, 0.0);
            sl(ui, &mut s.gate_rel_ms, 80.0, 10.0..=500.0, "gate release", " ms", true, 0.0);
            sl(ui, &mut s.hpf_hz, 20.0, 20.0..=600.0, "high-pass", " Hz", true, 0.0);
            hint(ui, "Gate: -80 = off. High-pass: 20 = off. Slower release = smoother word endings.");
        });
        egui::CollapsingHeader::new("Tone & dynamics").show(ui, |ui| {
            sl(ui, &mut s.eq_low_db, 0.0, -12.0..=12.0, "bass (120 Hz)", " dB", false, 0.0);
            sl(ui, &mut s.eq_mid_db, 0.0, -12.0..=12.0, "mid (1 kHz)", " dB", false, 0.0);
            sl(ui, &mut s.eq_high_db, 0.0, -12.0..=12.0, "treble (6 kHz)", " dB", false, 0.0);
            ui.separator();
            sl(ui, &mut s.comp_ratio, 1.0, 1.0..=20.0, "compressor ratio", ":1", true, 0.0);
            sl(ui, &mut s.comp_thresh_db, -18.0, -50.0..=0.0, "compressor threshold", " dB", false, 0.0);
            sl(ui, &mut s.comp_makeup_db, 0.0, 0.0..=24.0, "makeup gain", " dB", false, 0.0);
            hint(ui, "Compressor ratio 1 = off. It evens out loud and quiet parts of your voice.");
            ui.separator();
            sl(ui, &mut s.deess, 0.0, 0.0..=1.0, "de-esser", "", false, 0.0);
            hint(ui, "Softens harsh \"s\" sounds (useful after boosting treble).");
        });
        egui::CollapsingHeader::new("Voice").default_open(true).show(ui, |ui| {
            sl(ui, &mut s.semitones, 0.0, -24.0..=24.0, "pitch", " st", false, 0.5);
            sl(ui, &mut s.pitch_grain_ms, 50.0, 20.0..=100.0, "pitch smoothness", " ms", false, 0.0);
            hint(ui, "Smoothness: lower = tighter but more robotic, higher = smoother but a bit more echoey.");
            ui.separator();
            sl(ui, &mut s.robot_amount, 0.0, 0.0..=1.0, "robot amount", "", false, 0.0);
            sl(ui, &mut s.robot_hz, 50.0, 10.0..=200.0, "robot freq", " Hz", false, 0.0);
            combo_u8(ui, "robot_wave", "robot wave", &mut s.robot_wave, &["Sine", "Square", "Triangle", "Saw"]);
            ui.separator();
            sl(ui, &mut s.whisper, 0.0, 0.0..=1.0, "whisper", "", false, 0.0);
        });
        egui::CollapsingHeader::new("Modulation (vibrato, tremolo, chorus)").show(ui, |ui| {
            sl(ui, &mut s.vib_depth, 0.0, 0.0..=1.0, "vibrato depth", "", false, 0.0);
            sl(ui, &mut s.vib_hz, 5.0, 0.5..=12.0, "vibrato speed", " Hz", false, 0.0);
            ui.separator();
            sl(ui, &mut s.trem_depth, 0.0, 0.0..=1.0, "tremolo depth", "", false, 0.0);
            sl(ui, &mut s.trem_hz, 5.0, 0.5..=20.0, "tremolo speed", " Hz", false, 0.0);
            ui.separator();
            sl(ui, &mut s.chorus_mix, 0.0, 0.0..=1.0, "chorus / flanger mix", "", false, 0.0);
            sl(ui, &mut s.chorus_hz, 0.8, 0.1..=5.0, "sweep speed", " Hz", false, 0.0);
            sl(ui, &mut s.chorus_depth, 0.5, 0.0..=1.0, "sweep depth", "", false, 0.0);
            combo_u8(ui, "chorus_type", "type", &mut s.chorus_flange, &["Chorus", "Flanger"]);
        });
        egui::CollapsingHeader::new("Echo & reverb").show(ui, |ui| {
            sl(ui, &mut s.echo_mix, 0.0, 0.0..=1.0, "echo mix", "", false, 0.0);
            sl(ui, &mut s.echo_fb, 0.45, 0.0..=0.9, "echo feedback", "", false, 0.0);
            sl(ui, &mut s.echo_ms, 300.0, 50.0..=800.0, "echo time", " ms", false, 0.0);
            sl(ui, &mut s.echo_damp, 0.0, 0.0..=1.0, "echo darkness", "", false, 0.0);
            ui.separator();
            sl(ui, &mut s.reverb_mix, 0.0, 0.0..=1.0, "reverb mix", "", false, 0.0);
            sl(ui, &mut s.reverb_size, 0.5, 0.0..=1.0, "reverb size", "", false, 0.0);
        });
        egui::CollapsingHeader::new("Lo-fi (cheap mic)").show(ui, |ui| {
            sl(ui, &mut s.drive, 0.0, 0.0..=1.0, "distortion", "", false, 0.0);
            combo_u8(ui, "dist_type", "distortion type", &mut s.dist_type, &["Soft", "Hard clip", "Fold"]);
            sl(ui, &mut s.bits, 16.0, 4.0..=16.0, "bit depth", "", false, 1.0);
            sl(ui, &mut s.rate_hz, 48000.0, 2000.0..=48000.0, "sample rate", " Hz", true, 0.0);
            sl(ui, &mut s.lpf_hz, 20000.0, 1000.0..=20000.0, "low-pass", " Hz", true, 0.0);
            sl(ui, &mut s.hiss, 0.0, 0.0..=0.1, "hiss / noise", "", false, 0.0);
            hint(ui, "Bit depth 16, sample rate 48000 and low-pass 20000 = off.");
        });
    }

    fn ui_soundboard(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Add sound files...").clicked() {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Audio", &["mp3", "wav", "ogg", "flac"])
                    .pick_files()
                {
                    let specs: Vec<(PathBuf, f32, bool, f32)> =
                        paths.into_iter().map(|p| (p, 1.0, false, 1.0)).collect();
                    self.add_sounds(specs);
                }
            }
            if ui.button("Stop all").clicked() {
                self.stop_sounds();
            }
            let loading = self.sounds.iter().filter(|s| s.status == Status::Loading).count();
            if loading > 0 {
                ui.label(egui::RichText::new(format!("Loading {loading}...")).weak());
            }
        });
        sl(ui, &mut self.sb_vol, 1.0, 0.0..=2.0, "soundboard volume", "", false, 0.0);
        hint(ui, "Your sound list is saved automatically and comes back next time. Keep the files in the same folder.");
        ui.separator();

        if self.sounds.is_empty() {
            ui.label(egui::RichText::new("No sounds yet. Add mp3, wav, ogg or flac files.").weak());
        }
        let mut remove: Option<usize> = None;
        let mut retry: Option<usize> = None;
        for i in 0..self.sounds.len() {
            let name = self.sounds[i].name.clone();
            let status = self.sounds[i].status.clone();
            let dur = self.sounds[i].duration();
            ui.horizontal(|ui| {
                match &status {
                    Status::Ready => {
                        if ui.button("Play").clicked() {
                            self.play_sound(i);
                        }
                    }
                    Status::Loading => {
                        ui.add_enabled(false, egui::Button::new("Play"));
                    }
                    Status::Failed(_) => {
                        if ui.button("Retry").clicked() {
                            retry = Some(i);
                        }
                    }
                }
                if ui.small_button("X").on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
                match &status {
                    Status::Ready => {
                        ui.label(format!("{name}  ({dur:.1}s)"));
                    }
                    Status::Loading => {
                        ui.label(egui::RichText::new(format!("{name}  (loading...)")).weak());
                    }
                    Status::Failed(e) => {
                        ui.colored_label(egui::Color32::LIGHT_RED, format!("{name}: {e}"));
                    }
                }
            });
            if status == Status::Ready {
                ui.horizontal_wrapped(|ui| {
                    let snd = &mut self.sounds[i];
                    sl(ui, &mut snd.volume, 1.0, 0.0..=2.0, "volume", "", false, 0.0);
                    sl(ui, &mut snd.speed, 1.0, 0.5..=2.0, "speed", "x", false, 0.0);
                    ui.checkbox(&mut snd.looping, "loop");
                });
            }
        }
        if let Some(i) = retry {
            self.sounds[i].status = Status::Loading;
            let job = (self.sounds[i].id, self.sounds[i].path.clone());
            self.spawn_decode(vec![job]);
        }
        if let Some(i) = remove {
            self.sounds.remove(i);
        }
    }
}

fn device_combo(ui: &mut egui::Ui, id: &str, names: &[String], sel: &mut String, empty_label: &str) -> bool {
    let before = sel.clone();
    let text = if sel.is_empty() { empty_label.to_string() } else { sel.clone() };
    egui::ComboBox::from_id_source(id)
        .width(300.0)
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(sel, String::new(), empty_label);
            for n in names {
                ui.selectable_value(sel, n.clone(), n);
            }
        });
    *sel != before
}

fn meter(ui: &mut egui::Ui, label: &str, level: f32) {
    ui.horizontal(|ui| {
        ui.label(format!("{label:<4}"));
        ui.add(egui::ProgressBar::new(level.sqrt().min(1.0)).desired_width(320.0));
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // finished sound decodes
        while let Ok((id, res)) = self.load_rx.try_recv() {
            let rate = self.engine.as_ref().map(|e| e.sample_rate);
            if let Some(snd) = self.sounds.iter_mut().find(|s| s.id == id) {
                match res {
                    Ok((data, r)) => {
                        snd.data = Arc::new(data);
                        snd.rate = r;
                        snd.status = Status::Ready;
                        if let Some(rate) = rate {
                            snd.convert_to(rate); // safety net if the rate changed while loading
                        }
                    }
                    Err(e) => snd.status = Status::Failed(e),
                }
            }
        }

        let mut restart = false;

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                sl(ui, &mut self.out_gain, 1.0, 0.0..=3.0, "mic output gain", "", false, 0.0);
                ui.checkbox(&mut self.muted, "Mute mic");
            });
            meter(ui, "In", self.in_level);
            meter(ui, "Out", self.out_level);
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("Error: {err}"));
                if ui.button("Retry").clicked() {
                    restart = true;
                }
            } else if let Some(e) = &self.engine {
                let voice = if self.voice_tab == Tab::Custom { "Custom" } else { "Quick" };
                ui.label(
                    egui::RichText::new(format!("Running at {} Hz  |  voice: {voice} tab", e.sample_rate)).weak(),
                );
                if let Some(w) = &e.warning {
                    ui.colored_label(egui::Color32::YELLOW, w);
                }
            }
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Voice Changer");
                ui.label(egui::RichText::new("Use headphones to avoid feedback.").weak());

                egui::CollapsingHeader::new("Devices").default_open(true).show(ui, |ui| {
                    egui::Grid::new("devices").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Microphone");
                        restart |= device_combo(ui, "in", &self.inputs, &mut self.in_sel, "System default");
                        ui.end_row();
                        ui.label("Output");
                        restart |= device_combo(ui, "out", &self.outputs, &mut self.out_sel, "System default");
                        ui.end_row();
                        ui.label("Monitor (you hear)");
                        restart |= device_combo(ui, "mon", &self.outputs, &mut self.mon_sel, "Off");
                        ui.end_row();
                        ui.label("Monitor volume");
                        sl(ui, &mut self.mon_vol, 1.0, 0.0..=2.0, "", "", false, 0.0);
                        ui.end_row();
                    });
                    if ui.small_button("Refresh devices").clicked() {
                        self.inputs = device_names(true);
                        self.outputs = device_names(false);
                    }
                });
                egui::CollapsingHeader::new("Using it in Discord / calls").show(ui, |ui| {
                    ui.label(
                        "Apps like Discord only hear a microphone device, so install a free virtual \
                         audio cable (VB-CABLE, vb-audio.com), then:\n\
                         1. Here: set Output to \"CABLE Input (VB-Audio Virtual Cable)\".\n\
                         2. Here: set Monitor to your headphones so you can hear yourself.\n\
                         3. In Discord > Voice & Video: set Input Device to \"CABLE Output\".\n\
                         4. In Discord turn OFF noise suppression, echo cancellation and automatic \
                         gain control, or they will fight the effects. Use the Noise suppression \
                         slider below instead: it never cuts your voice out completely.",
                    );
                });
                egui::CollapsingHeader::new("Noise suppression").default_open(true).show(ui, |ui| {
                    sl(ui, &mut self.ns_amount, 0.0, 0.0..=1.0, "strength", "", false, 0.0);
                    hint(
                        ui,
                        "0 = off. Removes steady background noise (fans, hiss, hum, room tone) before the \
                         effects, and never fully mutes you. Adds about 10 ms of delay. Try 0.5 first: \
                         raise it if you still hear noise, lower it if your voice sounds watery. Sudden \
                         sounds like keyboard clicks are only reduced a little; use the noise gate for those.",
                    );
                });

                ui.separator();
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Quick, "Quick");
                    ui.selectable_value(&mut self.tab, Tab::Custom, "Custom");
                    ui.selectable_value(&mut self.tab, Tab::Soundboard, "Soundboard");
                });
                if self.tab != Tab::Soundboard {
                    self.voice_tab = self.tab;
                }
                ui.separator();

                match self.tab {
                    Tab::Quick => self.ui_quick(ui),
                    Tab::Custom => self.ui_custom(ui),
                    Tab::Soundboard => self.ui_soundboard(ui),
                }
            });
        });

        // Push the active voice settings to the audio thread (only when changed).
        let mut s = if self.voice_tab == Tab::Custom { self.custom } else { self.quick_settings() };
        s.ns_amount = self.ns_amount;
        s.out_gain = self.out_gain;
        s.muted = self.muted;
        s.sb_vol = self.sb_vol;
        s.mon_vol = self.mon_vol;
        if s != self.last_sent {
            if let Ok(mut g) = self.params.settings.lock() {
                *g = s;
                self.last_sent = s;
            }
        }

        // Meters (peak-hold since last frame, smooth fall-off).
        let ip = f32::from_bits(self.params.in_peak.swap(0, Relaxed));
        let op = f32::from_bits(self.params.out_peak.swap(0, Relaxed));
        self.in_level = ip.max(self.in_level * 0.6);
        self.out_level = op.max(self.out_level * 0.6);

        if restart {
            self.restart_engine();
        }

        // Save settings a moment after anything changes.
        if self.last_check.elapsed() >= Duration::from_millis(700) {
            self.last_check = Instant::now();
            let text = self.config_text();
            if text != self.last_saved && save_config(&text).is_ok() {
                self.last_saved = text;
            }
        }

        // Low refresh rate, to leave the CPU free for everything else (your
        // typing, games, Discord): ~8 fps in front, ~4 fps when the meters are
        // idle, 1 fps in the background, and almost nothing when minimized.
        // Clicks and drags always redraw instantly, finished sound loads wake
        // the window by themselves, and the audio runs on its own thread.
        let (focused, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (v.focused.unwrap_or(true), v.minimized.unwrap_or(false))
        });
        let idle = self.in_level < 0.0005 && self.out_level < 0.0005;
        let ms = if minimized {
            3000
        } else if !focused {
            1000
        } else if idle {
            250
        } else {
            125
        };
        ctx.request_repaint_after(Duration::from_millis(ms));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 800.0])
            .with_min_inner_size([480.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("Voice Changer", options, Box::new(|cc| Box::new(App::new(cc.egui_ctx.clone()))))
}

// ---------------------------------------------------------------------------
// Tests: run the whole chain on synthetic audio, no sound hardware needed
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with(settings: Settings, n: usize, input: &dyn Fn(usize) -> f32) -> Vec<f32> {
        let params = Arc::new(Params::new());
        *params.settings.lock().unwrap() = settings;
        let (_tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        let mut out = Vec::new();
        for block in 0..(n / 480) {
            p.begin_block();
            for i in 0..480 {
                out.push(p.process(input(block * 480 + i)));
            }
            p.end_block();
        }
        out
    }

    fn tone(i: usize) -> f32 {
        0.3 * (2.0 * PI * 220.0 * i as f32 / 48000.0).sin()
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
    }

    #[test]
    fn neutral_is_transparent() {
        let out = run_with(Settings::neutral(), 4800, &tone);
        assert!((out[4000] - tone(4000)).abs() < 1e-4);
    }

    #[test]
    fn extreme_settings_stay_finite_and_bounded() {
        let s = Settings {
            in_gain_db: 30.0,
            gate_db: -20.0,
            hpf_hz: 600.0,
            ns_amount: 1.0,
            eq_low_db: 12.0,
            eq_mid_db: -12.0,
            eq_high_db: 12.0,
            comp_ratio: 20.0,
            comp_thresh_db: -40.0,
            comp_makeup_db: 24.0,
            deess: 1.0,
            semitones: -24.0,
            pitch_grain_ms: 100.0,
            robot_amount: 1.0,
            robot_hz: 200.0,
            robot_wave: 1,
            whisper: 1.0,
            vib_depth: 1.0,
            trem_depth: 1.0,
            chorus_mix: 1.0,
            chorus_flange: 1,
            echo_mix: 1.0,
            echo_fb: 0.9,
            echo_ms: 800.0,
            echo_damp: 1.0,
            reverb_mix: 1.0,
            reverb_size: 1.0,
            drive: 1.0,
            dist_type: 2,
            bits: 4.0,
            rate_hz: 2000.0,
            lpf_hz: 1000.0,
            hiss: 0.1,
            out_gain: 3.0,
            sb_vol: 2.0,
            ..Settings::neutral()
        };
        let out = run_with(s, 96000, &tone);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        assert!(out.iter().any(|v| v.abs() > 0.01));
    }

    #[test]
    fn each_new_effect_alone_is_finite() {
        let n = Settings::neutral();
        let cases = [
            Settings { eq_low_db: 9.0, eq_mid_db: 6.0, eq_high_db: -9.0, ..n },
            Settings { comp_ratio: 6.0, ..n },
            Settings { deess: 0.8, ..n },
            Settings { vib_depth: 0.8, ..n },
            Settings { trem_depth: 0.8, ..n },
            Settings { chorus_mix: 0.8, ..n },
            Settings { chorus_mix: 0.8, chorus_flange: 1, ..n },
            Settings { reverb_mix: 0.8, ..n },
            Settings { whisper: 0.8, ..n },
            Settings { semitones: 24.0, ..n },
        ];
        for s in cases {
            let out = run_with(s, 48000, &tone);
            assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
            assert!(rms(&out[24000..]) > 0.001);
        }
    }

    #[test]
    fn fft_round_trip() {
        let fft = Fft::new(NS_N);
        let orig: Vec<f32> = (0..NS_N).map(|i| ((i * 7) % 13) as f32 - 6.0).collect();
        let mut re = orig.clone();
        let mut im = vec![0.0; NS_N];
        fft.run(&mut re, &mut im, false);
        fft.run(&mut re, &mut im, true);
        for i in 0..NS_N {
            assert!((re[i] - orig[i]).abs() < 1e-3);
            assert!(im[i].abs() < 1e-3);
        }
    }

    #[test]
    fn noise_suppression_reduces_steady_noise() {
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..48000 * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / 16777216.0 * 2.0 - 1.0) * 0.1
            })
            .collect();
        let s = Settings { ns_amount: 1.0, ..Settings::neutral() };
        let out = run_with(s, noise.len(), &|i| noise[i]);
        let before = rms(&noise[3 * 48000..4 * 48000 - 480]);
        let after = rms(&out[3 * 48000..4 * 48000 - 480]);
        assert!(after < before * 0.5, "before {before}, after {after}");
    }

    #[test]
    fn noise_suppression_keeps_a_voice_like_tone() {
        let mut seed = 777u32;
        let noise: Vec<f32> = (0..48000 * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / 16777216.0 * 2.0 - 1.0) * 0.05
            })
            .collect();
        let input = |i: usize| {
            let t = i as f32 / 48000.0;
            noise[i] + if t > 1.0 { 0.3 * (2.0 * PI * 300.0 * t).sin() } else { 0.0 }
        };
        let s = Settings { ns_amount: 1.0, ..Settings::neutral() };
        let out = run_with(s, noise.len(), &input);
        let a: Vec<f32> = (3 * 48000..4 * 48000 - 480).map(input).collect();
        let b = &out[3 * 48000..4 * 48000 - 480];
        assert!(rms(b) > rms(&a) * 0.8, "kept {} of {}", rms(b), rms(&a));
    }

    #[test]
    fn settings_round_trip_through_text() {
        let mut s = Settings::neutral();
        s.semitones = -5.5;
        s.reverb_mix = 0.35;
        s.robot_wave = 3;
        s.dist_type = 2;
        s.chorus_flange = 1;
        let text = s.serialize();
        let mut back = Settings::neutral();
        back.apply(&text);
        assert_eq!(back, s);
        // junk is ignored
        back.apply("nonsense;semitones=abc;=;robot_wave=99");
        assert_eq!(back.robot_wave, 3);
        assert_eq!(back.semitones, -5.5);
    }

    #[test]
    fn mute_silences_mic_but_soundboard_plays() {
        let params = Arc::new(Params::new());
        let mut s = Settings::neutral();
        s.muted = true;
        *params.settings.lock().unwrap() = s;
        let (tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        tx.send(Cmd::Play { data: Arc::new(vec![16384i16; 100]), gain: 1.0, step: 1.0, looping: false }).unwrap();
        p.begin_block();
        let a = p.process(0.9);
        for _ in 0..98 {
            p.process(0.9);
        }
        let last = p.process(0.9);
        let after = p.process(0.9);
        p.end_block();
        assert!((a - 0.5).abs() < 1e-6 && (last - 0.5).abs() < 1e-6);
        assert!(after.abs() < 1e-6);
        assert!(p.voices.is_empty());
    }

    #[test]
    fn looping_sound_keeps_playing() {
        let params = Arc::new(Params::new());
        let (tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        tx.send(Cmd::Play { data: Arc::new(vec![8192i16; 50]), gain: 1.0, step: 1.0, looping: true }).unwrap();
        p.begin_block();
        for _ in 0..500 {
            p.process(0.0);
        }
        p.end_block();
        assert_eq!(p.voices.len(), 1);
    }

    #[test]
    fn resample_changes_length() {
        let v: Vec<i16> = (0..44100).map(|i| ((i as f32 * 0.01).sin() * 10000.0) as i16).collect();
        let r = resample_i16(&v, 44100, 48000);
        assert!((r.len() as i64 - 48000).abs() <= 1);
    }
}
