//! Decoding audio files, resampling, and the soundboard data types.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Decodes a file to mono 16-bit samples (half the memory of f32), optionally
/// resampling to `target_rate` so only one copy is ever kept.
pub(crate) fn decode_file(path: &Path, target_rate: Option<u32>) -> Result<(Vec<i16>, u32), String> {
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

pub(crate) fn resample_i16(data: &[i16], from: u32, to: u32) -> Vec<i16> {
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


// ---------------------------------------------------------------------------
// Sounds (a single 16-bit mono copy each, kept at the engine's sample rate)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq)]
pub(crate) enum Status {
    Loading,
    Ready,
    Failed(String),
}

pub(crate) struct Sound {
    pub(crate) id: usize,
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) data: Arc<Vec<i16>>,
    pub(crate) rate: u32,
    pub(crate) volume: f32,
    pub(crate) speed: f32,
    pub(crate) looping: bool,
    pub(crate) trim_start: f32, // seconds from the start of the file
    pub(crate) trim_end: f32,   // seconds; 0 = play to the end
    pub(crate) vc: bool,        // play through the voice changer
    pub(crate) group: String,   // folder name, "" = Unsorted
    pub(crate) status: Status,
}

/// Everything saved about a sound (used when loading from the config or adding files).
#[derive(Clone)]
pub(crate) struct SoundSpec {
    pub(crate) path: PathBuf,
    pub(crate) volume: f32,
    pub(crate) speed: f32,
    pub(crate) looping: bool,
    pub(crate) trim_start: f32,
    pub(crate) trim_end: f32,
    pub(crate) vc: bool,
    pub(crate) group: String,
}

impl SoundSpec {
    pub(crate) fn new(path: PathBuf, group: &str) -> Self {
        Self {
            path,
            volume: 1.0,
            speed: 1.0,
            looping: false,
            trim_start: 0.0,
            trim_end: 0.0,
            vc: false,
            group: group.to_string(),
        }
    }
}

pub(crate) fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "mp3" | "wav" | "ogg" | "flac"))
        .unwrap_or(false)
}

/// Names of folders and presets: no control characters, no '=' or ';' or tabs.
pub(crate) fn clean_name(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && *c != '=' && *c != ';')
        .collect::<String>()
        .trim()
        .to_string()
}

impl Sound {
    pub(crate) fn duration(&self) -> f32 {
        self.data.len() as f32 / self.rate.max(1) as f32
    }

    /// Converts the stored copy to a new sample rate (only when it differs).
    pub(crate) fn convert_to(&mut self, rate: u32) {
        if rate != self.rate {
            self.data = Arc::new(resample_i16(&self.data, self.rate, rate));
            self.rate = rate;
        }
    }
}

pub(crate) type LoadMsg = (usize, Result<(Vec<i16>, u32), String>);
