//! FFT and the spectral noise suppressor.

use std::f32::consts::PI;

// ----- FFT + spectral noise suppressor ------------------------------------

pub(super) const NS_N: usize = 512; // FFT size
const NS_HOP: usize = 256; // 50 % overlap -> about 10 ms of delay at 48 kHz
const FM_W: usize = 2; // envelope smoothing: +-2 bins (about +-190 Hz at 48 kHz)
const FM_MIN: f32 = 0.05; // formant gain limits, kept only for stability
const FM_MAX: f32 = 10.0;

pub(super) struct Fft {
    rev: Vec<usize>,
    wr: Vec<f32>,
    wi: Vec<f32>,
}

impl Fft {
    pub(super) fn new(n: usize) -> Self {
        let bits = n.trailing_zeros();
        let rev: Vec<usize> = (0..n).map(|i| i.reverse_bits() >> (usize::BITS - bits)).collect();
        let wr: Vec<f32> = (0..n / 2).map(|k| (2.0 * PI * k as f32 / n as f32).cos()).collect();
        let wi: Vec<f32> = (0..n / 2).map(|k| -(2.0 * PI * k as f32 / n as f32).sin()).collect();
        Self { rev, wr, wi }
    }

    /// In-place radix-2 FFT. `inverse` also scales by 1/n.
    pub(super) fn run(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let n = self.rev.len();
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut start = 0;
            while start < n {
                for k in 0..half {
                    let wr = self.wr[k * step];
                    let wi = if inverse { -self.wi[k * step] } else { self.wi[k * step] };
                    let a = start + k;
                    let b = a + half;
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
                start += len;
            }
            len <<= 1;
        }
        if inverse {
            let s = 1.0 / n as f32;
            for v in re.iter_mut() {
                *v *= s;
            }
            for v in im.iter_mut() {
                *v *= s;
            }
        }
    }
}

/// Spectral noise suppressor. It tracks the steady background noise in every
/// frequency band (fans, hiss, hum, room tone) and turns those bands down
/// whenever your voice is not louder than the noise there. It never mutes
/// completely: the gain floor stops the "cut out" effect of hard gates.
pub(super) struct NoiseSuppressor {
    env: Vec<f32>,
    fm_g: Vec<f32>,
    fft: Fft,
    win: Vec<f32>,
    ring: Vec<f32>,  // last NS_N input samples (circular)
    pos: usize,      // next write index in `ring`
    count: usize,    // samples since the last frame
    acc: Vec<f32>,   // overlap-add accumulator
    ready: Vec<f32>, // finished samples currently being played out
    re: Vec<f32>,
    im: Vec<f32>,
    ps: Vec<f32>,    // smoothed power per bin
    noise: Vec<f32>, // noise power estimate per bin
    graw: Vec<f32>,  // gain before smoothing
    gain: Vec<f32>,  // smoothed gain actually applied
    primed: bool,
}

impl NoiseSuppressor {
    pub(super) fn new() -> Self {
        let half = NS_N / 2 + 1;
        Self {
            env: vec![1.0; half], fm_g: vec![1.0; half],
            fft: Fft::new(NS_N),
            win: (0..NS_N).map(|i| (PI * (i as f32 + 0.5) / NS_N as f32).sin()).collect(),
            ring: vec![0.0; NS_N],
            pos: 0,
            count: 0,
            acc: vec![0.0; NS_N],
            ready: vec![0.0; NS_HOP],
            re: vec![0.0; NS_N],
            im: vec![0.0; NS_N],
            ps: vec![0.0; half],
            noise: vec![0.0; half],
            graw: vec![1.0; half],
            gain: vec![1.0; half],
            primed: false,
        }
    }

    pub(super) fn reset(&mut self) {
        self.fm_g.fill(1.0);
        self.ring.fill(0.0);
        self.acc.fill(0.0);
        self.ready.fill(0.0);
        self.ps.fill(0.0);
        self.gain.fill(1.0);
        self.pos = 0;
        self.count = 0;
        self.primed = false;
    }

