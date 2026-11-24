//! The `Processor` struct, its construction and per-buffer bookkeeping.
//! Derived coefficients live in `derived.rs`, the per-sample chain in `chain.rs`.

use std::f32::consts::PI;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use super::filters::{enable_ftz, Biquad};
use super::noise::NoiseSuppressor;
use crate::settings::{Cmd, Params, Settings};

mod chain;
mod derived;
mod autotune;
use autotune::Autotune;

pub(super) struct Voice {
    data: Arc<Vec<i16>>,
    pos: f64,
    step: f64,
    gain: f32,
    looping: bool,
    start: f64,
    end: f64,
    fade_in: bool,  // short fade when a sound was trimmed at the start
    fade_out: bool, // short fade when a sound was trimmed at the end
    vc: bool,       // run this sound through the voice changer
}

const MAX_VOICES: usize = 16;
const RV_DAMP: f32 = 0.3;

pub(crate) struct Processor {
    params: Arc<Params>,
    rx: Receiver<Cmd>,
    sr: f32,
    pub(crate) st: Settings,
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
    at_on: bool,
    fm_inv: f32, // 1 / formant ratio, 0 = off
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
    harm_on: [bool; 2],
    harm_step: [f32; 2],
    growl_on: bool,
    growl_inc: f32,
    dbl_on: bool,
    dbl_base: f32,
    dbl_inc: f32,
    limiter_on: bool,
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
    at: Autotune,
    hpf: Biquad,
    lpf: Biquad,
    eq_low: Biquad,
    eq_mid: Biquad,
    eq_high: Biquad,
    env: f32,
    gate_g: f32,
    gate_open: bool,
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
    hphase: [f32; 2],
    gphase: f32,
    dbuf: Vec<f32>,
    dwrite: usize,
    dphase: f32,
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
    pub(super) voices: Vec<Voice>,
}

impl Processor {
    pub(crate) fn new(params: Arc<Params>, sr: f32, rx: Receiver<Cmd>) -> Self {
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
            at_on: false,
            fm_inv: 0.0,
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
            harm_on: [false; 2],
            harm_step: [0.0; 2],
            growl_on: false,
            growl_inc: 0.0,
            dbl_on: false,
            dbl_base: 0.0,
            dbl_inc: 0.0,
            limiter_on: true,
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
            at: Autotune::new(sr),
            hpf: Biquad::default(),
            lpf: Biquad::default(),
            eq_low: Biquad::default(),
            eq_mid: Biquad::default(),
            eq_high: Biquad::default(),
            env: 0.0,
            gate_g: 1.0,
            gate_open: false,
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
            hphase: [0.0; 2],
            gphase: 0.0,
            dbuf: vec![0.0; (sr * 0.08) as usize + 8],
            dwrite: 0,
            dphase: 0.0,
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
    pub(crate) fn begin_block(&mut self) {
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
                Cmd::Play { data, gain, step, looping, start, end, vc } => {
                    if self.voices.len() >= MAX_VOICES {
                        self.voices.remove(0);
                    }
                    let len = data.len() as f64;
                    let end = end.min(len);
                    if end > start + 1.0 {
                        self.voices.push(Voice {
                            data,
                            pos: start,
                            step,
                            gain,
                            looping,
                            start,
                            end,
                            fade_in: start > 0.5,
                            fade_out: end < len - 0.5,
                            vc,
                        });
                    }
                }
                Cmd::StopAll => self.voices.clear(),
            }
        }
        if self.st_dirty {
            self.st_dirty = false;
            self.update_derived();
        }
    }
}
