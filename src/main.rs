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
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, SampleRate, Stream, StreamConfig};
use eframe::egui;
use ringbuf::{traits::*, HeapCons, HeapProd, HeapRb};

// ---------------------------------------------------------------------------
// Settings shared between the UI thread and the audio thread
// ---------------------------------------------------------------------------

/// Every adjustable value of the effect chain. The UI writes a fresh copy
/// every frame; the audio thread picks it up once per buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    // cleanup / input
    in_gain_db: f32, // -20..30 dB
    gate_db: f32,    // noise gate threshold, <= -79 means off
    hpf_hz: f32,     // high-pass, <= 25 means off
    // voice
    semitones: f32,    // pitch shift, ~0 means off
    robot_amount: f32, // 0..1 blend of ring modulation
    robot_hz: f32,
    // echo
    echo_mix: f32,
    echo_fb: f32,
    echo_ms: f32,
    // lo-fi / "cheap mic"
    drive: f32,    // distortion 0..1
    bits: f32,     // bit depth, >= 16 means off
    rate_hz: f32,  // sample-rate reduction, >= engine rate means off
    lpf_hz: f32,   // low-pass, >= 19000 means off
    hiss: f32,     // noise floor 0..0.1
    // output
    out_gain: f32,
    muted: bool, // mutes the mic only; soundboard keeps playing
    sb_vol: f32, // soundboard volume
}

impl Settings {
    /// Everything off: the voice passes through untouched.
    fn neutral() -> Self {
        Self {
            in_gain_db: 0.0,
            gate_db: -80.0,
            hpf_hz: 20.0,
            semitones: 0.0,
            robot_amount: 0.0,
            robot_hz: 50.0,
            echo_mix: 0.0,
            echo_fb: 0.45,
            echo_ms: 300.0,
            drive: 0.0,
            bits: 16.0,
            rate_hz: 48000.0,
            lpf_hz: 20000.0,
            hiss: 0.0,
            out_gain: 1.0,
            muted: false,
            sb_vol: 1.0,
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
    Play(Arc<Vec<i16>>),
    StopAll,
}

// ---------------------------------------------------------------------------
// DSP
// ---------------------------------------------------------------------------

/// Second-order (12 dB/oct) Butterworth filter.
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
    fn set(&mut self, lowpass: bool, fc: f32, sr: f32) {
        let w0 = 2.0 * PI * fc.clamp(10.0, sr * 0.45) / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * 0.7071);
        let (b0, b1, b2) = if lowpass {
            ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0)
        } else {
            ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0)
        };
        let a0 = 1.0 + alpha;
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = -2.0 * cos / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

struct Voice {
    data: Arc<Vec<i16>>,
    pos: usize,
}

const MAX_VOICES: usize = 16;

struct Processor {
    params: Arc<Params>,
    rx: Receiver<Cmd>,
    sr: f32,
    st: Settings,

    // values derived from `st` once per buffer
    robot_amount: f32,
    echo_on: bool,
    echo_mix: f32,
    echo_fb: f32,
    drive: f32,
    hiss: f32,
    out_gain: f32,
    muted: bool,
    sb_vol: f32,
    in_gain: f32,
    gate_thresh: f32,
    hpf_on: bool,
    lpf_on: bool,
    pitch_on: bool,
    pitch_step: f32,
    robot_inc: f32,
    echo_delay: usize,
    drive_k: f32,
    levels: f32,    // bit-crusher levels, 0 = off
    hold_step: f32, // sample-rate reducer step, 0 = off

    // filter state
    hpf: Biquad,
    lpf: Biquad,
    last_hpf: f32,
    last_lpf: f32,
    // noise gate
    env: f32,
    gate_g: f32,
    gate_atk: f32,
    gate_rel: f32,
    env_rel: f32,
    // pitch shifter (two crossfaded delay-line taps)
    pbuf: Vec<f32>,
    pwrite: usize,
    pphase: f32,
    pwindow: f32,
    // robot
    rphase: f32,
    // echo
    ebuf: Vec<f32>,
    eidx: usize,
    // sample-rate reducer
    hold_acc: f32,
    hold_val: f32,
    // noise
    rng: u32,
    // soundboard
    voices: Vec<Voice>,
}

