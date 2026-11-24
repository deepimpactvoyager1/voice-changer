//! Global hotkeys. Types + editor widget are portable; registration is Windows-only.
use crate::settings::Cmd;
use eframe::egui;
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};

pub const CTRL: u8 = 1;
pub const ALT: u8 = 2;
pub const SHIFT: u8 = 4;
pub const WIN: u8 = 8;

pub type Action = Arc<dyn Fn() + Send + Sync>;
/// Current engine's command sender, shared with the hotkey/tray threads.
pub type SharedTx = Arc<Mutex<Option<Sender<Cmd>>>>;

pub fn send_cmd(tx: &SharedTx, c: Cmd) {
    if let Ok(g) = tx.lock() {
        if let Some(t) = g.as_ref() {
            let _ = t.send(c);
        }
    }
}

pub struct Key { pub code: String, pub label: String, pub plain_ok: bool }

/// F-keys and numpad keys may be used alone; letters/digits/etc. need a modifier.
pub fn keys() -> &'static [Key] {
    static K: OnceLock<Vec<Key>> = OnceLock::new();
    K.get_or_init(|| {
        let mut v = Vec::new();
        let mut add = |c: String, l: String, p: bool| v.push(Key { code: c, label: l, plain_ok: p });
        for n in 1..=24 { add(format!("F{n}"), format!("F{n}"), true); }
        for n in 0..=9 { add(format!("Numpad{n}"), format!("Numpad {n}"), true); }
        for (c, l) in [("NumpadAdd", "Numpad +"), ("NumpadSubtract", "Numpad -"), ("NumpadMultiply", "Numpad *"),
                       ("NumpadDivide", "Numpad /"), ("NumpadDecimal", "Numpad .")] {
            add(c.into(), l.into(), true);
        }
        for c in 'A'..='Z' { add(format!("Key{c}"), c.to_string(), false); }
        for n in 0..=9 { add(format!("Digit{n}"), n.to_string(), false); }
        for (c, l) in [("Space", "Space"), ("Insert", "Insert"), ("Delete", "Delete"), ("Home", "Home"), ("End", "End"),
                       ("PageUp", "Page Up"), ("PageDown", "Page Down"), ("ArrowUp", "Up"), ("ArrowDown", "Down"),
                       ("ArrowLeft", "Left"), ("ArrowRight", "Right")] {
            add(c.into(), l.into(), false);
        }
        v
    })
}

/// `key` is a keyboard_types::Code name ("F5", "Numpad3", "KeyA"); empty = unassigned.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct HotKeySpec { pub mods: u8, pub key: String }

impl HotKeySpec {
    pub fn is_set(&self) -> bool { !self.key.is_empty() }
    pub fn check(&self) -> Result<(), &'static str> {
        let k = keys().iter().find(|k| k.code == self.key).ok_or("unknown key")?;
        if self.mods == 0 && !k.plain_ok { return Err("needs Ctrl/Alt/Shift/Win (or use an F-key/numpad key)"); }
        Ok(())
    }
    pub fn to_text(&self) -> String { format!("{}:{}", self.mods, self.key) }
    pub fn parse(s: &str) -> Option<Self> {
        let (m, k) = s.split_once(':')?;
        let mods = m.trim().parse::<u8>().ok()? & 15;
        let key = k.trim();
        keys().iter().any(|x| x.code == key).then(|| Self { mods, key: key.to_string() })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status { Ok, Conflict, Failed(String) }

pub struct Binding { pub slot: String, pub spec: HotKeySpec, pub action: Action }

/// Everything persisted: Stop all, Mute mic, and per-sound keys (keyed by file path).
#[derive(Default, Clone)]
pub struct HotkeyConfig {
    pub stop: HotKeySpec,
    pub mute: HotKeySpec,
    pub sounds: HashMap<String, HotKeySpec>,
}

impl HotkeyConfig {
    pub fn write_lines(&self, out: &mut String) {
        use std::fmt::Write;
        if self.stop.is_set() { let _ = writeln!(out, "hk_stop={}", self.stop.to_text()); }
        if self.mute.is_set() { let _ = writeln!(out, "hk_mute={}", self.mute.to_text()); }
        let mut v: Vec<_> = self.sounds.iter().filter(|(_, s)| s.is_set()).collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        for (p, s) in v { let _ = writeln!(out, "hk_sound={}\t{}", p, s.to_text()); }
    }
    /// Returns true if the key was an hk_* key.
    pub fn parse_line(&mut self, key: &str, val: &str) -> bool {
        match key {
            "hk_stop" => { if let Some(s) = HotKeySpec::parse(val) { self.stop = s; } }
            "hk_mute" => { if let Some(s) = HotKeySpec::parse(val) { self.mute = s; } }
            "hk_sound" => {
                if let Some((p, s)) = val.rsplit_once('\t') {
                    if let Some(s) = HotKeySpec::parse(s) { self.sounds.insert(p.to_string(), s); }
                }
            }
            _ => return false,
        }
        true
    }
}

/// Modifier checkboxes + key dropdown + Clear + status. Returns true if changed.
pub fn editor(ui: &mut egui::Ui, id: impl std::hash::Hash + Copy, spec: &mut HotKeySpec, status: Option<&Status>) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        for (bit, name) in [(CTRL, "Ctrl"), (ALT, "Alt"), (SHIFT, "Shift"), (WIN, "Win")] {
            let mut on = spec.mods & bit != 0;
            if ui.checkbox(&mut on, name).changed() {
                if on { spec.mods |= bit } else { spec.mods &= !bit }
                changed = true;
            }
        }
        let cur = keys().iter().find(|k| k.code == spec.key).map(|k| k.label.clone()).unwrap_or_else(|| "(none)".into());
        egui::ComboBox::from_id_source((id, "hk_key")).selected_text(cur).width(90.0).show_ui(ui, |ui| {
            for k in keys() {
                if ui.selectable_label(spec.key == k.code, &k.label).clicked() { spec.key = k.code.clone(); changed = true; }
            }
        });
        if ui.small_button("Clear").clicked() { *spec = HotKeySpec::default(); changed = true; }
        if spec.is_set() {
            let (amber, red, green) = (egui::Color32::from_rgb(230, 160, 60), egui::Color32::from_rgb(230, 80, 80), egui::Color32::from_rgb(90, 200, 120));
            match (spec.check(), status) {
                (Err(e), _) => { ui.colored_label(amber, e); }
                (_, Some(Status::Conflict)) => { ui.colored_label(red, "Conflict: same combo used twice"); }
                (_, Some(Status::Failed(m))) => { ui.colored_label(red, "Not registered (in use by another program?)").on_hover_text(m); }
                (_, Some(Status::Ok)) => { ui.colored_label(green, "Active"); }
                _ => {}
            }
        }
    });
    changed
}

