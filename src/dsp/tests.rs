//! Tests: run the whole chain on synthetic audio, no sound hardware needed.

use std::f32::consts::PI;
use std::sync::{mpsc, Arc};

use super::noise::*;
use super::Processor;
use crate::settings::{Cmd, Params, Settings};
use crate::soundboard::resample_i16;

    fn run_with(settings: Settings, n: usize, input: &dyn Fn(usize) -> f32) -> Vec<f32> {
        let params = Arc::new(Params::new());
        *params.settings.lock().unwrap() = settings;
        let (_tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        let mut out = Vec::new();
        for block in 0..(n / 480) {
            p.begin_block();
            for i in 0..480 {
                out.push(p.process(input(block * 480 + i)));
            }
            p.end_block();
        }
        out
    }

    fn tone(i: usize) -> f32 {
        0.3 * (2.0 * PI * 220.0 * i as f32 / 48000.0).sin()
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
    }

    fn play(data: Vec<i16>, looping: bool, start: f64, end: f64, vc: bool) -> Cmd {
        Cmd::Play { data: Arc::new(data), gain: 1.0, step: 1.0, looping, start, end, vc }
    }

    #[test]
    fn neutral_is_transparent() {
        let out = run_with(Settings::neutral(), 4800, &tone);
        assert!((out[4000] - tone(4000)).abs() < 1e-4);
    }

    #[test]
    fn default_gate_is_minus_45_db() {
        assert_eq!(Settings::neutral().gate_db, -45.0);
        // a normal-level voice passes, a very quiet hiss is gated away
        let quiet = |i: usize| 0.001 * (2.0 * PI * 220.0 * i as f32 / 48000.0).sin();
        let out = run_with(Settings::neutral(), 48000, &quiet);
        assert!(rms(&out[24000..]) < 0.0001);
    }

    #[test]
    fn extreme_settings_stay_finite_and_bounded() {
        let s = Settings {
            in_gain_db: 30.0,
            gate_db: -20.0,
            hpf_hz: 600.0,
            ns_amount: 1.0,
            eq_low_db: 12.0,
            eq_mid_db: -12.0,
            eq_high_db: 12.0,
            comp_ratio: 20.0,
            comp_thresh_db: -40.0,
            comp_makeup_db: 24.0,
            deess: 1.0,
            semitones: -24.0,
            pitch_grain_ms: 100.0,
            robot_amount: 1.0,
            robot_hz: 200.0,
            robot_wave: 1,
            whisper: 1.0,
            harm1_mix: 1.0,
            harm1_semi: -24.0,
            harm2_mix: 1.0,
            harm2_semi: 24.0,
            growl_depth: 1.0,
            vib_depth: 1.0,
            trem_depth: 1.0,
            chorus_mix: 1.0,
            chorus_flange: 1,
            dbl_mix: 1.0,
            dbl_ms: 40.0,
            echo_mix: 1.0,
            echo_fb: 0.9,
            echo_ms: 800.0,
            echo_damp: 1.0,
            reverb_mix: 1.0,
            reverb_size: 1.0,
            drive: 1.0,
            dist_type: 2,
            bits: 4.0,
            rate_hz: 2000.0,
            lpf_hz: 1000.0,
            hiss: 0.1,
            out_gain: 3.0,
            sb_vol: 2.0,
            ..Settings::neutral()
        };
        let out = run_with(s, 96000, &tone);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        assert!(out.iter().any(|v| v.abs() > 0.01));
    }

    #[test]
    fn max_input_gain_with_max_distortion_is_bounded() {
        let s = Settings { in_gain_db: 30.0, drive: 1.0, gate_db: -80.0, ..Settings::neutral() };
        let noise = |i: usize| ((i * 7919 % 2001) as f32 / 1000.0 - 1.0) * 0.01;
        let out = run_with(s, 48000, &noise);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
    }

    #[test]
    fn each_new_effect_alone_is_finite() {
        let n = Settings::neutral();
        let cases = [
            Settings { eq_low_db: 9.0, eq_mid_db: 6.0, eq_high_db: -9.0, ..n },
            Settings { comp_ratio: 6.0, ..n },
            Settings { deess: 0.8, ..n },
            Settings { vib_depth: 0.8, ..n },
            Settings { trem_depth: 0.8, ..n },
            Settings { chorus_mix: 0.8, ..n },
            Settings { chorus_mix: 0.8, chorus_flange: 1, ..n },
            Settings { reverb_mix: 0.8, ..n },
            Settings { whisper: 0.8, ..n },
            Settings { semitones: 24.0, ..n },
            Settings { harm1_mix: 0.8, harm1_semi: 4.0, harm2_mix: 0.8, harm2_semi: -12.0, ..n },
            Settings { growl_depth: 0.8, ..n },
            Settings { dbl_mix: 0.8, ..n },
        ];
        for s in cases {
            let out = run_with(s, 48000, &tone);
            assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
            assert!(rms(&out[24000..]) > 0.001);
        }
    }

    #[test]
    fn fft_round_trip() {
        let fft = Fft::new(NS_N);
        let orig: Vec<f32> = (0..NS_N).map(|i| ((i * 7) % 13) as f32 - 6.0).collect();
        let mut re = orig.clone();
        let mut im = vec![0.0; NS_N];
        fft.run(&mut re, &mut im, false);
        fft.run(&mut re, &mut im, true);
        for i in 0..NS_N {
            assert!((re[i] - orig[i]).abs() < 1e-3);
            assert!(im[i].abs() < 1e-3);
        }
    }

    #[test]
    fn noise_suppression_reduces_steady_noise() {
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..48000 * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / 16777216.0 * 2.0 - 1.0) * 0.1
            })
            .collect();
        let s = Settings { ns_amount: 1.0, gate_db: -80.0, ..Settings::neutral() };
        let out = run_with(s, noise.len(), &|i| noise[i]);
        let before = rms(&noise[3 * 48000..4 * 48000 - 480]);
        let after = rms(&out[3 * 48000..4 * 48000 - 480]);
        assert!(after < before * 0.5, "before {before}, after {after}");
    }

    #[test]
    fn noise_suppression_keeps_a_voice_like_tone() {
        let mut seed = 777u32;
        let noise: Vec<f32> = (0..48000 * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / 16777216.0 * 2.0 - 1.0) * 0.05
            })
            .collect();
        let input = |i: usize| {
            let t = i as f32 / 48000.0;
            noise[i] + if t > 1.0 { 0.3 * (2.0 * PI * 300.0 * t).sin() } else { 0.0 }
        };
        let s = Settings { ns_amount: 1.0, gate_db: -80.0, ..Settings::neutral() };
        let out = run_with(s, noise.len(), &input);
        let a: Vec<f32> = (3 * 48000..4 * 48000 - 480).map(input).collect();
        let b = &out[3 * 48000..4 * 48000 - 480];
        assert!(rms(b) > rms(&a) * 0.8, "kept {} of {}", rms(b), rms(&a));
    }

    #[test]
    fn settings_round_trip_through_text() {
        let mut s = Settings::neutral();
        s.semitones = -5.5;
        s.reverb_mix = 0.35;
        s.robot_wave = 3;
        s.dist_type = 2;
        s.chorus_flange = 1;
        s.harm1_semi = -12.0;
        s.dbl_mix = 0.25;
        let text = s.serialize();
        let mut back = Settings::neutral();
        back.apply(&text);
        assert_eq!(back, s);
        // junk is ignored
        back.apply("nonsense;semitones=abc;=;robot_wave=99");
        assert_eq!(back.robot_wave, 3);
        assert_eq!(back.semitones, -5.5);
    }

    #[test]
    fn imported_values_are_clamped_to_slider_ranges() {
        let mut s = Settings::neutral();
        s.apply("semitones=999;in_gain_db=500;reverb_mix=7;echo_fb=5;gate_db=-300");
        assert_eq!(s.semitones, 24.0);
        assert_eq!(s.in_gain_db, 30.0);
        assert_eq!(s.reverb_mix, 1.0);
        assert_eq!(s.echo_fb, 0.9);
        assert_eq!(s.gate_db, -80.0);
    }

    #[test]
    fn mute_silences_mic_but_soundboard_plays() {
        let params = Arc::new(Params::new());
        let mut s = Settings::neutral();
        s.muted = true;
        *params.settings.lock().unwrap() = s;
        let (tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        tx.send(play(vec![16384i16; 100], false, 0.0, 100.0, false)).unwrap();
        p.begin_block();
        let a = p.process(0.9);
        for _ in 0..98 {
            p.process(0.9);
        }
        let last = p.process(0.9);
        let after = p.process(0.9);
        p.end_block();
        assert!((a - 0.5).abs() < 1e-6 && (last - 0.5).abs() < 1e-6);
        assert!(after.abs() < 1e-6);
        assert!(p.voices.is_empty());
    }

    #[test]
    fn sound_through_voice_changer_survives_mute_and_gets_effects() {
        let params = Arc::new(Params::new());
        let mut s = Settings::neutral();
        s.muted = true;
        *params.settings.lock().unwrap() = s;
        let (tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        tx.send(play(vec![16384i16; 2000], false, 0.0, 2000.0, true)).unwrap();
        p.begin_block();
        let y = p.process(0.9);
        p.end_block();
        assert!((y - 0.5).abs() < 1e-4, "got {y}");

        // with robot on, the routed sound is modulated; a plain sound would not be
        let params = Arc::new(Params::new());
        let s = Settings { robot_amount: 1.0, robot_hz: 100.0, muted: true, ..Settings::neutral() };
        *params.settings.lock().unwrap() = s;
        let (tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        tx.send(play(vec![16384i16; 4800], false, 0.0, 4800.0, true)).unwrap();
        p.begin_block();
        let outs: Vec<f32> = (0..4800).map(|_| p.process(0.0)).collect();
        let min = outs.iter().cloned().fold(f32::MAX, f32::min);
        let max = outs.iter().cloned().fold(f32::MIN, f32::max);
        assert!(max - min > 0.3, "range {min}..{max}");
    }

    #[test]
    fn trimmed_sound_plays_only_its_range() {
        let params = Arc::new(Params::new());
        let (tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        tx.send(play(vec![16384i16; 1000], false, 100.0, 400.0, false)).unwrap();
        p.begin_block();
        let outs: Vec<f32> = (0..600).map(|_| p.process(0.0)).collect();
        p.end_block();
        let nonzero = outs.iter().filter(|v| v.abs() > 1e-6).count();
        assert!(nonzero >= 290 && nonzero <= 300, "nonzero {nonzero}");
        assert!(p.voices.is_empty());
    }

    #[test]
    fn looping_sound_keeps_playing() {
        let params = Arc::new(Params::new());
        let (tx, rx) = mpsc::channel();
        let mut p = Processor::new(params, 48000.0, rx);
        tx.send(play(vec![8192i16; 50], true, 0.0, 50.0, false)).unwrap();
        p.begin_block();
        for _ in 0..500 {
            p.process(0.0);
        }
        p.end_block();
        assert_eq!(p.voices.len(), 1);
    }

    #[test]
    fn resample_changes_length() {
        let v: Vec<i16> = (0..44100).map(|i| ((i as f32 * 0.01).sin() * 10000.0) as i16).collect();
        let r = resample_i16(&v, 44100, 48000);
        assert!((r.len() as i64 - 48000).abs() <= 1);
    }
