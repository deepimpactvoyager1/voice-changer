//! Biquad filter, fractional delay reads, pitch-shift tap and denormal flushing.

use std::f32::consts::PI;

/// Second-order filter (Butterworth low/high-pass, peaking EQ, shelves).
#[derive(Clone, Copy, Default)]
pub(super) struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn set_norm(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    pub(super) fn set(&mut self, lowpass: bool, fc: f32, sr: f32) {
        let w0 = 2.0 * PI * fc.clamp(10.0, sr * 0.45) / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * 0.7071);
        let (b0, b1, b2) = if lowpass {
            ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0)
        } else {
            ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0)
        };
        self.set_norm(b0, b1, b2, 1.0 + alpha, -2.0 * cos, 1.0 - alpha);
    }

    pub(super) fn peak(&mut self, fc: f32, gain_db: f32, q: f32, sr: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * fc.clamp(10.0, sr * 0.45) / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        self.set_norm(1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a);
    }

    pub(super) fn shelf(&mut self, high: bool, fc: f32, gain_db: f32, sr: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * fc.clamp(10.0, sr * 0.45) / sr;
        let (sin, cos) = w0.sin_cos();
        let beta = 2.0 * a.sqrt() * (sin * 0.5 * 2f32.sqrt());
        if high {
            self.set_norm(
                a * ((a + 1.0) + (a - 1.0) * cos + beta),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - beta),
                (a + 1.0) - (a - 1.0) * cos + beta,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - beta,
            );
        } else {
            self.set_norm(
                a * ((a + 1.0) - (a - 1.0) * cos + beta),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - beta),
                (a + 1.0) + (a - 1.0) * cos + beta,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - beta,
            );
        }
    }

    pub(super) fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Reads a fractionally delayed sample from a circular buffer. `write` is the
/// index of the NEXT write, so the newest stored sample sits at `write - 1`.
pub(super) fn read_frac(buf: &[f32], write: usize, delay: f32) -> f32 {
    let len = buf.len();
    let pos = (write as f32 - 1.0 - delay).rem_euclid(len as f32);
    let fl = pos.floor();
    let i0 = fl as usize % len;
    let i1 = (i0 + 1) % len;
    let frac = pos - fl;
    buf[i0] * (1.0 - frac) + buf[i1] * frac
}

/// One pitch-shifted tap: two delay-line readers half a grain apart, cross-faded
/// with a Hann window (this is the classic cheap pitch shifter).
pub(super) fn shift_tap(buf: &[f32], write: usize, phase: f32, window: f32) -> f32 {
    let pb = (phase + 0.5).rem_euclid(1.0);
    let wa = 0.5 - 0.5 * (2.0 * PI * phase).cos();
    read_frac(buf, write, phase * window) * wa + read_frac(buf, write, pb * window) * (1.0 - wa)
}

/// Flush denormal floats to zero on this thread. Denormals make filter and
/// echo tails up to ~100x slower on some CPUs, which steals time from the
/// whole PC (this is a classic cause of audio apps making other apps lag).
#[cfg(target_arch = "x86_64")]
#[allow(deprecated, unused_unsafe)]
fn set_ftz() {
    unsafe {
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        _mm_setcsr(_mm_getcsr() | 0x8040); // FTZ (bit 15) + DAZ (bit 6)
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn set_ftz() {}

pub(super) fn enable_ftz() {
    set_ftz();
}
