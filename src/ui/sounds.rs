//! Soundboard actions on `App`: decoding, adding, importing, playing.

use std::path::PathBuf;
use std::sync::Arc;

use crate::settings::Cmd;
use crate::soundboard::{clean_name, decode_file, is_audio, Sound, SoundSpec, Status};

use super::App;

impl App {
    pub(super) fn spawn_decode(&self, jobs: Vec<(usize, PathBuf)>) {
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
                let res = decode_file(&path, target);
                let _ = tx.send((id, res));
                ctx.request_repaint();
            }
        });
    }

    pub(super) fn add_sounds(&mut self, specs: Vec<SoundSpec>) {
        let rate = self.engine.as_ref().map(|e| e.sample_rate).unwrap_or(48000);
        let mut jobs = Vec::new();
        for spec in specs {
            let id = self.next_id;
            self.next_id += 1;
            let name = spec
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "sound".into());
            self.sounds.push(Sound {
                id,
                path: spec.path.clone(),
                name,
                data: Arc::new(Vec::new()),
                rate,
                volume: spec.volume,
                speed: spec.speed,
                looping: spec.looping,
                trim_start: spec.trim_start,
                trim_end: spec.trim_end,
                vc: spec.vc,
                group: spec.group,
                status: Status::Loading,
            });
            jobs.push((id, spec.path));
        }
        self.spawn_decode(jobs);
    }

    /// Adds every mp3/wav/ogg/flac in a folder (not its sub-folders) as a new group.
    pub(super) fn import_folder(&mut self, dir: PathBuf) {
        let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && is_audio(p))
                .collect(),
            Err(e) => {
                self.notice = Some(format!("Could not read the folder: {e}"));
                return;
            }
        };
        files.sort();
        if files.is_empty() {
            self.notice = Some("No mp3, wav, ogg or flac files in that folder.".into());
            return;
        }
        let mut group = dir
            .file_name()
            .map(|n| clean_name(&n.to_string_lossy()))
            .unwrap_or_default();
        if group.is_empty() {
            group = "Folder".into();
        }
        if !self.groups.contains(&group) {
            self.groups.push(group.clone());
        }
        let specs: Vec<SoundSpec> = files.into_iter().map(|p| SoundSpec::new(p, &group)).collect();
        self.add_sounds(specs);
    }

    pub(super) fn build_play(&mut self, i: usize) -> Option<Cmd> {
        let e = self.engine.as_ref()?;
        let snd = &mut self.sounds[i];
        if snd.status != Status::Ready || snd.data.is_empty() {
            return None;
        }
        snd.convert_to(e.sample_rate);
        let rate = snd.rate as f64;
        let len = snd.data.len() as f64;
        let start = (snd.trim_start.max(0.0) as f64 * rate).min(len);
        let end = if snd.trim_end > 0.0 { (snd.trim_end as f64 * rate).min(len) } else { len };
        Some(Cmd::Play {
            data: snd.data.clone(),
            gain: snd.volume,
            step: snd.speed as f64,
            looping: snd.looping,
            start,
            end,
            vc: snd.vc,
        })
    }

    pub(super) fn play_sound(&mut self, i: usize) {
        if let Some(c) = self.build_play(i) {
            if let Some(e) = &self.engine {
                let _ = e.tx.send(c);
            }
        }
    }

    pub(super) fn sound_key(&self, i: usize) -> String {
        self.sounds[i].path.to_string_lossy().into_owned()
    }

    /// Re-registers all global hotkeys (call when `hk_dirty`).
    pub(super) fn sync_hotkeys(&mut self) {
        use crate::hotkeys::{send_cmd, Binding};
        self.hk_dirty = false;
        let mut b: Vec<Binding> = Vec::new();
        let tx = self.shared_tx.clone();
        b.push(Binding { slot: "stop".into(), spec: self.hk.stop.clone(), action: Arc::new(move || send_cmd(&tx, Cmd::StopAll)) });
        let p = self.params.clone();
        b.push(Binding { slot: "mute".into(), spec: self.hk.mute.clone(), action: Arc::new(move || p.toggle_mute()) });
        for i in 0..self.sounds.len() {
            let key = self.sound_key(i);
            let Some(spec) = self.hk.sounds.get(&key).filter(|s| s.is_set()).cloned() else { continue };
            let Some(cmd) = self.build_play(i) else { continue };
            let tx = self.shared_tx.clone();
            b.push(Binding { slot: format!("snd:{key}"), spec, action: Arc::new(move || send_cmd(&tx, cmd.clone())) });
        }
        if let Some(h) = self.hotkeys.as_mut() {
            self.hk_status = h.sync(b);
        }
    }

    pub(super) fn stop_sounds(&mut self) {
        if let Some(e) = &self.engine {
            let _ = e.tx.send(Cmd::StopAll);
        }
    }
}