    /// `ns` = Some((alpha, floor)) when noise suppression is on; `fm_inv` = 1 / formant
    /// ratio, <= 0 when formant shift is off.
    pub(super) fn process(&mut self, x: f32, ns: Option<(f32, f32)>, fm_inv: f32) -> f32 {
        self.ring[self.pos] = x;
        self.pos = (self.pos + 1) % NS_N;
        let out = self.ready[self.count];
        self.count += 1;
        if self.count == NS_HOP {
            self.count = 0;
            self.frame(ns, fm_inv);
        }
        out
    }

    fn frame(&mut self, ns: Option<(f32, f32)>, fm_inv: f32) {
        let half = NS_N / 2;
        for i in 0..NS_N {
            let idx = (self.pos + i) % NS_N; // oldest sample is at `pos`
            self.re[i] = self.ring[idx] * self.win[i];
            self.im[i] = 0.0;
        }
        self.fft.run(&mut self.re, &mut self.im, false);

        if let Some((alpha, floor)) = ns {
            let mut total = 0.0f32;
            for k in 0..=half {
                let p = self.re[k] * self.re[k] + self.im[k] * self.im[k];
                self.ps[k] = 0.6 * self.ps[k] + 0.4 * p;
                total += self.ps[k];
                if !self.primed {
                    self.noise[k] = self.ps[k];
                }
                let calm = self.ps[k] < 2.5 * self.noise[k];
                self.noise[k] = if calm {
                    self.noise[k] + (self.ps[k] - self.noise[k]) * 0.05
                } else {
                    self.noise[k] * 1.0015
                };
                let xi = (self.ps[k] / self.noise[k].max(1e-12) - 1.0).max(0.0);
                self.graw[k] = (xi / (xi + alpha)).max(floor);
            }
            self.primed = total > 1e-5;
            for k in 0..=half {
                let l = self.graw[k.saturating_sub(1)];
                let r = self.graw[(k + 1).min(half)];
                let gs = 0.25 * l + 0.5 * self.graw[k] + 0.25 * r;
                let gp = self.gain[k];
                self.gain[k] = if gs > gp { gp + (gs - gp) * 0.7 } else { gp + (gs - gp) * 0.25 };
            }
        } else {
            self.primed = false;
            self.ps.fill(0.0);
            self.gain.fill(1.0);
        }

        if fm_inv > 0.0 {
            // Envelope = frequency-smoothed power. Warp it along the frequency axis and
            // apply (warped / original) as a per-bin gain. Phases are untouched, so the
            // pitch (fine structure) stays and only the formant envelope moves.
            for k in 0..=half {
                let lo = k.saturating_sub(FM_W);
                let hi = (k + FM_W).min(half);
                let mut sum = 0.0f32;
                for j in lo..=hi {
                    sum += self.re[j] * self.re[j] + self.im[j] * self.im[j];
                }
                self.env[k] = sum / (hi - lo + 1) as f32 + 1e-9;
            }
            for k in 0..=half {
                let src = k as f32 * fm_inv;
                let i = src as usize;
                let e = if i >= half {
                    self.env[half]
                } else {
                    let f = src - i as f32;
                    self.env[i] + (self.env[i + 1] - self.env[i]) * f
                };
                let gt = (e / self.env[k]).sqrt().clamp(FM_MIN, FM_MAX);
                self.fm_g[k] += (gt - self.fm_g[k]) * 0.5;
            }
        } else {
            self.fm_g.fill(1.0);
        }

        for k in 0..=half {
            let g = self.gain[k] * self.fm_g[k];
            self.re[k] *= g;
            self.im[k] *= g;
            if k > 0 && k < half {
                self.re[NS_N - k] *= g;
                self.im[NS_N - k] *= g;
            }
        }
        self.fft.run(&mut self.re, &mut self.im, true);

        for i in 0..NS_N {
            self.acc[i] += self.re[i] * self.win[i];
        }
        self.ready.copy_from_slice(&self.acc[..NS_HOP]);
        self.acc.copy_within(NS_HOP.., 0);
        for v in self.acc[NS_N - NS_HOP..].iter_mut() {
            *v = 0.0;
        }
    }
}
