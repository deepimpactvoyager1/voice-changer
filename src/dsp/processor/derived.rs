//! Settings -> derived coefficients (recomputed only when settings change).

use std::f32::consts::PI;

use super::Processor;

impl Processor {
    pub(super) fn update_derived(&mut self) {
        let s = self.st;
        let sr = self.sr;
        let coef = |ms: f32| 1.0 - (-1.0 / (ms.max(0.05) * 0.001 * sr)).exp();

        self.in_gain = 10f32.powf(s.in_gain_db / 20.0);
        self.gate_thresh = if s.gate_db <= -79.0 { 0.0 } else { 10f32.powf(s.gate_db / 20.0) };
        self.gate_atk = coef(s.gate_atk_ms);
        self.gate_rel = coef(s.gate_rel_ms);

        self.hpf_on = s.hpf_hz > 25.0;
        if self.hpf_on {
            self.hpf.set(false, s.hpf_hz, sr);
        }
        self.lpf_on = s.lpf_hz < 19000.0f32.min(sr * 0.45);
        if self.lpf_on {
            self.lpf.set(true, s.lpf_hz, sr);
        }

        let ns_on = s.ns_amount > 0.01;
        if ns_on && !self.ns_on {
            self.ns.reset();
        }
        self.ns_on = ns_on;
        let amt = s.ns_amount.clamp(0.0, 1.0);
        self.ns_alpha = 1.0 + 4.0 * amt;
        self.ns_floor = 10f32.powf(-(8.0 + 22.0 * amt) / 20.0);

        self.eq_on = s.eq_low_db.abs() > 0.1 || s.eq_mid_db.abs() > 0.1 || s.eq_high_db.abs() > 0.1;
        if self.eq_on {
            self.eq_low.shelf(false, 120.0, s.eq_low_db, sr);
            self.eq_mid.peak(1000.0, s.eq_mid_db, 0.8, sr);
            self.eq_high.shelf(true, 6000.0, s.eq_high_db, sr);
        }

        self.comp_on = s.comp_ratio > 1.05;
        self.comp_thresh = 10f32.powf(s.comp_thresh_db / 20.0);
        self.comp_pow = 1.0 - 1.0 / s.comp_ratio.max(1.0);
        self.comp_makeup = 10f32.powf(s.comp_makeup_db / 20.0);
        self.deess_on = s.deess > 0.01;

        self.pitch_on = s.semitones.abs() > 0.05;
        self.pwindow = (sr * s.pitch_grain_ms.clamp(10.0, 100.0) * 0.001).min(self.pbuf.len() as f32 - 8.0);
        let ratio = 2f32.powf(s.semitones / 12.0);
        self.pitch_step = (1.0 - ratio) / self.pwindow;

        self.robot_inc = 2.0 * PI * s.robot_hz / sr;
        self.whisper_on = s.whisper > 0.01;
        for k in 0..2 {
            let (semi, mix) = if k == 0 { (s.harm1_semi, s.harm1_mix) } else { (s.harm2_semi, s.harm2_mix) };
            self.harm_on[k] = mix > 0.01;
            self.harm_step[k] = (1.0 - 2f32.powf(semi / 12.0)) / self.pwindow;
        }
        self.growl_on = s.growl_depth > 0.01;
        self.growl_inc = s.growl_hz.clamp(10.0, 120.0) / sr;
        let dbl_on = s.dbl_mix > 0.01;
        if dbl_on && !self.dbl_on {
            self.dbuf.fill(0.0);
        }
        self.dbl_on = dbl_on;
        self.dbl_base = s.dbl_ms.clamp(10.0, 40.0) * 0.001 * sr;
        self.dbl_inc = 2.0 * PI * 0.4 / sr;
        self.limiter_on = s.limiter;

        let vib_on = s.vib_depth > 0.01;
        if vib_on && !self.vib_on {
            self.vbuf.fill(0.0);
        }
        self.vib_on = vib_on;
        self.vib_inc = 2.0 * PI * s.vib_hz / sr;
        self.vib_base = 0.005 * sr;
        self.vib_depth = s.vib_depth.clamp(0.0, 1.0) * 0.004 * sr;

        self.trem_on = s.trem_depth > 0.01;
        self.trem_inc = 2.0 * PI * s.trem_hz / sr;

        let chorus_on = s.chorus_mix > 0.01;
        if chorus_on && !self.chorus_on {
            self.cbuf.fill(0.0);
        }
        self.chorus_on = chorus_on;
        let flange = s.chorus_flange == 1;
        self.chorus_inc = 2.0 * PI * s.chorus_hz / sr;
        self.chorus_base = if flange { 0.003 * sr } else { 0.014 * sr };
        self.chorus_depth = s.chorus_depth.clamp(0.0, 1.0) * if flange { 0.0025 * sr } else { 0.006 * sr };
        self.chorus_fb = if flange { 0.55 } else { 0.0 };

        // Echo is skipped entirely while off; clear the tail when it switches on.
        let echo_on = s.echo_mix > 0.001;
        if echo_on && !self.echo_on {
            self.ebuf.fill(0.0);
            self.edamp = 0.0;
        }
        self.echo_on = echo_on;
        self.echo_delay = ((s.echo_ms * 0.001 * sr) as usize).clamp(1, self.ebuf.len() - 1);
        self.echo_coef = 1.0 - 0.9 * s.echo_damp.clamp(0.0, 1.0);

        let reverb_on = s.reverb_mix > 0.01;
        if reverb_on && !self.reverb_on {
            for b in self.rv_comb.iter_mut() {
                b.fill(0.0);
            }
            for b in self.rv_ap.iter_mut() {
                b.fill(0.0);
            }
            self.rv_cf = [0.0; 4];
        }
        self.reverb_on = reverb_on;
        self.rv_fb = 0.7 + 0.25 * s.reverb_size.clamp(0.0, 1.0);

        self.drive_k = 1.0 + s.drive * 19.0;
        self.levels = if s.bits < 15.5 { 2f32.powf(s.bits.round().max(2.0) - 1.0) } else { 0.0 };
        self.hold_step = if s.rate_hz < sr * 0.98 { sr / s.rate_hz.max(500.0) } else { 0.0 };
    }
}
