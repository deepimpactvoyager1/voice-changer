// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Real-time voice changer with a desktop UI (egui).
//!
//!   cargo run --release
//!
//! USE HEADPHONES, otherwise the speakers feed back into the mic.

// Hide the console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::f32::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, Stream, StreamConfig};
use eframe::egui;
use ringbuf::{traits::*, HeapRb};

// ---------------------------------------------------------------------------
// Shared parameters (UI thread writes, audio thread reads; all lock-free)
// ---------------------------------------------------------------------------

const MODE_NORMAL: u8 = 0;
const MODE_PITCH: u8 = 1;
const MODE_ROBOT: u8 = 2;
const MODE_ECHO: u8 = 3;

struct Params {
    mode: AtomicU8,
    semitones: AtomicU32, // f32 stored as bits
    robot_hz: AtomicU32,
    echo_mix: AtomicU32,
    echo_fb: AtomicU32,
    gain: AtomicU32,
    muted: AtomicBool,
    // Peak levels as f32 bits. Non-negative floats order the same as their
    // bit patterns, so fetch_max works as a peak-hold; the UI resets with swap(0).
    in_peak: AtomicU32,
    out_peak: AtomicU32,
}

fn f(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Relaxed))
}
fn set(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Relaxed);
}

impl Params {
    fn new() -> Self {
        Self {
            mode: AtomicU8::new(MODE_PITCH),
            semitones: AtomicU32::new(4.0f32.to_bits()),
            robot_hz: AtomicU32::new(50.0f32.to_bits()),
            echo_mix: AtomicU32::new(0.5f32.to_bits()),
            echo_fb: AtomicU32::new(0.45f32.to_bits()),
            gain: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            in_peak: AtomicU32::new(0),
            out_peak: AtomicU32::new(0),
        }
    }
}

// ---------------------------------------------------------------------------
// DSP
// ---------------------------------------------------------------------------

/// Parameter snapshot taken once per audio buffer.
struct Settings {
    mode: u8,
    pitch_step: f32,
    robot_inc: f32,
    echo_mix: f32,
    echo_fb: f32,
    gain: f32,
    muted: bool,
}

struct Processor {
    params: Arc<Params>,
    sr: f32,
    // pitch shifter (two crossfaded delay-line taps)
    pbuf: Vec<f32>,
    pwrite: usize,
    pphase: f32,
    pwindow: f32,
    // robot (ring modulator)
    rphase: f32,
    // echo
    ebuf: Vec<f32>,
    eidx: usize,
}

impl Processor {
    fn new(params: Arc<Params>, sr: f32) -> Self {
        let pwindow = sr * 0.05; // 50 ms grains
        Self {
            params,
            sr,
            pbuf: vec![0.0; pwindow as usize + 8],
            pwrite: 0,
            pphase: 0.0,
            pwindow,
            rphase: 0.0,
            ebuf: vec![0.0; (sr * 0.3) as usize], // 300 ms
            eidx: 0,
        }
    }

    fn snapshot(&self) -> Settings {
        let p = &self.params;
        let ratio = 2f32.powf(f(&p.semitones) / 12.0);
        Settings {
            mode: p.mode.load(Relaxed),
            pitch_step: (1.0 - ratio) / self.pwindow,
            robot_inc: 2.0 * PI * f(&p.robot_hz) / self.sr,
            echo_mix: f(&p.echo_mix),
            echo_fb: f(&p.echo_fb),
            gain: f(&p.gain),
            muted: p.muted.load(Relaxed),
        }
    }

    fn read_pitch(&self, delay: f32) -> f32 {
        let len = self.pbuf.len();
        let pos = (self.pwrite as f32 - 1.0 - delay).rem_euclid(len as f32);
        let i0 = pos.floor() as usize % len;
        let i1 = (i0 + 1) % len;
        let frac = pos - pos.floor();
        self.pbuf[i0] * (1.0 - frac) + self.pbuf[i1] * frac
    }

    fn process(&mut self, x: f32, s: &Settings) -> f32 {
        // Always keep the pitch delay line fed so switching modes is click-free.
        self.pbuf[self.pwrite] = x;
        self.pwrite = (self.pwrite + 1) % self.pbuf.len();

        let y = match s.mode {
            MODE_PITCH => {
                self.pphase = (self.pphase + s.pitch_step).rem_euclid(1.0);
                let pb = (self.pphase + 0.5).rem_euclid(1.0);
                let wa = (PI * self.pphase).sin().powi(2);
                let wb = (PI * pb).sin().powi(2);
                self.read_pitch(self.pphase * self.pwindow) * wa
                    + self.read_pitch(pb * self.pwindow) * wb
            }
            MODE_ROBOT => {
                self.rphase = (self.rphase + s.robot_inc) % (2.0 * PI);
                x * self.rphase.sin()
            }
            MODE_ECHO => {
                let delayed = self.ebuf[self.eidx];
                self.ebuf[self.eidx] = x + delayed * s.echo_fb;
                self.eidx = (self.eidx + 1) % self.ebuf.len();
                x + delayed * s.echo_mix
            }
            _ => x,
        };

        if s.muted {
            0.0
        } else {
            (y * s.gain).clamp(-1.0, 1.0)
        }
    }
}

