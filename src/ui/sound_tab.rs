//! The "soundboard" tab: sound rows and the folder/groups list.

use eframe::egui;

use crate::soundboard::{clean_name, SoundSpec, Status};

use super::widgets::*;
use super::{App, RowActions};

impl App {
    fn sound_row(&mut self, ui: &mut egui::Ui, i: usize, act: &mut RowActions) {
        let name = self.sounds[i].name.clone();
        let status = self.sounds[i].status.clone();
        let dur = self.sounds[i].duration();
        let id = self.sounds[i].id;

        ui.horizontal(|ui| {
            match &status {
                Status::Ready => {
                    if ui.add(big_btn!("PLAY")).clicked() {
                        self.play_sound(i);
                    }
                }
                Status::Loading => {
                    ui.add_enabled(false, big_btn!("PLAY"));
                }
                Status::Failed(_) => {
                    if ui.add(big_btn!("Retry")).clicked() {
                        act.retry = Some(i);
                    }
                }
            }
            ui.vertical(|ui| {
                match &status {
                    Status::Ready => {
                        ui.label(egui::RichText::new(format!("{name}  ({dur:.1}s)")).strong());
                    }
                    Status::Loading => {
                        ui.label(egui::RichText::new(format!("{name}  (loading...)")).weak());
                    }
                    Status::Failed(e) => {
                        ui.colored_label(egui::Color32::LIGHT_RED, format!("{name}: {e}"));
                    }
                }
                ui.horizontal(|ui| {
                    let cur = self.sounds[i].group.clone();
                    let mut sel = cur.clone();
                    let shown = if cur.is_empty() { "Unsorted".to_string() } else { cur.clone() };
                    egui::ComboBox::from_id_source(("folder", id)).selected_text(shown).show_ui(ui, |ui| {
                        ui.selectable_value(&mut sel, String::new(), "Unsorted");
                        for g in &self.groups {
                            ui.selectable_value(&mut sel, g.clone(), g.as_str());
                        }
                    });
                    if sel != cur {
                        self.sounds[i].group = sel;
                    }
                    if ui.small_button("Remove").on_hover_text("Remove from the soundboard").clicked() {
                        act.remove = Some(i);
                    }
                });
            });
        });

        if status == Status::Ready {
            egui::CollapsingHeader::new("options: volume, speed, loop, trim, voice changer")
                .id_source(("sound_opts", id))
                .show(ui, |ui| {
                    let snd = &mut self.sounds[i];
                    ui.horizontal_wrapped(|ui| {
                        slider_inline(ui, &mut snd.volume, 1.0, 0.0..=2.0, "volume", "", false, 0.0, true);
                    });
                    ui.horizontal_wrapped(|ui| {
                        slider_inline(ui, &mut snd.speed, 1.0, 0.5..=2.0, "speed", "x", false, 0.0, true);
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut snd.looping, "loop");
                        ui.checkbox(&mut snd.vc, "play through the voice changer")
                            .on_hover_text("The sound is mixed into your voice before the effects, so pitch, echo, reverb and so on also change it.");
                    });
                    let dur = snd.duration().max(0.1);
                    let mut start = snd.trim_start.clamp(0.0, dur);
                    let mut end = if snd.trim_end > 0.0 { snd.trim_end.min(dur) } else { dur };
                    ui.horizontal_wrapped(|ui| {
                        slider_inline(ui, &mut start, 0.0, 0.0..=dur, "trim start", " s", false, 0.0, true);
                    });
                    ui.horizontal_wrapped(|ui| {
                        slider_inline(ui, &mut end, dur, 0.0..=dur, "trim end", " s", false, 0.0, true);
                    });
                    if end < start + 0.05 {
                        end = (start + 0.05).min(dur);
                        if end < start + 0.05 {
                            start = (end - 0.05).max(0.0);
                        }
                    }
                    snd.trim_start = start;
                    snd.trim_end = if end >= dur - 0.01 { 0.0 } else { end };
                });
        }
        ui.separator();
    }

    pub(super) fn ui_soundboard(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add sound files...").clicked() {
                if let Some(paths) = rfd::FileDialog::new()
                    .add_filter("Audio", &["mp3", "wav", "ogg", "flac"])
                    .pick_files()
                {
                    let specs: Vec<SoundSpec> = paths.into_iter().map(|p| SoundSpec::new(p, "")).collect();
                    self.add_sounds(specs);
                }
            }
            if ui.button("Add folder...").clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.import_folder(dir);
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
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_group).hint_text("new folder name").desired_width(150.0));
            if ui.button("Create folder").clicked() {
                let g = clean_name(&self.new_group);
                if !g.is_empty() && !self.groups.contains(&g) {
                    self.groups.push(g);
                }
                self.new_group.clear();
            }
        });
        sl_lbl(ui, &mut self.sb_vol, 1.0, 0.0..=2.0, "Soundboard volume", "", false, 0.0);
        hint(ui, "Your sound list is saved automatically and comes back next time. Keep the files in the same place. Use each sound's folder menu to sort sounds into folders.");
        ui.separator();

        if self.sounds.is_empty() {
            ui.label(egui::RichText::new("No sounds yet. Add mp3, wav, ogg or flac files, or a whole folder.").weak());
        }

        let mut act = RowActions::default();
        let mut del_group: Option<String> = None;
        let has_groups = !self.groups.is_empty();
        let mut names: Vec<String> = vec![String::new()];
        names.extend(self.groups.iter().cloned());
        for gname in names {
            let count = self.sounds.iter().filter(|s| s.group == gname).count();
            if gname.is_empty() {
                if count == 0 {
                    continue;
                }
                if has_groups {
                    egui::CollapsingHeader::new(format!("Unsorted ({count})"))
                        .id_source("grp_unsorted")
                        .default_open(true)
                        .show(ui, |ui| {
                            for i in 0..self.sounds.len() {
                                if self.sounds[i].group.is_empty() {
                                    self.sound_row(ui, i, &mut act);
                                }
                            }
                        });
                } else {
                    for i in 0..self.sounds.len() {
                        self.sound_row(ui, i, &mut act);
                    }
                }
            } else {
                egui::CollapsingHeader::new(format!("{gname} ({count})"))
                    .id_source(("grp", gname.clone()))
                    .default_open(true)
                    .show(ui, |ui| {
                        for i in 0..self.sounds.len() {
                            if self.sounds[i].group == gname {
                                self.sound_row(ui, i, &mut act);
                            }
                        }
                        if ui
                            .small_button("Delete this folder")
                            .on_hover_text("Its sounds move to Unsorted; no files are deleted")
                            .clicked()
                        {
                            del_group = Some(gname.clone());
                        }
                    });
            }
        }

        if let Some(i) = act.retry {
            self.sounds[i].status = Status::Loading;
            let job = (self.sounds[i].id, self.sounds[i].path.clone());
            self.spawn_decode(vec![job]);
        }
        if let Some(i) = act.remove {
            self.sounds.remove(i);
        }
        if let Some(g) = del_group {
            self.groups.retain(|x| *x != g);
            for s in self.sounds.iter_mut() {
                if s.group == g {
                    s.group.clear();
                }
            }
        }
    }
}
