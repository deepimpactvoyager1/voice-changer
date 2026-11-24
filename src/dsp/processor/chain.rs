//! The per-sample effect chain (steps 1-19, then the output limiter).

use std::f32::consts::PI;

use super::{Processor, RV_DAMP};
use crate::dsp::filters::{read_frac, shift_tap};

impl Processor {
    /// Call once at the end of every audio buffer.
    pub(crate) fn end_block(&mut self) {
        self.voices.retain(|v| v.looping || v.pos < v.end);
        if self.ns.profile_ready() {
            if let Ok(mut g) = self.params.noise_prof.try_lock() {
                let p = self.ns.profile();
                if g.len() == p.len() {
                    g.copy_from_slice(p);
                    self.ns.ack_profile();
                    self.params.ns_state.store(2, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    fn noise(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Mixes every playing soundboard voice. Returns (plain sounds, sounds that
    /// should go through the voice changer).
    fn mix_voices(&mut self) -> (f32, f32) {
        let mut plain = 0.0f32;
        let mut vc = 0.0f32;
        for v in self.voices.iter_mut() {
            if v.pos < v.end {
                let len = v.data.len();
                let i = (v.pos as usize).min(len - 1);
                let a = v.data[i] as f32;
                let b = if i + 1 < len { v.data[i + 1] as f32 } else { a };
                let frac = (v.pos - i as f64) as f32;
                let mut g = v.gain;
                if v.fade_in {
                    g *= ((v.pos - v.start) as f32 / 128.0).min(1.0);
                }
                if v.fade_out {
                    g *= ((v.end - v.pos) as f32 / 256.0).clamp(0.0, 1.0);
                }
                let sample = (a + (b - a) * frac) * g;
                if v.vc {
                    vc += sample;
                } else {
                    plain += sample;
                }
                v.pos += v.step;
                if v.looping && v.pos >= v.end {
                    v.pos = v.start + (v.pos - v.end).min(v.end - v.start - 1.0).max(0.0);
                }
            }
        }
        (plain, vc)
    }

    pub(crate) fn process(&mut self, input: f32) -> f32 {
        let (sb_plain, sb_vc) = self.mix_voices();
        let mut x = input * self.in_gain;

        // 1. noise suppression / formant shift (one shared STFT; effects don't boost noise)
        if self.ns_on || self.fm_inv > 0.0 || self.ns.learning() {
            let ns = if self.ns_on { Some((self.ns_alpha, self.ns_floor)) } else { None };
            x = self.ns.process(x, ns, self.fm_inv);
        }

        // 2. noise gate
        if self.gate_thresh > 0.0 {
            let a = x.abs();
            self.env = if a > self.env { a } else { self.env * self.env_rel };
            if self.env > self.gate_thresh {
                self.gate_open = true;
            } else if self.env < self.gate_thresh * 0.5 {
                self.gate_open = false; // closes only 6 dB below the open level
            }
            let target = if self.gate_open { 1.0 } else { 0.0 };
            let c = if target > self.gate_g { self.gate_atk } else { self.gate_rel };
            self.gate_g += (target - self.gate_g) * c;
            x *= self.gate_g;
        }

        // 2b. soundboard sounds set to "through voice changer" join here, after
        // the noise suppressor and gate (so they are never gated away), and
        // go through everything below. Muting the mic does not mute them.
        let mic = if self.st.muted { 0.0 } else { x };
        x = mic + sb_vc * (self.st.sb_vol / 32768.0);

        // 3. high-pass (rumble / low-end removal)
        if self.hpf_on {
            x = self.hpf.process(x);
        }

        // 4. EQ
        if self.eq_on {
            x = self.eq_low.process(x);
            x = self.eq_mid.process(x);
            x = self.eq_high.process(x);
        }

        // 5. de-esser (turns down the high band only when it dominates)
        if self.deess_on {
            self.de_lo += (x - self.de_lo) * self.de_a;
            let lo = self.de_lo;
            let hi = x - lo;
            self.de_he = hi.abs().max(self.de_he * self.de_rel);
            self.de_le = lo.abs().max(self.de_le * self.de_rel);
            let limit = 0.9 * self.de_le + 0.002;
            let g = if self.de_he > limit { limit / self.de_he } else { 1.0 };
            x = lo + hi * (1.0 - self.st.deess * (1.0 - g));
        }

        // 6. compressor
        if self.comp_on {
            let a = x.abs();
            let c = if a > self.comp_env { self.comp_atk } else { self.comp_rel };
            self.comp_env += (a - self.comp_env) * c;
            let g = if self.comp_env > self.comp_thresh {
                (self.comp_thresh / self.comp_env).powf(self.comp_pow)
            } else {
                1.0
            };
            x *= g * self.comp_makeup;
        }

        // 6b. autotune: after the compressor (steady level helps the detector), before
        // pitch shift / harmonizer (so harmonies and transposition build on the corrected
        // voice) and before every modulation, echo and reverb effect.
        if self.at_on {
            x = self.at.process(x, &self.params.at_note);
        }

        // 7. pitch shift + harmonizer (the delay line is always fed so toggling is click-free)
        self.pbuf[self.pwrite] = x;
        self.pwrite = (self.pwrite + 1) % self.pbuf.len();
        if self.pitch_on {
            self.pphase = (self.pphase + self.pitch_step).rem_euclid(1.0);
            x = shift_tap(&self.pbuf, self.pwrite, self.pphase, self.pwindow);
        }
        for k in 0..2 {
            if self.harm_on[k] {
                self.hphase[k] = (self.hphase[k] + self.harm_step[k]).rem_euclid(1.0);
                let mix = if k == 0 { self.st.harm1_mix } else { self.st.harm2_mix };
                x += shift_tap(&self.pbuf, self.pwrite, self.hphase[k], self.pwindow) * mix;
            }
        }

        // 8. vibrato (wobbling pitch)
        if self.vib_on {
            self.vbuf[self.vwrite] = x;
            self.vwrite = (self.vwrite + 1) % self.vbuf.len();
            self.vphase += self.vib_inc;
            if self.vphase >= 2.0 * PI {
                self.vphase -= 2.0 * PI;
            }
            let d = self.vib_base + self.vib_depth * self.vphase.sin();
            x = read_frac(&self.vbuf, self.vwrite, d);
        }

        // 9. robot (ring modulation, blended)
        if self.st.robot_amount > 0.001 {
            self.rphase += self.robot_inc;
            if self.rphase >= 2.0 * PI {
                self.rphase -= 2.0 * PI;
            }
            let ph = self.rphase * (0.5 / PI);
            let carrier = match self.st.robot_wave {
                0 => self.rphase.sin(),
                1 => {
                    if ph < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                2 => 4.0 * (ph - 0.5).abs() - 1.0,
                _ => 2.0 * ph - 1.0,
            };
            let a = self.st.robot_amount;
            x *= 1.0 - a + a * carrier;
        }

        // 9b. growl (fast pulsing volume, like vocal fry)
        if self.growl_on {
            self.gphase += self.growl_inc;
            if self.gphase >= 1.0 {
                self.gphase -= 1.0;
            }
            x *= 1.0 - self.st.growl_depth * self.gphase;
        }

        // 10. whisper (noise that follows your volume)
        if self.whisper_on {
            let a = x.abs();
            let c = if a > self.w_env { self.w_atk } else { self.w_rel };
            self.w_env += (a - self.w_env) * c;
            let n = self.noise();
            let hn = n - self.w_prev; // first difference = hissier noise
            self.w_prev = n;
            let w = self.st.whisper;
            x = x * (1.0 - w) + hn * self.w_env * 1.2 * w;
        }

        // 11. chorus / flanger
        if self.chorus_on {
            self.cphase += self.chorus_inc;
            if self.cphase >= 2.0 * PI {
                self.cphase -= 2.0 * PI;
            }
            let (s, c) = self.cphase.sin_cos();
            let t1 = read_frac(&self.cbuf, self.cwrite, self.chorus_base + self.chorus_depth * s);
            let t2 = read_frac(&self.cbuf, self.cwrite, self.chorus_base + self.chorus_depth * c);
            let wet = 0.5 * (t1 + t2);
            self.cbuf[self.cwrite] = x + wet * self.chorus_fb;
            self.cwrite = (self.cwrite + 1) % self.cbuf.len();
            let m = self.st.chorus_mix;
            x = x * (1.0 - 0.5 * m) + wet * 0.7 * m;
        }

        // 11b. voice doubler (two quietly detuned, delayed copies)
        if self.dbl_on {
            self.dphase += self.dbl_inc;
            if self.dphase >= 2.0 * PI {
                self.dphase -= 2.0 * PI;
            }
            let (s, c) = self.dphase.sin_cos();
            let t1 = read_frac(&self.dbuf, self.dwrite, self.dbl_base + 60.0 * s);
            let t2 = read_frac(&self.dbuf, self.dwrite, self.dbl_base * 1.6 + 60.0 * c);
            self.dbuf[self.dwrite] = x;
            self.dwrite = (self.dwrite + 1) % self.dbuf.len();
            let m = self.st.dbl_mix;
            x = x * (1.0 - 0.3 * m) + 0.5 * (t1 + t2) * 0.8 * m;
        }

        // 12. tremolo
        if self.trem_on {
            self.tphase += self.trem_inc;
            if self.tphase >= 2.0 * PI {
                self.tphase -= 2.0 * PI;
            }
            x *= 1.0 - self.st.trem_depth * (0.5 - 0.5 * self.tphase.sin());
        }

        // 13. echo (skipped completely while the mix is 0)
        if self.echo_on {
            let elen = self.ebuf.len();
            let delayed = self.ebuf[(self.eidx + elen - self.echo_delay) % elen];
            self.edamp += (delayed - self.edamp) * self.echo_coef;
            self.ebuf[self.eidx] = x + self.edamp * self.st.echo_fb;
            self.eidx = (self.eidx + 1) % elen;
            x += self.edamp * self.st.echo_mix;
        }

        // 14. reverb (4 combs + 2 all-pass filters)
        if self.reverb_on {
            let inp = x * 0.12;
            let mut acc = 0.0f32;
            for k in 0..4 {
                let i = self.rv_ci[k];
                let o = self.rv_comb[k][i];
                self.rv_cf[k] = o * (1.0 - RV_DAMP) + self.rv_cf[k] * RV_DAMP;
                self.rv_comb[k][i] = inp + self.rv_cf[k] * self.rv_fb;
                self.rv_ci[k] = if i + 1 >= self.rv_comb[k].len() { 0 } else { i + 1 };
                acc += o;
            }
            for k in 0..2 {
                let i = self.rv_ai[k];
                let b = self.rv_ap[k][i];
                let o = b - acc;
                self.rv_ap[k][i] = acc + b * 0.5;
                self.rv_ai[k] = if i + 1 >= self.rv_ap[k].len() { 0 } else { i + 1 };
                acc = o;
            }
            x += acc * self.st.reverb_mix;
        }

        // 15. distortion
        if self.st.drive > 0.01 {
            let k = self.drive_k;
            x = match self.st.dist_type {
                1 => (x * k).clamp(-1.0, 1.0),
                2 => (x * k * 0.5 * PI).sin(),
                _ => (x * k).tanh() / k.tanh(),
            };
        }

        // 16. bit crusher
        if self.levels > 0.0 {
            x = (x.clamp(-1.0, 1.0) * self.levels).round() / self.levels;
        }

        // 17. sample-rate reducer (sample & hold)
        if self.hold_step > 0.0 {
            self.hold_acc += 1.0;
            if self.hold_acc >= self.hold_step {
                self.hold_acc -= self.hold_step;
                self.hold_val = x;
            }
            x = self.hold_val;
        }

        // 18. low-pass ("telephone" / muffled)
        if self.lpf_on {
            x = self.lpf.process(x);
        }

        // 19. hiss
        if self.st.hiss > 0.0 {
            x += self.noise() * self.st.hiss;
        }

        let y = x * self.st.out_gain + sb_plain * (self.st.sb_vol / 32768.0);

        // Output safety: a smooth limiter rounds peaks off above 90 % instead of
        // hard-clipping them (hard clipping is what makes loud, harsh buzzing).
        let a = y.abs();
        if self.limiter_on && a > 0.9 {
            y.signum() * (0.9 + 0.1 * ((a - 0.9) * 10.0).tanh())
        } else {
            y.clamp(-1.0, 1.0)
        }
    }
}