// ---------------------------------------------------------------------------
// Audio engine (owns the cpal streams; dropping it stops audio)
// ---------------------------------------------------------------------------

struct Engine {
    _input: Stream,
    _output: Stream,
    sample_rate: u32,
}

fn device_names(input: bool) -> Vec<String> {
    let host = cpal::default_host();
    let devices: Vec<Device> = if input {
        host.input_devices().map(|d| d.collect()).unwrap_or_default()
    } else {
        host.output_devices().map(|d| d.collect()).unwrap_or_default()
    };
    devices.iter().filter_map(|d| d.name().ok()).collect()
}

fn find_device(name: &str, input: bool) -> Result<Device, String> {
    let host = cpal::default_host();
    if name.is_empty() {
        return if input { host.default_input_device() } else { host.default_output_device() }
            .ok_or_else(|| "no default audio device found".to_string());
    }
    let devices: Vec<Device> = if input {
        host.input_devices().map(|d| d.collect()).unwrap_or_default()
    } else {
        host.output_devices().map(|d| d.collect()).unwrap_or_default()
    };
    devices
        .into_iter()
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        .ok_or_else(|| format!("device '{name}' not found"))
}

impl Engine {
    fn start(in_name: &str, out_name: &str, params: Arc<Params>) -> Result<Engine, String> {
        let input = find_device(in_name, true)?;
        let output = find_device(out_name, false)?;

        let in_cfg = input.default_input_config().map_err(|e| e.to_string())?;
        let out_cfg = output.default_output_config().map_err(|e| e.to_string())?;
        if in_cfg.sample_format() != SampleFormat::F32 || out_cfg.sample_format() != SampleFormat::F32 {
            return Err("only f32 audio devices are supported".into());
        }

        let sample_rate = in_cfg.sample_rate();
        let in_ch = in_cfg.channels() as usize;
        let out_ch = out_cfg.channels() as usize;

        let in_stream_cfg: StreamConfig = in_cfg.into();
        let out_stream_cfg = StreamConfig {
            channels: out_cfg.channels(),
            sample_rate,
            buffer_size: BufferSize::Default,
        };

        // Lock-free queue of processed mono samples, input callback -> output callback.
        let rb = HeapRb::<f32>::new(sample_rate.0 as usize / 2);
        let (mut prod, mut cons) = rb.split();
        for _ in 0..(sample_rate.0 as usize / 50) {
            let _ = prod.try_push(0.0); // ~20 ms head start against underruns
        }

        let mut proc = Processor::new(params.clone(), sample_rate.0 as f32);
        let p_in = params.clone();
        let input_stream = input
            .build_input_stream(
                &in_stream_cfg,
                move |data: &[f32], _| {
                    let s = proc.snapshot();
                    let mut peak = 0.0f32;
                    for frame in data.chunks(in_ch) {
                        peak = peak.max(frame[0].abs());
                        let _ = prod.try_push(proc.process(frame[0], &s));
                    }
                    p_in.in_peak.fetch_max(peak.to_bits(), Relaxed);
                },
                |e| eprintln!("input error: {e}"),
                None,
            )
            .map_err(|e| format!("input: {e}"))?;

        let p_out = params;
        let output_stream = output
            .build_output_stream(
                &out_stream_cfg,
                move |data: &mut [f32], _| {
                    let mut peak = 0.0f32;
                    for frame in data.chunks_mut(out_ch) {
                        let s = cons.try_pop().unwrap_or(0.0);
                        peak = peak.max(s.abs());
                        frame.iter_mut().for_each(|o| *o = s);
                    }
                    p_out.out_peak.fetch_max(peak.to_bits(), Relaxed);
                },
                |e| eprintln!("output error: {e}"),
                None,
            )
            .map_err(|e| format!("output (sample rate {} Hz may be unsupported): {e}", sample_rate.0))?;

        input_stream.play().map_err(|e| e.to_string())?;
        output_stream.play().map_err(|e| e.to_string())?;

        Ok(Engine { _input: input_stream, _output: output_stream, sample_rate: sample_rate.0 })
    }
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

struct App {
    params: Arc<Params>,
    engine: Option<Engine>,
    error: Option<String>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    in_sel: String, // "" = system default
    out_sel: String,
    mode: u8,
    semitones: f32,
    robot_hz: f32,
    echo_mix: f32,
    echo_fb: f32,
    gain: f32,
    muted: bool,
    in_level: f32,
    out_level: f32,
}

impl App {
    fn new() -> Self {
        let mut app = Self {
            params: Arc::new(Params::new()),
            engine: None,
            error: None,
            inputs: device_names(true),
            outputs: device_names(false),
            in_sel: String::new(),
            out_sel: String::new(),
            mode: MODE_PITCH,
            semitones: 4.0,
            robot_hz: 50.0,
            echo_mix: 0.5,
            echo_fb: 0.45,
            gain: 1.0,
            muted: false,
            in_level: 0.0,
            out_level: 0.0,
        };
        app.restart_engine();
        app
    }

