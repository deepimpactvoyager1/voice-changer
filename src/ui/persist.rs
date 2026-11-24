//! Saving/loading the config, preset export/import and backups.

use std::path::PathBuf;

use crate::settings::Settings;
use crate::soundboard::{clean_name, SoundSpec};

use super::App;

impl App {
    pub(super) fn config_text(&self) -> String {
        let mut t = String::from("version=2\n");
        t.push_str(&format!("in_sel={}\n", self.in_sel));
        t.push_str(&format!("out_sel={}\n", self.out_sel));
        t.push_str(&format!("mon_sel={}\n", self.mon_sel));
        t.push_str(&format!("ns_amount={}\n", self.ns_amount));
        t.push_str(&format!("out_gain={}\n", self.out_gain));
        t.push_str(&format!("muted={}\n", if self.muted { 1 } else { 0 }));
        t.push_str(&format!("sb_vol={}\n", self.sb_vol));
        t.push_str(&format!("mon_vol={}\n", self.mon_vol));
        t.push_str(&format!("limiter={}\n", if self.limiter { 1 } else { 0 }));
        t.push_str(&format!("custom={}\n", self.custom.serialize()));
        for (name, data) in &self.presets {
            t.push_str(&format!("preset={}\t{}\n", name, data));
        }
        for g in &self.groups {
            t.push_str(&format!("group={}\n", g));
        }
        for s in &self.sounds {
            t.push_str(&format!(
                "sound={}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                s.path.to_string_lossy(),
                s.volume,
                if s.looping { 1 } else { 0 },
                s.speed,
                s.trim_start,
                s.trim_end,
                if s.vc { 1 } else { 0 },
                s.group
            ));
        }
        self.hk.write_lines(&mut t);
        t.push_str(&format!("tray_hide={}\n", if self.hide_to_tray { 1 } else { 0 }));
        t
    }

    /// Reads a saved config (or backup); returns the sounds to (re)load.
    pub(super) fn load_config(&mut self, text: &str) -> Vec<SoundSpec> {
        let num = |v: &str| v.trim().parse::<f32>().ok().filter(|n| n.is_finite());
        let mut sounds: Vec<SoundSpec> = Vec::new();
        let mut version = 1;
        for line in text.lines() {
            let Some((key, val)) = line.split_once('=') else { continue };
            match key {
                "version" => version = val.trim().parse::<u32>().unwrap_or(1),
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
                "limiter" => self.limiter = val.trim() != "0",
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
                "group" => {
                    let g = clean_name(val);
                    if !g.is_empty() && !self.groups.contains(&g) {
                        self.groups.push(g);
                    }
                }
                "sound" => {
                    let mut it = val.split('\t');
                    let path = it.next().unwrap_or("");
                    if path.is_empty() {
                        continue;
                    }
                    let mut spec = SoundSpec::new(PathBuf::from(path), "");
                    spec.volume = it.next().and_then(num).unwrap_or(1.0).clamp(0.0, 2.0);
                    spec.looping = it.next().map(|v| v.trim() == "1").unwrap_or(false);
                    spec.speed = it.next().and_then(num).unwrap_or(1.0).clamp(0.5, 2.0);
                    spec.trim_start = it.next().and_then(num).unwrap_or(0.0).max(0.0);
                    spec.trim_end = it.next().and_then(num).unwrap_or(0.0).max(0.0);
                    spec.vc = it.next().map(|v| v.trim() == "1").unwrap_or(false);
                    spec.group = clean_name(it.next().unwrap_or(""));
                    if !spec.group.is_empty() && !self.groups.contains(&spec.group) {
                        self.groups.push(spec.group.clone());
                    }
                    sounds.push(spec);
                }
                "tray_hide" => self.hide_to_tray = val.trim() != "0",
                k if k.starts_with("hk_") => {
                    self.hk.parse_line(k, val);
                }
                _ => {}
            }
        }
        // Older saves had the noise gate switched off; the new standard is -45 dB.
        if version < 2 && self.custom.gate_db <= -79.0 {
            self.custom.gate_db = -45.0;
        }
        sounds
    }

    // ----- export / import -----

    pub(super) fn export_preset(&mut self) {
        let name = clean_name(&self.preset_name);
        let name = if name.is_empty() { "My preset".to_string() } else { name };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Voice Changer preset", &["vcpreset"])
            .set_file_name(format!("{name}.vcpreset"))
            .save_file()
        else {
            return;
        };
        let text = format!("VoiceChangerPreset=1\nname={}\nsettings={}\n", name, self.custom.serialize());
        self.notice = Some(match std::fs::write(&path, text) {
            Ok(_) => format!("Preset exported to {}", path.display()),
            Err(e) => format!("Could not save the preset: {e}"),
        });
    }

    pub(super) fn import_preset(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Voice Changer preset", &["vcpreset"])
            .pick_file()
        else {
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.notice = Some(format!("Could not read the file: {e}"));
                return;
            }
        };
        let mut name = String::new();
        let mut data: Option<String> = None;
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                match k {
                    "name" => name = clean_name(v),
                    "settings" => data = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        let Some(data) = data else {
            self.notice = Some("That file is not a Voice Changer preset.".into());
            return;
        };
        if name.is_empty() {
            name = path
                .file_stem()
                .map(|s| clean_name(&s.to_string_lossy()))
                .unwrap_or_default();
        }
        if name.is_empty() {
            name = "Imported preset".into();
        }
        let mut s = Settings::neutral();
        s.apply(&data);
        self.custom = s;
        let stored = self.custom.serialize();
        if let Some(p) = self.presets.iter_mut().find(|p| p.0 == name) {
            p.1 = stored;
        } else {
            self.presets.push((name.clone(), stored));
        }
        self.notice = Some(format!("Imported preset \"{name}\" and applied it."));
    }

    pub(super) fn save_backup(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Voice Changer backup", &["vcbackup"])
            .set_file_name("voice_changer_backup.vcbackup")
            .save_file()
        else {
            return;
        };
        self.notice = Some(match std::fs::write(&path, self.config_text()) {
            Ok(_) => format!("Backup saved to {}", path.display()),
            Err(e) => format!("Could not save the backup: {e}"),
        });
    }

    pub(super) fn load_backup(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Voice Changer backup", &["vcbackup"])
            .pick_file()
        else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) if text.lines().any(|l| l.starts_with("version=")) => {
                self.stop_sounds();
                self.sounds.clear();
                self.presets.clear();
                self.groups.clear();
                self.hk = Default::default();
                let specs = self.load_config(&text);
                self.start_with_fallback();
                self.add_sounds(specs);
                self.notice = Some("Backup restored.".into());
            }
            Ok(_) => self.notice = Some("That file is not a Voice Changer backup.".into()),
            Err(e) => self.notice = Some(format!("Could not read the file: {e}")),
        }
    }
}