impl Processor {
    fn new(params: Arc<Params>, sr: f32, rx: Receiver<Cmd>) -> Self {
        let pwindow = sr * 0.05; // 50 ms grains
        let coef = |secs: f32| 1.0 - (-1.0 / (secs * sr)).exp();
        Self {
            params,
            rx,
            sr,
            st: Settings::neutral(),
            robot_amount: 0.0,
            echo_on: false,
            echo_mix: 0.0,
            echo_fb: 0.0,
            drive: 0.0,
            hiss: 0.0,
            out_gain: 1.0,
            muted: false,
            sb_vol: 1.0,
            in_gain: 1.0,
            gate_thresh: 0.0,
            hpf_on: false,
            lpf_on: false,
            pitch_on: false,
            pitch_step: 0.0,
            robot_inc: 0.0,
            echo_delay: 1,
            drive_k: 1.0,
            levels: 0.0,
            hold_step: 0.0,
            hpf: Biquad::default(),
            lpf: Biquad::default(),
            last_hpf: 0.0,
            last_lpf: 0.0,
            env: 0.0,
            gate_g: 1.0,
            gate_atk: coef(0.002),
            gate_rel: coef(0.08),
            env_rel: (-1.0 / (0.03 * sr)).exp(),
            pbuf: vec![0.0; pwindow as usize + 8],
            pwrite: 0,
            pphase: 0.0,
            pwindow,
            rphase: 0.0,
            ebuf: vec![0.0; sr as usize + 1], // up to 1 s of echo
            eidx: 0,
            hold_acc: 0.0,
            hold_val: 0.0,
            rng: 0x1234_5678,
            voices: Vec::with_capacity(MAX_VOICES),
        }
    }

