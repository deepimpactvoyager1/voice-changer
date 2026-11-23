//! The "voice" tab: every effect slider.

use std::sync::atomic::Ordering;

fn note_text(v: u32) -> String {
    if v == 0 {
        return "no voice".to_string();
    }
    const N: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let note = (v >> 8) as i32 - 1;
    let cents = (v & 0xFF) as u8 as i8;
    format!("{}{} {:+} cents", N[(note % 12) as usize], note / 12 - 1, cents)
}

use eframe::egui;

use crate::settings::Settings;
use crate::soundboard::clean_name;

use super::widgets::*;
use super::App;

impl App {
    pub(super) fn ui_custom(&mut self, ui: &mut egui::Ui) {
        ui.label("Every effect runs at the same time. Each slider's rest position means \"off\". Click the round arrow next to a slider to reset it.");
        ui.horizontal_wrapped(|ui| {
            ui.label("Presets:");
            let c = Settings::neutral();
            if ui.button("Normal").clicked() {
                self.custom = c;
            }
            if ui.button("Deep").clicked() {
                self.custom = Settings { semitones: -6.0, ..c };
            }
            if ui.button("Chipmunk").clicked() {
                self.custom = Settings { semitones: 7.0, ..c };
            }
            if ui.button("Robot").clicked() {
                self.custom = Settings { robot_amount: 1.0, robot_hz: 50.0, ..c };
            }
            if ui.button("Echo").clicked() {
                self.custom = Settings { echo_mix: 0.5, echo_fb: 0.45, ..c };
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
            if ui.button("Megaphone").clicked() {
                self.custom = Settings {
                    hpf_hz: 400.0,
                    lpf_hz: 3500.0,
                    drive: 0.35,
                    dist_type: 1,
                    comp_ratio: 6.0,
                    comp_thresh_db: -24.0,
                    comp_makeup_db: 6.0,
                    eq_mid_db: 5.0,
                    ..c
                };
            }
            if ui.button("Underwater").clicked() {
                self.custom = Settings {
                    lpf_hz: 1200.0,
                    vib_depth: 0.5,
                    vib_hz: 3.0,
                    chorus_mix: 0.5,
                    reverb_mix: 0.3,
                    reverb_size: 0.6,
                    ..c
                };
            }
            if ui.button("Demon").clicked() {
                self.custom = Settings {
                    semitones: -7.0,
                    drive: 0.2,
                    echo_mix: 0.25,
                    echo_ms: 120.0,
                    lpf_hz: 6000.0,
                    growl_depth: 0.3,
                    ..c
                };
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
            if ui.button("Singer").clicked() {
                self.custom = Settings {
                    gate_db: -58.0,
                    hpf_hz: 80.0,
                    eq_low_db: 1.5,
                    eq_high_db: 2.5,
                    comp_thresh_db: -20.0,
                    comp_ratio: 3.0,
                    comp_makeup_db: 4.0,
                    deess: 0.4,
                    at_amount: 0.5,
                    at_speed_ms: 80.0,
                    at_humanize: 0.4,
                    chorus_mix: 0.15,
                    chorus_hz: 0.6,
                    chorus_depth: 0.4,
                    dbl_mix: 0.25,
                    dbl_ms: 18.0,
                    reverb_mix: 0.25,
                    reverb_size: 0.6,
                    ..c
                };
            }
            if ui.button("Robot singer").clicked() {
                self.custom = Settings {
                    gate_db: -58.0,
                    hpf_hz: 80.0,
                    comp_thresh_db: -20.0,
                    comp_ratio: 3.0,
                    comp_makeup_db: 4.0,
                    deess: 0.3,
                    at_amount: 1.0,
                    at_speed_ms: 0.0,
                    at_humanize: 0.0,
                    reverb_mix: 0.2,
                    reverb_size: 0.5,
                    ..c
                };
            }
        });

        // user presets
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.preset_name).hint_text("preset name").desired_width(150.0));
            if ui.button("Save as preset").clicked() {
                let name = clean_name(&self.preset_name);
                if !name.is_empty() {
                    let data = self.custom.serialize();
                    if let Some(p) = self.presets.iter_mut().find(|p| p.0 == name) {
                        p.1 = data;
                    } else {
                        self.presets.push((name, data));
                    }
                }
            }
            if ui.button("Export preset...").clicked() {
                self.export_preset();
            }
            if ui.button("Import preset...").clicked() {
                self.import_preset();
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

        let note = self.params.at_note.load(Ordering::Relaxed);
        let s = &mut self.custom;
        egui::CollapsingHeader::new("Input & cleanup").default_open(true).show(ui, |ui| {
            sl(ui, &mut s.in_gain_db, 0.0, -20.0..=30.0, "input gain", " dB", false, 0.0);
            sl(ui, &mut s.gate_db, -45.0, -80.0..=-20.0, "noise gate", " dB", false, 0.0);
            sl(ui, &mut s.gate_atk_ms, 2.0, 0.5..=50.0, "gate attack", " ms", true, 0.0);
            sl(ui, &mut s.gate_rel_ms, 80.0, 10.0..=500.0, "gate release", " ms", true, 0.0);
            sl(ui, &mut s.hpf_hz, 20.0, 20.0..=600.0, "high-pass", " Hz", true, 0.0);
            hint(ui, "Gate: -80 = off, -45 is the standard. High-pass: 20 = off. Slower release = smoother word endings.");
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
            sl(ui, &mut s.growl_depth, 0.0, 0.0..=1.0, "growl", "", false, 0.0);
            sl(ui, &mut s.growl_hz, 40.0, 10.0..=120.0, "growl speed", " Hz", false, 0.0);
            ui.separator();
            sl(ui, &mut s.harm1_mix, 0.0, 0.0..=1.0, "harmony 1 volume", "", false, 0.0);
            sl(ui, &mut s.harm1_semi, 4.0, -24.0..=24.0, "harmony 1 interval", " st", false, 1.0);
            sl(ui, &mut s.harm2_mix, 0.0, 0.0..=1.0, "harmony 2 volume", "", false, 0.0);
            sl(ui, &mut s.harm2_semi, 7.0, -24.0..=24.0, "harmony 2 interval", " st", false, 1.0);
            hint(ui, "Harmonizer: extra voices above or below yours. +4 and +7 make a major chord, -12 is an octave down.");
        });
        egui::CollapsingHeader::new("Autotune & formant").show(ui, |ui| {
            sl(ui, &mut s.at_amount, 0.0, 0.0..=1.0, "autotune amount", "", false, 0.0);
            sl(ui, &mut s.at_speed_ms, 40.0, 0.0..=400.0, "retune speed", " ms", false, 0.0);
            sl(ui, &mut s.at_humanize, 0.0, 0.0..=1.0, "humanize (keep vibrato)", "", false, 0.0);
            combo_u8(ui, "at_key", "key", &mut s.at_key, &["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]);
            combo_u8(ui, "at_scale", "scale", &mut s.at_scale, &["Chromatic", "Major", "Minor", "Major pentatonic", "Minor pentatonic", "Blues"]);
            if s.at_amount > 0.01 {
                ui.label(format!("You are singing: {}", note_text(note)));
            }
            hint(ui, "Amount 0 = off. Retune speed 0 = hard robotic snap, 400 = slow natural correction. Humanize keeps your vibrato.");
            ui.separator();
            sl(ui, &mut s.fm_shift, 0.0, -12.0..=12.0, "formant (voice size)", " st", false, 0.0);
            hint(ui, "Changes the voice's size and character without changing the note: + is smaller/brighter, - is bigger/deeper. Pitch (above) moves the note itself.");
        });
        egui::CollapsingHeader::new("Modulation (vibrato, tremolo, chorus, doubler)").show(ui, |ui| {
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
            ui.separator();
            sl(ui, &mut s.dbl_mix, 0.0, 0.0..=1.0, "doubler mix", "", false, 0.0);
            sl(ui, &mut s.dbl_ms, 20.0, 10.0..=40.0, "doubler spread", " ms", false, 0.0);
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
}
