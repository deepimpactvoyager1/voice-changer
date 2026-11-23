//! Autotune: YIN pitch detector on a downsampled copy, plus a pitch-synchronous
//! splice shifter that repeats or skips whole pitch periods.

use std::f32::consts::PI;
use std::sync::atomic::{AtomicU32, Ordering};

const WIN: usize = 160; // YIN window in decimated samples (20 ms at 8 kHz)
const HOP: usize = 48; // detect every 48 decimated samples (6 ms)
const DEC_N: usize = 512; // decimated ring size (power of two)
const F_MIN: f32 = 70.0;
const F_MAX: f32 = 1000.0;

/// Semitones above the key. To add a scale: add a row here, raise the `[..; N]`,
/// raise the `min(5)` in settings.rs and add a name in voice_tab.rs.
const SCALES: [&[u8]; 6] = [
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11], // chromatic
    &[0, 2, 4, 5, 7, 9, 11],                 // major
    &[0, 2, 3, 5, 7, 8, 10],                 // minor
    &[0, 2, 4, 7, 9],                        // major pentatonic
    &[0, 3, 5, 7, 10],                       // minor pentatonic
    &[0, 3, 5, 6, 7, 10],                    // blues
];

pub(super) struct Autotune {
    sr: f32,
    on: bool,
    amount: f32,
    hum: f32,
    key: i32,
    mask: u32,
    coef_speed: f32,
    coef_rel: f32,
    a_slow: f32,
    // detector
    dfac: usize,
    dcount: usize,
    hop: usize,
    lp_a: f32,
    lp1: f32,
    lp2: f32,
    dacc: f32,
    dec: Vec<f32>,
    dpos: usize,
    tmin: usize,
    tmax: usize,
    scratch: Vec<f32>,
    cmnd: Vec<f32>,
    // tracker
    voiced: bool,
    hang: u32,
    hist: [f32; 3],
    slow: f32,
    period: f32,
    ratio_tgt: f32,
    last_pub: u32,
    // shifter
    buf: Vec<f32>,
    w: usize,
    ratio: f32,
    d: f32,     // read delay in samples behind the newest sample
    d_old: f32, // previous read head while crossfading
    dlo: f32,
    dmax: f32,
    fade: usize,
    fade_len: usize,
}

#[inline]
fn cubic(ym1: f32, y0: f32, y1: f32, y2: f32, f: f32) -> f32 {
    let c1 = 0.5 * (y1 - ym1);
    let c2 = ym1 - 2.5 * y0 + 2.0 * y1 - 0.5 * y2;
    let c3 = 0.5 * (y2 - ym1) + 1.5 * (y0 - y1);
    ((c3 * f + c2) * f + c1) * f + y0
}

impl Autotune {
    pub(super) fn new(sr: f32) -> Self {
        let dfac = ((sr / 8000.0).round() as usize).max(1);
        let dsr = sr / dfac as f32;
        let tmin = ((dsr / F_MAX) as usize).max(2);
        let tmax = ((dsr / F_MIN) as usize + 2).min(DEC_N - WIN - 2);
        let blen = (sr * 0.1) as usize + 16;
        let mut a = Self {
            sr,
            on: false,
            amount: 0.0,
            hum: 0.0,
            key: 0,
            mask: 0xFFF,
            coef_speed: 0.1,
            coef_rel: 1.0 - (-1.0 / (0.03 * sr)).exp(),
            a_slow: 0.1,
            dfac,
            dcount: 0,
            hop: 0,
            lp_a: 1.0 - (-2.0 * PI * 1500.0 / sr).exp(),
            lp1: 0.0,
            lp2: 0.0,
            dacc: 0.0,
            dec: vec![0.0; DEC_N],
            dpos: 0,
            tmin,
            tmax,
            scratch: vec![0.0; WIN + tmax + 1],
            cmnd: vec![1.0; tmax + 2],
            voiced: false,
            hang: 0,
            hist: [69.0; 3],
            slow: 69.0,
            period: sr / 200.0,
            ratio_tgt: 1.0,
            last_pub: u32::MAX,
            buf: vec![0.0; blen],
            w: 0,
            ratio: 1.0,
            d: 0.0,
            d_old: 0.0,
            dlo: 0.003 * sr,
            dmax: blen as f32 - 8.0,
            fade: 0,
            fade_len: ((0.0015 * sr) as usize).max(8),
        };
        a.reset();
        a
    }

