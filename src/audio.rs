//! Audio devices and the engine that owns the cpal streams.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, SampleRate, Stream, StreamConfig};
use ringbuf::{traits::*, HeapCons, HeapProd, HeapRb};

use crate::dsp::Processor;
use crate::settings::{Cmd, Params};

pub(crate) struct Engine {
    _input: Stream,
    _output: Stream,
    _monitor: Option<Stream>,
    pub(crate) sample_rate: u32,
    pub(crate) tx: Sender<Cmd>,
    pub(crate) warning: Option<String>,
}

fn list_devices(input: bool) -> Vec<Device> {
    let host = cpal::default_host();
    if input {
        host.input_devices().map(|d| d.collect()).unwrap_or_default()
    } else {
        host.output_devices().map(|d| d.collect()).unwrap_or_default()
    }
}

pub(crate) fn device_names(input: bool) -> Vec<String> {
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
    pub(crate) fn start(in_name: &str, out_name: &str, mon_name: &str, params: Arc<Params>) -> Result<Engine, String> {
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
                    let mon_gain = proc.st.mon_vol;
                    let mut peak = 0.0f32;
                    for frame in data.chunks(in_ch) {
                        peak = peak.max(frame[0].abs());
                        let y = proc.process(frame[0]);
                        let _ = prod.try_push(y);
                        if let Some(m) = mon_prod.as_mut() {
                            let _ = m.try_push(y * mon_gain);
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