#[cfg(windows)]
mod imp {
    use super::*;
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use std::str::FromStr;

    /// Must be created AND used (register/unregister) on the main/UI thread.
    pub struct Hotkeys {
        mgr: GlobalHotKeyManager,
        regs: Vec<HotKey>,
        table: Arc<Mutex<HashMap<u32, Action>>>,
    }

    impl Hotkeys {
        pub fn new(ctx: egui::Context) -> Option<Self> {
            let mgr = GlobalHotKeyManager::new().ok()?;
            let table: Arc<Mutex<HashMap<u32, Action>>> = Default::default();
            let t = table.clone();
            // Works while the window is hidden: does not depend on update().
            std::thread::Builder::new().name("hotkey-events".into()).spawn(move || {
                let rx = GlobalHotKeyEvent::receiver();
                while let Ok(ev) = rx.recv() {
                    if ev.state != HotKeyState::Pressed { continue; }
                    let act = t.lock().ok().and_then(|m| m.get(&ev.id).cloned());
                    if let Some(a) = act { a(); ctx.request_repaint(); }
                }
            }).ok()?;
            Some(Self { mgr, regs: Vec::new(), table })
        }

        /// Re-registers everything; returns status per slot (invalid/unset specs are skipped).
        pub fn sync(&mut self, binds: Vec<Binding>) -> HashMap<String, Status> {
            for h in self.regs.drain(..) { let _ = self.mgr.unregister(h); }
            let valid: Vec<&Binding> = binds.iter().filter(|b| b.spec.check().is_ok()).collect();
            let mut table = HashMap::new();
            let mut out = HashMap::new();
            for b in &valid {
                if valid.iter().filter(|o| o.spec == b.spec).count() > 1 {
                    out.insert(b.slot.clone(), Status::Conflict);
                    continue;
                }
                let Ok(code) = Code::from_str(&b.spec.key) else { continue };
                let mut m = Modifiers::empty();
                if b.spec.mods & CTRL != 0 { m |= Modifiers::CONTROL; }
                if b.spec.mods & ALT != 0 { m |= Modifiers::ALT; }
                if b.spec.mods & SHIFT != 0 { m |= Modifiers::SHIFT; }
                if b.spec.mods & WIN != 0 { m |= Modifiers::SUPER; }
                let hk = HotKey::new(if m.is_empty() { None } else { Some(m) }, code);
                match self.mgr.register(hk) {
                    Ok(()) => { table.insert(hk.id(), b.action.clone()); self.regs.push(hk); out.insert(b.slot.clone(), Status::Ok); }
                    Err(e) => { out.insert(b.slot.clone(), Status::Failed(e.to_string())); }
                }
            }
            if let Ok(mut t) = self.table.lock() { *t = table; }
            out
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub struct Hotkeys;
    impl Hotkeys {
        pub fn new(_ctx: egui::Context) -> Option<Self> { None }
        pub fn sync(&mut self, _b: Vec<Binding>) -> HashMap<String, Status> { HashMap::new() }
    }
}
pub use imp::Hotkeys;