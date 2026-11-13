//! RBJ "Audio EQ Cookbook" biquads, matching EasyEffects' `APO (DR)` band mode.

use std::f64::consts::{PI, TAU};

/// Normalised biquad coefficients (a0 = 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    /// Peaking (bell) filter.
    #[must_use]
    pub fn peaking(rate: f64, freq: f64, gain_db: f64, q: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = TAU * freq / rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        Self::normalise(
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        )
    }

    /// Second-order high-pass filter.
    #[must_use]
    pub fn high_pass(rate: f64, freq: f64, q: f64) -> Self {
        let w0 = TAU * freq / rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        Self::normalise(
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    /// Second-order low-pass filter.
    #[must_use]
    pub fn low_pass(rate: f64, freq: f64, q: f64) -> Self {
        let w0 = TAU * freq / rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        Self::normalise(
            (1.0 - cos) / 2.0,
            1.0 - cos,
            (1.0 - cos) / 2.0,
            1.0 + alpha,
            -2.0 * cos,
            1.0 - alpha,
        )
    }

    /// High-shelf filter with shelf slope S = 1.
    #[must_use]
    pub fn high_shelf(rate: f64, freq: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = TAU * freq / rate;
        let cos = w0.cos();
        let alpha = w0.sin() / 2.0 * 2f64.sqrt();
        let sq = 2.0 * a.sqrt() * alpha;
        Self::normalise(
            a * ((a + 1.0) + (a - 1.0) * cos + sq),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - sq),
            (a + 1.0) - (a - 1.0) * cos + sq,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - sq,
        )
    }

    fn normalise(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Magnitude response in dB at `freq`.
    #[must_use]
    pub fn magnitude_db(&self, rate: f64, freq: f64) -> f64 {
        let w = 2.0 * PI * freq / rate;
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -(self.b1 * s1 + self.b2 * s2);
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -(self.a1 * s1 + self.a2 * s2);
        let num = num_re * num_re + num_im * num_im;
        let den = den_re * den_re + den_im * den_im;
        10.0 * (num / den).max(1e-30).log10()
    }

    /// Filters `samples` in place (direct form II transposed, zero initial state).
    pub fn process(&self, samples: &mut [f64]) {
        let mut state = BiquadState::default();
        for x in samples.iter_mut() {
            *x = self.tick(&mut state, *x);
        }
    }

    /// Filters one sample, carrying `state` between calls.
    pub fn tick(&self, state: &mut BiquadState, x: f64) -> f64 {
        let y = self.b0 * x + state.z1;
        state.z1 = self.b1 * x - self.a1 * y + state.z2;
        state.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Delay-line state for [`Biquad::tick`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BiquadState {
    z1: f64,
    z2: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    #[test]
    fn peaking_has_full_gain_at_centre() {
        for (f, g, q) in [(100.0, -6.0, 4.0), (1000.0, 3.0, 1.0), (8000.0, -12.0, 0.7)] {
            let bq = Biquad::peaking(RATE, f, g, q);
            let got = bq.magnitude_db(RATE, f);
            assert!((got - g).abs() < 1e-9, "f={f} g={g}: got {got}");
        }
    }

    #[test]
    fn peaking_is_unity_far_away_and_at_dc() {
        let bq = Biquad::peaking(RATE, 1000.0, -10.0, 2.0);
        assert!(bq.magnitude_db(RATE, 1e-3).abs() < 1e-6);
        assert!(bq.magnitude_db(RATE, 20.0).abs() < 0.01);
    }

    #[test]
    fn peaking_matches_hand_computed_coefficients() {
        // f0 = 1 kHz, +6 dB, Q = 1 at 48 kHz, computed independently from the cookbook formulas.
        let bq = Biquad::peaking(RATE, 1000.0, 6.0, 1.0);
        let a = 10f64.powf(6.0 / 40.0);
        let w0 = TAU * 1000.0 / RATE;
        let alpha = w0.sin() / 2.0;
        let a0 = 1.0 + alpha / a;
        assert!((bq.b0 - (1.0 + alpha * a) / a0).abs() < 1e-12);
        assert!((bq.a2 - (1.0 - alpha / a) / a0).abs() < 1e-12);
        // Half-gain bandwidth: for Q=1 the -3 dB-of-6 points sit near f0 * 2^(±0.69).
        let edge = bq.magnitude_db(RATE, 1000.0 * 2f64.powf(0.6943));
        assert!((edge - 3.0).abs() < 0.15, "edge gain {edge}");
    }

    #[test]
    fn processing_matches_magnitude() {
        let bq = Biquad::peaking(RATE, 500.0, -9.0, 3.0);
        let n = 48_000;
        let mut x: Vec<f64> = (0..n)
            .map(|i| (TAU * 500.0 * i as f64 / RATE).sin())
            .collect();
        bq.process(&mut x);
        let rms = (x[n / 2..].iter().map(|v| v * v).sum::<f64>() / (n / 2) as f64).sqrt();
        let got = 20.0 * (rms * 2f64.sqrt()).log10();
        assert!((got + 9.0).abs() < 0.01, "got {got}");
    }
}