    fn reset(&mut self) {
        self.buf.fill(0.0);
        self.dec.fill(0.0);
        self.w = 0;
        self.dpos = 0;
        self.dcount = 0;
        self.hop = 0;
        self.dacc = 0.0;
        self.lp1 = 0.0;
        self.lp2 = 0.0;
        self.voiced = false;
        self.hang = 0;
        self.ratio = 1.0;
        self.ratio_tgt = 1.0;
        self.period = self.sr / 200.0;
        self.d = self.dlo + self.period;
        self.d_old = self.d;
        self.fade = 0;
        self.last_pub = u32::MAX;
    }

    /// Called from update_derived. Returns whether autotune is on.
    pub(super) fn configure(&mut self, amount: f32, speed_ms: f32, key: u8, scale: u8, hum: f32) -> bool {
        let on = amount > 0.01;
        if on && !self.on {
            self.reset();
        }
        self.on = on;
        self.amount = amount.clamp(0.0, 1.0);
        self.hum = hum.clamp(0.0, 1.0);
        self.key = (key as i32).min(11);
        let sc = SCALES[(scale as usize).min(SCALES.len() - 1)];
        self.mask = sc.iter().fold(0u32, |m, &d| m | (1u32 << d));
        self.coef_speed = 1.0 - (-1.0 / (speed_ms.max(2.0) * 0.001 * self.sr)).exp();
        let dt = (HOP * self.dfac) as f32 / self.sr;
        self.a_slow = 1.0 - (-dt / (0.02 + 0.13 * self.hum)).exp();
        on
    }

    /// Nearest allowed (in-scale) MIDI note to the fractional pitch `p`.
    fn nearest(&self, p: f32) -> f32 {
        let f = p.floor() as i32;
        let ok = |n: i32| (self.mask >> ((n - self.key).rem_euclid(12) as u32)) & 1 != 0;
        let lo = (0..=12).map(|d| f - d).find(|&n| ok(n));
        let hi = (0..=12).map(|d| f + 1 + d).find(|&n| ok(n));
        match (lo, hi) {
            (Some(l), Some(h)) => {
                if p - l as f32 <= h as f32 - p {
                    l as f32
                } else {
                    h as f32
                }
            }
            (Some(l), None) => l as f32,
            (None, Some(h)) => h as f32,
            _ => p.round(),
        }
    }

    /// YIN on the decimated ring. Returns (period in full-rate samples, aperiodicity).
    fn detect(&mut self) -> Option<(f32, f32)> {
        let n = WIN + self.tmax + 1;
        let start = (self.dpos + DEC_N - n) & (DEC_N - 1);
        for i in 0..n {
            self.scratch[i] = self.dec[(start + i) & (DEC_N - 1)];
        }
        let s = &self.scratch;
        let e: f32 = s[n - WIN..].iter().map(|v| v * v).sum();
        if e < WIN as f32 * 1e-6 {
            return None; // below about -60 dBFS: silence
        }
        let mut run = 0.0f32;
        self.cmnd[0] = 1.0;
        for tau in 1..=self.tmax {
            let d: f32 = s[..WIN].iter().zip(&s[tau..tau + WIN]).map(|(a, b)| (a - b) * (a - b)).sum();
            run += d;
            self.cmnd[tau] = if run > 1e-12 { d * tau as f32 / run } else { 1.0 };
        }
        let mut tau = 0usize;
        let mut t = self.tmin;
        while t < self.tmax {
            if self.cmnd[t] < 0.15 {
                while t + 1 < self.tmax && self.cmnd[t + 1] < self.cmnd[t] {
                    t += 1;
                }
                tau = t;
                break;
            }
            t += 1;
        }
        if tau == 0 {
            let mut best = self.tmin;
            for t in self.tmin..self.tmax {
                if self.cmnd[t] < self.cmnd[best] {
                    best = t;
                }
            }
            if self.cmnd[best] > 0.35 {
                return None;
            }
            tau = best;
        }
        let (a, b, c) = (self.cmnd[tau - 1], self.cmnd[tau], self.cmnd[tau + 1]);
        let den = a - 2.0 * b + c;
        let off = if den.abs() > 1e-9 { (0.5 * (a - c) / den).clamp(-1.0, 1.0) } else { 0.0 };
        let period = ((tau as f32 + off) * self.dfac as f32).clamp(self.sr / 1100.0, self.sr / 60.0);
        Some((period, self.cmnd[tau]))
    }

    fn publish(&mut self, v: u32, out: &AtomicU32) {
        if v != self.last_pub {
            self.last_pub = v;
            out.store(v, Ordering::Relaxed);
        }
    }