    /// Call once at the start of every audio buffer.
    fn begin_block(&mut self) {
        if let Ok(g) = self.params.settings.try_lock() {
            self.st = *g;
        }
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Cmd::Play(data) => {
                    if self.voices.len() >= MAX_VOICES {
                        self.voices.remove(0);
                    }
                    self.voices.push(Voice { data, pos: 0 });
                }
                Cmd::StopAll => self.voices.clear(),
            }
        }

        let s = self.st;
        self.robot_amount = s.robot_amount;
        self.echo_mix = s.echo_mix;
        self.echo_fb = s.echo_fb;
        self.drive = s.drive;
        self.hiss = s.hiss;
        self.out_gain = s.out_gain;
        self.muted = s.muted;
        self.sb_vol = s.sb_vol;
        // Echo is skipped entirely while off; clear the tail when it switches on.
        let echo_on = s.echo_mix > 0.001;
        if echo_on && !self.echo_on {
            self.ebuf.fill(0.0);
        }
        self.echo_on = echo_on;
        self.in_gain = 10f32.powf(s.in_gain_db / 20.0);
        self.gate_thresh = if s.gate_db <= -79.0 { 0.0 } else { 10f32.powf(s.gate_db / 20.0) };

        self.hpf_on = s.hpf_hz > 25.0;
        if self.hpf_on && (s.hpf_hz - self.last_hpf).abs() > 0.5 {
            self.hpf.set(false, s.hpf_hz, self.sr);
            self.last_hpf = s.hpf_hz;
        }
        self.lpf_on = s.lpf_hz < 19000.0f32.min(self.sr * 0.45);
        if self.lpf_on && (s.lpf_hz - self.last_lpf).abs() > 0.5 {
            self.lpf.set(true, s.lpf_hz, self.sr);
            self.last_lpf = s.lpf_hz;
        }

        self.pitch_on = s.semitones.abs() > 0.05;
        let ratio = 2f32.powf(s.semitones / 12.0);
        self.pitch_step = (1.0 - ratio) / self.pwindow;
        self.robot_inc = 2.0 * PI * s.robot_hz / self.sr;
        self.echo_delay = ((s.echo_ms * 0.001 * self.sr) as usize).clamp(1, self.ebuf.len() - 1);
        self.drive_k = 1.0 + s.drive * 19.0;
        self.levels = if s.bits < 15.5 { 2f32.powf(s.bits.round().max(2.0) - 1.0) } else { 0.0 };
        self.hold_step = if s.rate_hz < self.sr * 0.98 { self.sr / s.rate_hz.max(500.0) } else { 0.0 };
    }

    /// Call once at the end of every audio buffer.
    fn end_block(&mut self) {
        self.voices.retain(|v| v.pos < v.data.len());
    }

    fn read_pitch(&self, delay: f32) -> f32 {
        let len = self.pbuf.len();
        let pos = (self.pwrite as f32 - 1.0 - delay).rem_euclid(len as f32);
        let i0 = pos.floor() as usize % len;
        let i1 = (i0 + 1) % len;
        let frac = pos - pos.floor();
        self.pbuf[i0] * (1.0 - frac) + self.pbuf[i1] * frac
    }

    fn noise(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn process(&mut self, input: f32) -> f32 {
        let mut x = input * self.in_gain;

        // 1. noise gate
        if self.gate_thresh > 0.0 {
            let a = x.abs();
            self.env = if a > self.env { a } else { self.env * self.env_rel };
            let target = if self.env > self.gate_thresh { 1.0 } else { 0.0 };
            let c = if target > self.gate_g { self.gate_atk } else { self.gate_rel };
            self.gate_g += (target - self.gate_g) * c;
            x *= self.gate_g;
        }

        // 2. high-pass (rumble / low-end removal)
        if self.hpf_on {
            x = self.hpf.process(x);
        }

        // 3. pitch shift (delay line is always fed so toggling is click-free)
        self.pbuf[self.pwrite] = x;
        self.pwrite = (self.pwrite + 1) % self.pbuf.len();
        if self.pitch_on {
            self.pphase = (self.pphase + self.pitch_step).rem_euclid(1.0);
            let pb = (self.pphase + 0.5).rem_euclid(1.0);
            // Hann crossfade: sin^2(pi*p) = (1 - cos(2*pi*p)) / 2, and the other tap is 1 - that.
            let wa = 0.5 - 0.5 * (2.0 * PI * self.pphase).cos();
            let wb = 1.0 - wa;
            x = self.read_pitch(self.pphase * self.pwindow) * wa
                + self.read_pitch(pb * self.pwindow) * wb;
        }

        // 4. robot (ring modulation, blended)
        if self.robot_amount > 0.001 {
            self.rphase = (self.rphase + self.robot_inc) % (2.0 * PI);
            x *= 1.0 - self.robot_amount + self.robot_amount * self.rphase.sin();
        }

        // 5. echo (skipped completely while the mix is 0)
        if self.echo_on {
            let elen = self.ebuf.len();
            let delayed = self.ebuf[(self.eidx + elen - self.echo_delay) % elen];
            self.ebuf[self.eidx] = x + delayed * self.echo_fb;
            self.eidx = (self.eidx + 1) % elen;
            x += delayed * self.echo_mix;
        }

        // 6. distortion
        if self.drive > 0.01 {
            x = (x * self.drive_k).tanh() / self.drive_k.tanh();
        }

        // 7. bit crusher
        if self.levels > 0.0 {
            x = (x.clamp(-1.0, 1.0) * self.levels).round() / self.levels;
        }

        // 8. sample-rate reducer (sample & hold)
        if self.hold_step > 0.0 {
            self.hold_acc += 1.0;
            if self.hold_acc >= self.hold_step {
                self.hold_acc -= self.hold_step;
                self.hold_val = x;
            }
            x = self.hold_val;
        }

        // 9. low-pass ("telephone" / muffled)
        if self.lpf_on {
            x = self.lpf.process(x);
        }

        // 10. hiss
        if self.hiss > 0.0 {
            x += self.noise() * self.hiss;
        }

        let voice = if self.muted { 0.0 } else { x * self.out_gain };

        // soundboard (16-bit samples) is mixed in after the effects
        let mut sb = 0.0f32;
        for v in self.voices.iter_mut() {
            if let Some(&smp) = v.data.get(v.pos) {
                sb += smp as f32;
                v.pos += 1;
            }
        }

        (voice + sb * (self.sb_vol / 32768.0)).clamp(-1.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// Sound file decoding (mp3, wav, ogg, flac) -> mono f32
// ---------------------------------------------------------------------------

struct Loaded {
    name: String,
    data: Vec<i16>,
    rate: u32,
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

/// A loaded sound: a single 16-bit mono copy, kept at the engine's sample rate.
struct Sound {
    name: String,
    data: Arc<Vec<i16>>,
    rate: u32,
}

impl Sound {
    fn duration(&self) -> f32 {
        self.data.len() as f32 / self.rate as f32
    }

    /// Converts the stored copy to a new sample rate (only when it differs).
    fn convert_to(&mut self, rate: u32) {
        if rate != self.rate {
            self.data = Arc::new(resample_i16(&self.data, self.rate, rate));
            self.rate = rate;
        }
    }
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
                    let mut peak = 0.0f32;
                    for frame in data.chunks(in_ch) {
                        peak = peak.max(frame[0].abs());
                        let y = proc.process(frame[0]);
                        let _ = prod.try_push(y);
                        if let Some(m) = mon_prod.as_mut() {
                            let _ = m.try_push(y);
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

struct App {
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

    // custom tab (full chain)
    custom: Settings,

    // common
    out_gain: f32,
    muted: bool,
    sb_vol: f32,
    last_sent: Settings,
    in_level: f32,
    out_level: f32,

    // soundboard
    sounds: Vec<Sound>,
    load_tx: Sender<Result<Loaded, String>>,
    load_rx: Receiver<Result<Loaded, String>>,
    pending: usize,
    sound_error: Option<String>,
}

impl App {
    fn new() -> Self {
        let (load_tx, load_rx) = mpsc::channel();
        let mut app = Self {
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
            out_gain: 1.0,
            muted: false,
            sb_vol: 1.0,
            last_sent: Settings::neutral(),
            in_level: 0.0,
            out_level: 0.0,
            sounds: Vec::new(),
            load_tx,
            load_rx,
            pending: 0,
            sound_error: None,
        };
        app.restart_engine();
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

    fn add_sounds(&mut self, paths: Vec<PathBuf>) {
        let target = self.engine.as_ref().map(|e| e.sample_rate);
        for p in paths {
            let tx = self.load_tx.clone();
            self.pending += 1;
            std::thread::spawn(move || {
                let name = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "sound".into());
                let res = decode_file(&p, target)
                    .map(|(data, rate)| Loaded { name, data, rate })
                    .map_err(|e| format!("{}: {e}", p.display()));
                let _ = tx.send(res);
            });
        }
    }

    fn play_sound(&mut self, i: usize) {
        if let Some(e) = &self.engine {
            let snd = &mut self.sounds[i];
            snd.convert_to(e.sample_rate);
            let _ = e.tx.send(Cmd::Play(snd.data.clone()));
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
                ui.add(egui::Slider::new(&mut self.q_semis, -12.0..=12.0).text("semitones").step_by(0.5));
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
                ui.add(egui::Slider::new(&mut self.q_robot_hz, 10.0..=200.0).text("mod freq (Hz)"));
            }
            Q_ECHO => {
                ui.add(egui::Slider::new(&mut self.q_echo_mix, 0.0..=1.0).text("mix"));
                ui.add(egui::Slider::new(&mut self.q_echo_fb, 0.0..=0.9).text("feedback"));
            }
            _ => {
                ui.label(egui::RichText::new("Your voice passes through unchanged.").weak());
            }
        }
    }

    fn ui_custom(&mut self, ui: &mut egui::Ui) {
        ui.label("Every effect runs at the same time. Each slider's rest position means \"off\".");
        ui.horizontal_wrapped(|ui| {
            ui.label("Presets:");
            let c = Settings::neutral();
            if ui.button("Reset").clicked() {
                self.custom = c;
            }
            if ui.button("Clean").clicked() {
                self.custom = Settings { in_gain_db: 3.0, gate_db: -55.0, hpf_hz: 90.0, ..c };
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
        });
        ui.add_space(6.0);

        let s = &mut self.custom;
        egui::CollapsingHeader::new("Input & cleanup").default_open(true).show(ui, |ui| {
            ui.add(egui::Slider::new(&mut s.in_gain_db, -20.0..=30.0).text("input gain").suffix(" dB"));
            ui.add(egui::Slider::new(&mut s.gate_db, -80.0..=-20.0).text("noise gate").suffix(" dB"));
            ui.add(egui::Slider::new(&mut s.hpf_hz, 20.0..=600.0).logarithmic(true).text("high-pass").suffix(" Hz"));
            ui.label(egui::RichText::new("Gate: -80 = off. High-pass: 20 = off.").weak().small());
        });
        egui::CollapsingHeader::new("Voice").default_open(true).show(ui, |ui| {
            ui.add(egui::Slider::new(&mut s.semitones, -12.0..=12.0).text("pitch").suffix(" st").step_by(0.5));
            ui.add(egui::Slider::new(&mut s.robot_amount, 0.0..=1.0).text("robot amount"));
            ui.add(egui::Slider::new(&mut s.robot_hz, 10.0..=200.0).text("robot freq").suffix(" Hz"));
        });
        egui::CollapsingHeader::new("Echo").default_open(true).show(ui, |ui| {
            ui.add(egui::Slider::new(&mut s.echo_mix, 0.0..=1.0).text("mix"));
            ui.add(egui::Slider::new(&mut s.echo_fb, 0.0..=0.9).text("feedback"));
            ui.add(egui::Slider::new(&mut s.echo_ms, 50.0..=800.0).text("time").suffix(" ms"));
        });
        egui::CollapsingHeader::new("Lo-fi (cheap mic)").default_open(true).show(ui, |ui| {
            ui.add(egui::Slider::new(&mut s.drive, 0.0..=1.0).text("distortion"));
            ui.add(egui::Slider::new(&mut s.bits, 4.0..=16.0).text("bit depth").step_by(1.0));
            ui.add(egui::Slider::new(&mut s.rate_hz, 2000.0..=48000.0).logarithmic(true).text("sample rate").suffix(" Hz"));
            ui.add(egui::Slider::new(&mut s.lpf_hz, 1000.0..=20000.0).logarithmic(true).text("low-pass").suffix(" Hz"));
            ui.add(egui::Slider::new(&mut s.hiss, 0.0..=0.1).text("hiss / noise"));
            ui.label(egui::RichText::new("Bit depth 16, sample rate 48000 and low-pass 20000 = off.").weak().small());
        });
    }

    fn ui_soundboard(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Add sound files...").clicked() {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Audio", &["mp3", "wav", "ogg", "flac"])
                    .pick_files()
                {
                    self.add_sounds(paths);
                }
            }
            if ui.button("Stop all").clicked() {
                self.stop_sounds();
            }
            if self.pending > 0 {
                ui.label(egui::RichText::new(format!("Loading {}...", self.pending)).weak());
            }
        });
        ui.add(egui::Slider::new(&mut self.sb_vol, 0.0..=2.0).text("soundboard volume"));
        if let Some(e) = &self.sound_error {
            ui.colored_label(egui::Color32::LIGHT_RED, e);
        }
        ui.separator();

        if self.sounds.is_empty() {
            ui.label(egui::RichText::new("No sounds yet. Add mp3, wav, ogg or flac files.").weak());
        }
        let mut remove: Option<usize> = None;
        for i in 0..self.sounds.len() {
            let name = self.sounds[i].name.clone();
            let dur = self.sounds[i].duration();
            ui.horizontal(|ui| {
                if ui.button("Play").clicked() {
                    self.play_sound(i);
                }
                if ui.small_button("X").on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
                ui.label(format!("{name}  ({dur:.1}s)"));
            });
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
        while let Ok(res) = self.load_rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match res {
                Ok(l) => {
                    self.sound_error = None;
                    let mut snd = Sound { name: l.name, data: Arc::new(l.data), rate: l.rate };
                    if let Some(e) = &self.engine {
                        snd.convert_to(e.sample_rate); // safety net if the rate changed while loading
                    }
                    self.sounds.push(snd);
                }
                Err(e) => self.sound_error = Some(format!("Could not load {e}")),
            }
        }

        let mut restart = false;

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut self.out_gain, 0.0..=3.0).text("mic output gain"));
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
                         gain control, or they will fight the effects.",
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

        // Push the active voice settings to the audio thread.
        let mut s = if self.voice_tab == Tab::Custom { self.custom } else { self.quick_settings() };
        s.out_gain = self.out_gain;
        s.muted = self.muted;
        s.sb_vol = self.sb_vol;
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

        // Low refresh rate: ~8 fps while the window is in front, ~1 fps in the
        // background. User input always triggers an immediate redraw anyway, and
        // the audio runs on its own thread, so this only affects the meters.
        let (focused, minimized) = ctx.input(|i| {
            let v = i.viewport();
            (v.focused.unwrap_or(true), v.minimized.unwrap_or(false))
        });
        let ms = if minimized || !focused { 1000 } else { 125 };
        ctx.request_repaint_after(Duration::from_millis(ms));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([540.0, 760.0])
            .with_min_inner_size([480.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("Voice Changer", options, Box::new(|_cc| Box::new(App::new())))
}

// ---------------------------------------------------------------------------
// Tests: run the whole chain on synthetic audio, no sound hardware needed
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn run(settings: Settings, n: usize) -> (Vec<f32>, Sender<Cmd>) {
        let params = Arc::new(Params::new());
        *params.settings.lock().unwrap() = settings;
        let (tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        let mut out = Vec::new();
        for block in 0..(n / 480) {
            p.begin_block();
            for i in 0..480 {
                let t = (block * 480 + i) as f32 / 48000.0;
                let x = 0.3 * (2.0 * PI * 220.0 * t).sin();
                out.push(p.process(x));
            }
            p.end_block();
        }
        (out, tx)
    }

    #[test]
    fn neutral_is_transparent() {
        let (out, _tx) = run(Settings::neutral(), 4800);
        let expect = 0.3 * (2.0 * PI * 220.0 * 4000.0 / 48000.0).sin();
        assert!((out[4000] - expect).abs() < 1e-4);
    }

    #[test]
    fn extreme_settings_stay_finite_and_bounded() {
        let s = Settings {
            in_gain_db: 30.0,
            gate_db: -20.0,
            hpf_hz: 600.0,
            semitones: -12.0,
            robot_amount: 1.0,
            robot_hz: 200.0,
            echo_mix: 1.0,
            echo_fb: 0.9,
            echo_ms: 800.0,
            drive: 1.0,
            bits: 4.0,
            rate_hz: 2000.0,
            lpf_hz: 1000.0,
            hiss: 0.1,
            out_gain: 3.0,
            muted: false,
            sb_vol: 2.0,
        };
        let (out, _tx) = run(s, 96000);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        assert!(out.iter().any(|v| v.abs() > 0.01));
    }

    #[test]
    fn mute_silences_mic_but_soundboard_plays() {
        let params = Arc::new(Params::new());
        let mut s = Settings::neutral();
        s.muted = true;
        *params.settings.lock().unwrap() = s;
        let (tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        tx.send(Cmd::Play(Arc::new(vec![16384i16; 100]))).unwrap();
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
    fn resample_changes_length() {
        let v: Vec<i16> = (0..44100).map(|i| ((i as f32 * 0.01).sin() * 10000.0) as i16).collect();
        let r = resample_i16(&v, 44100, 48000);
        assert!((r.len() as i64 - 48000).abs() <= 1);
    }
}
