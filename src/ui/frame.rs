//! The per-frame loop (`eframe::App::update`): header, tabs, meters, autosave.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::audio::device_names;
use crate::config::save_config;
use crate::soundboard::Status;

use super::widgets::*;
use super::{App, Tab};

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
        let mut clear_notice = false;

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                slider_inline(ui, &mut self.out_gain, 1.0, 0.0..=3.0, "mic output gain", "", false, 0.0, false);
                ui.checkbox(&mut self.muted, "Mute mic");
                ui.checkbox(&mut self.limiter, "Soft limiter")
                    .on_hover_text("Smoothly rounds off peaks near full volume instead of hard clipping them (clipping sounds harsh).");
            });
            meter(ui, "In", self.in_level);
            meter(ui, "Out", self.out_level);
            if self.clip_frames >= 6 {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "The output is pinned at full volume (loud and distorted). Lower input gain, distortion or output gain.",
                );
            }
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("Error: {err}"));
                if ui.button("Retry").clicked() {
                    restart = true;
                }
            } else if let Some(e) = &self.engine {
                ui.label(egui::RichText::new(format!("Running at {} Hz", e.sample_rate)).weak());
                if let Some(w) = &e.warning {
                    ui.colored_label(egui::Color32::YELLOW, w);
                }
            }
            if let Some(n) = &self.notice {
                ui.horizontal_wrapped(|ui| {
                    ui.label(n.as_str());
                    if ui.small_button("OK").clicked() {
                        clear_notice = true;
                    }
                });
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
                    ui.selectable_value(&mut self.tab, Tab::Custom, "Voice effects");
                    ui.selectable_value(&mut self.tab, Tab::Soundboard, "Soundboard");
                });
                ui.separator();

                match self.tab {
                    Tab::Custom => self.ui_custom(ui),
                    Tab::Soundboard => self.ui_soundboard(ui),
                }

                ui.add_space(8.0);
                egui::CollapsingHeader::new("Backup & restore").show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Save backup...").clicked() {
                            self.save_backup();
                        }
                        if ui.button("Restore backup...").clicked() {
                            self.load_backup();
                        }
                    });
                    hint(
                        ui,
                        "A backup holds all settings, presets, folders and your sound list. Sounds are saved as \
                         file locations, so on another PC they only work if the files are in the same place. \
                         To share just a voice, use Export preset on the Voice effects tab.",
                    );
                });
            });
        });

        if clear_notice {
            self.notice = None;
        }

        // Push the active voice settings to the audio thread (only when changed).
        let mut s = self.custom;
        s.ns_amount = self.ns_amount;
        s.out_gain = self.out_gain;
        s.muted = self.muted;
        s.sb_vol = self.sb_vol;
        s.mon_vol = self.mon_vol;
        s.limiter = self.limiter;
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
        if op >= 0.97 {
            self.clip_frames = (self.clip_frames + 1).min(1000);
        } else if op < 0.9 {
            self.clip_frames = 0;
        }

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