    fn restart_engine(&mut self) {
        self.engine = None; // drop old streams first so devices are released
        match Engine::start(&self.in_sel, &self.out_sel, self.params.clone()) {
            Ok(e) => {
                self.engine = Some(e);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }
}

fn device_combo(ui: &mut egui::Ui, id: &str, names: &[String], sel: &mut String) -> bool {
    let before = sel.clone();
    let text = if sel.is_empty() { "System default".to_string() } else { sel.clone() };
    egui::ComboBox::from_id_source(id)
        .width(280.0)
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(sel, String::new(), "System default");
            for n in names {
                ui.selectable_value(sel, n.clone(), n);
            }
        });
    *sel != before
}

fn meter(ui: &mut egui::Ui, label: &str, level: f32) {
    ui.horizontal(|ui| {
        ui.label(format!("{label:<4}"));
        ui.add(egui::ProgressBar::new(level.sqrt().min(1.0)).desired_width(300.0));
    });
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut restart = false;

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Voice Changer");
            ui.label(egui::RichText::new("Use headphones to avoid feedback.").weak());
            ui.separator();

            egui::Grid::new("devices").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Microphone");
                restart |= device_combo(ui, "in", &self.inputs, &mut self.in_sel);
                ui.end_row();
                ui.label("Output");
                restart |= device_combo(ui, "out", &self.outputs, &mut self.out_sel);
                ui.end_row();
            });
            if ui.small_button("Refresh devices").clicked() {
                self.inputs = device_names(true);
                self.outputs = device_names(false);
            }

            ui.separator();
            ui.label("Effect");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mode, MODE_NORMAL, "Normal");
                ui.selectable_value(&mut self.mode, MODE_PITCH, "Pitch");
                ui.selectable_value(&mut self.mode, MODE_ROBOT, "Robot");
                ui.selectable_value(&mut self.mode, MODE_ECHO, "Echo");
            });
            ui.add_space(4.0);

            match self.mode {
                MODE_PITCH => {
                    ui.add(
                        egui::Slider::new(&mut self.semitones, -12.0..=12.0)
                            .text("semitones")
                            .step_by(0.5),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Deep (-6)").clicked() {
                            self.semitones = -6.0;
                        }
                        if ui.button("Normal (0)").clicked() {
                            self.semitones = 0.0;
                        }
                        if ui.button("Chipmunk (+7)").clicked() {
                            self.semitones = 7.0;
                        }
                    });
                }
                MODE_ROBOT => {
                    ui.add(egui::Slider::new(&mut self.robot_hz, 10.0..=200.0).text("mod freq (Hz)"));
                }
                MODE_ECHO => {
                    ui.add(egui::Slider::new(&mut self.echo_mix, 0.0..=1.0).text("mix"));
                    ui.add(egui::Slider::new(&mut self.echo_fb, 0.0..=0.9).text("feedback"));
                }
                _ => {
                    ui.label(egui::RichText::new("Passing your voice through unchanged.").weak());
                }
            }

            ui.separator();
            ui.add(egui::Slider::new(&mut self.gain, 0.0..=2.0).text("output gain"));
            ui.checkbox(&mut self.muted, "Mute");

            ui.separator();
            meter(ui, "In", self.in_level);
            meter(ui, "Out", self.out_level);

            ui.add_space(6.0);
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("Error: {err}"));
                if ui.button("Retry").clicked() {
                    restart = true;
                }
            } else if let Some(e) = &self.engine {
                ui.label(egui::RichText::new(format!("Running at {} Hz", e.sample_rate)).weak());
            }
        });

        // Push UI state to the audio thread.
        let p = &self.params;
        p.mode.store(self.mode, Relaxed);
        set(&p.semitones, self.semitones);
        set(&p.robot_hz, self.robot_hz);
        set(&p.echo_mix, self.echo_mix);
        set(&p.echo_fb, self.echo_fb);
        set(&p.gain, self.gain);
        p.muted.store(self.muted, Relaxed);

        // Pull meter levels (peak-hold since last frame, with a smooth fall-off).
        let ip = f32::from_bits(p.in_peak.swap(0, Relaxed));
        let op = f32::from_bits(p.out_peak.swap(0, Relaxed));
        self.in_level = ip.max(self.in_level * 0.85);
        self.out_level = op.max(self.out_level * 0.85);

        if restart {
            self.restart_engine();
        }

        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([460.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("Voice Changer", options, Box::new(|_cc| Box::new(App::new())))
}