    /// Runs every ~6 ms: detect, decide voiced/unvoiced, compute the target ratio.
    fn track(&mut self, out: &AtomicU32) {
        let thr = if self.voiced { 0.35 } else { 0.2 };
        match self.detect().filter(|d| d.1 < thr) {
            Some((period, _)) => {
                let midi = 69.0 + 12.0 * (self.sr / period / 440.0).log2();
                if !self.voiced {
                    self.hist = [midi; 3];
                    self.slow = midi;
                    self.period = period;
                } else {
                    self.hist = [self.hist[1], self.hist[2], midi];
                    self.period += (period - self.period) * 0.5;
                }
                let h = self.hist;
                let m = h[0].min(h[1]).max(h[0].max(h[1]).min(h[2])); // median of 3
                if (m - self.slow).abs() > 1.3 {
                    self.slow = m; // a new note, not vibrato
                } else {
                    self.slow += (m - self.slow) * self.a_slow;
                }
                let note = self.nearest(self.slow);
                let corr = (note + self.hum * (m - self.slow) - m) * self.amount;
                self.ratio_tgt = (corr * (1.0 / 12.0)).exp2().clamp(0.5, 2.0);
                self.voiced = true;
                self.hang = 0;
                let nn = (m.round() as i32).clamp(0, 127);
                let cents = ((m - m.round()) * 100.0).round() as i32;
                let v = (((nn + 1) as u32) << 8) | (cents as i8 as u8 as u32);
                self.publish(v, out);
            }
            None => {
                if self.voiced {
                    self.hang += 1; // bridge short drop-outs, keep the old ratio
                    if self.hang > 3 {
                        self.voiced = false;
                    }
                }
                if !self.voiced {
                    self.ratio_tgt = 1.0;
                    self.publish(0, out);
                }
            }
        }
    }

    fn feed(&mut self, x: f32, out: &AtomicU32) {
        self.lp1 += (x - self.lp1) * self.lp_a;
        self.lp2 += (self.lp1 - self.lp2) * self.lp_a;
        self.dacc += self.lp2;
        self.dcount += 1;
        if self.dcount >= self.dfac {
            self.dec[self.dpos] = self.dacc / self.dfac as f32;
            self.dpos = (self.dpos + 1) & (DEC_N - 1);
            self.dacc = 0.0;
            self.dcount = 0;
            self.hop += 1;
            if self.hop >= HOP {
                self.hop = 0;
                self.track(out);
            }
        }
    }

    #[inline]
    fn read(&self, newest: usize, d: f32) -> f32 {
        let n = self.buf.len();
        let id = d as usize; // d >= 2, so this is floor
        let f = d - id as f32;
        let at = |k: usize| {
            let i = newest + n - k;
            self.buf[if i >= n { i - n } else { i }]
        };
        cubic(at(id - 1), at(id), at(id + 1), at(id + 2), f)
    }

    pub(super) fn process(&mut self, x: f32, out: &AtomicU32) -> f32 {
        let x = if x.is_finite() { x } else { 0.0 };
        self.feed(x, out);

        let newest = self.w;
        self.buf[newest] = x;
        self.w = if newest + 1 == self.buf.len() { 0 } else { newest + 1 };

        // Smooth the ratio (retune speed; slower release when the voice stops).
        let c = if self.voiced { self.coef_speed } else { self.coef_rel };
        self.ratio += (self.ratio_tgt - self.ratio) * c;
        let dd = 1.0 - self.ratio;
        self.d += dd;
        self.d_old += dd;

        // Delay left its window: jump one whole period (repeat or skip) and crossfade.
        if self.voiced && self.fade == 0 {
            let dhi = self.dlo + 2.0 * self.period;
            if self.d > dhi {
                self.d_old = self.d;
                self.d -= self.period;
                self.fade = self.fade_len;
            } else if self.d < self.dlo {
                self.d_old = self.d;
                self.d += self.period;
                self.fade = self.fade_len;
            }
        }
        self.d = self.d.clamp(2.0, self.dmax);
        self.d_old = self.d_old.clamp(2.0, self.dmax);

        let y_new = self.read(newest, self.d);
        if self.fade > 0 {
            let a = 1.0 - self.fade as f32 / self.fade_len as f32;
            self.fade -= 1;
            let y_old = self.read(newest, self.d_old);
            y_old + (y_new - y_old) * a
        } else {
            y_new
        }
    }
}