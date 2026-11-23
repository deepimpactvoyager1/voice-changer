//! Where the config file lives and how it is written.

use std::path::PathBuf;

pub(crate) fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    match base {
        Some(b) => b.join("VoiceChanger").join("config.txt"),
        None => PathBuf::from("voice_changer_config.txt"),
    }
}

pub(crate) fn save_config(text: &str) -> std::io::Result<()> {
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
