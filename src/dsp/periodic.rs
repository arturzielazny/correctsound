//! Periodic noise, for live measurements that update several times a second.
//!
//! The noise is a stretch of band-limited pink noise played in a seamless loop. Every
//! frequency in it is a whole multiple of the loop rate, so once the room has settled, any
//! one period of the recording holds the room's response at exactly those frequencies:
//! `Y(k) = H(k)·X(k)`. Where the period starts only changes the phase, so the magnitude
//! needs no alignment between playback and recording, and comparing powers (not phases)
//! makes the small clock difference between a USB mic and the sound card harmless.
//!
//! The price of a short period is coarser frequency resolution (the bin width is the loop
//! rate): 2.9 Hz for a 0.34 s period at 48 kHz, enough for placement at 1/12 octave.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use super::{Rng, db_to_amplitude};

/// Noise that repeats every `len` samples, with known power per frequency bin.
#[derive(Debug, Clone)]
pub struct PeriodicNoise {
    samples: Vec<f64>,
    /// Power of the played samples per FFT bin (`len / 2 + 1` bins).
    power: Vec<f64>,
    band: (f64, f64),
    rate: f64,
}

/// Bins with less power than this share of the strongest carry no noise: no response.
const EMPTY_BIN: f64 = 1e-9;

/// Power spectrum of one period.
fn power_spectrum(samples: impl Iterator<Item = f64>, len: usize) -> Vec<f64> {
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(len);
    let mut buf: Vec<f64> = samples.collect();
    let mut out = fft.make_output_vec();
    fft.process(&mut buf, &mut out)
        .expect("buffer sizes match the plan");
    out.iter().map(Complex::norm_sqr).collect()
}

impl PeriodicNoise {
    /// Pink noise between `band.0` and `band.1` Hz, repeating every `len` samples, scaled
    /// so its highest sample is at `peak_dbfs`.
    #[must_use]
    pub fn new(len: usize, band: (f64, f64), peak_dbfs: f64, rate: f64) -> Self {
        let mut planner = RealFftPlanner::<f64>::new();
        let ifft = planner.plan_fft_inverse(len);
        let mut spectrum = ifft.make_input_vec();
        let bin_hz = rate / len as f64;
        let last = spectrum.len() - 1;
        // A fixed seed: the same noise every time, so nothing about it varies by chance.
        let mut rng = Rng::new(0x5EED_0FA1);
        for (k, s) in spectrum.iter_mut().enumerate() {
            let f = k as f64 * bin_hz;
            if k > 0 && k < last && (band.0..=band.1).contains(&f) {
                // Pink: power falls 3 dB per octave.
                *s = Complex::from_polar(1.0 / f.sqrt(), std::f64::consts::TAU * rng.next_f64());
            }
        }
        let mut samples = ifft.make_output_vec();
        ifft.process(&mut spectrum, &mut samples)
            .expect("buffer sizes match the plan");
        let peak = samples.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let gain = db_to_amplitude(peak_dbfs) / peak.max(1e-30);
        // What is played is f32; its spectrum is the reference.
        let samples: Vec<f64> = samples
            .iter()
            .map(|v| f64::from((v * gain) as f32))
            .collect();
        let power = power_spectrum(samples.iter().copied(), len);
        Self {
            samples,
            power,
            band,
            rate,
        }
    }

    /// Samples in one period.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// True for a period of no samples (never, for a constructed noise).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// One period, for playing in a loop.
    #[must_use]
    pub fn samples_f32(&self) -> Vec<f32> {
        self.samples.iter().map(|&v| v as f32).collect()
    }

    /// Width of one frequency bin, Hz: the loop rate.
    #[must_use]
    pub fn bin_hz(&self) -> f64 {
        self.rate / self.len() as f64
    }

    /// The response's power `|H|²` per bin from exactly one period of recording made once
    /// the room has settled; zero outside the noise's band.
    ///
    /// # Panics
    ///
    /// If `period` is not one period long.
    #[must_use]
    pub fn response_power(&self, period: &[f32]) -> Vec<f64> {
        assert_eq!(period.len(), self.len(), "one period of recording");
        let y = power_spectrum(period.iter().map(|&v| f64::from(v)), self.len());
        let floor = self.power.iter().copied().fold(0.0, f64::max) * EMPTY_BIN;
        y.iter()
            .zip(&self.power)
            .map(|(&y, &x)| if x > floor { y / x } else { 0.0 })
            .collect()
    }

    /// How far the recording's level inside the noise's band stands above its level in a
    /// band well above it (1–3 kHz), per bin, dB: low when the noise does not reach the mic.
    ///
    /// # Panics
    ///
    /// If `period` is not one period long.
    #[must_use]
    pub fn reaches_mic_db(&self, period: &[f32]) -> f64 {
        assert_eq!(period.len(), self.len(), "one period of recording");
        let y = power_spectrum(period.iter().map(|&v| f64::from(v)), self.len());
        let bin_hz = self.bin_hz();
        let mean = |lo: f64, hi: f64| {
            let (a, b) = (
                (lo / bin_hz) as usize,
                ((hi / bin_hz) as usize).min(y.len()),
            );
            y[a..b].iter().sum::<f64>() / (b - a).max(1) as f64
        };
        let inside = mean(self.band.0.max(40.0), self.band.1.min(400.0));
        super::power_to_db(inside / mean(1000.0, 3000.0).max(1e-30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::biquad::{Biquad, BiquadState};

    const RATE: f64 = 48_000.0;
    const LEN: usize = 1 << 14;

    /// Plays the noise through `filter` for `periods` and returns the last period.
    fn through(noise: &PeriodicNoise, filter: &Biquad, periods: usize, shift: usize) -> Vec<f32> {
        let one = noise.samples_f32();
        let mut state = BiquadState::default();
        let out: Vec<f32> = (0..periods * LEN + shift)
            .map(|i| filter.tick(&mut state, f64::from(one[i % LEN])) as f32)
            .collect();
        out[out.len() - LEN..].to_vec()
    }

    #[test]
    fn recovers_a_resonance_wherever_the_period_starts() {
        let noise = PeriodicNoise::new(LEN, (15.0, 600.0), -12.0, RATE);
        let peak = noise
            .samples_f32()
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((f64::from(peak) - db_to_amplitude(-12.0)).abs() < 1e-3);
        // A +10 dB resonance at 60 Hz, Q 8: a room mode.
        let mode = Biquad::peaking(RATE, 60.0, 10.0, 8.0);
        let bin_hz = noise.bin_hz();
        for shift in [0, 1234, 9000] {
            let power = noise.response_power(&through(&noise, &mode, 6, shift));
            for f in [30.0, 60.0, 100.0, 300.0] {
                let k = (f / bin_hz).round() as usize;
                let expected = mode.magnitude_db(RATE, k as f64 * bin_hz);
                let got = crate::dsp::power_to_db(power[k]);
                assert!((got - expected).abs() < 0.05, "{f} Hz: {got} vs {expected}");
            }
            // Outside the band there is no response.
            assert_eq!(power[(2000.0 / bin_hz) as usize], 0.0);
        }
    }

    #[test]
    fn tells_whether_the_noise_reaches_the_mic() {
        let noise = PeriodicNoise::new(LEN, (15.0, 600.0), -12.0, RATE);
        let mut rng = Rng::new(7);
        let hiss: Vec<f32> = (0..LEN)
            .map(|_| (1e-4 * rng.next_gaussian()) as f32)
            .collect();
        let heard: Vec<f32> = noise
            .samples_f32()
            .iter()
            .zip(&hiss)
            .map(|(s, h)| s * 0.1 + h)
            .collect();
        assert!(noise.reaches_mic_db(&heard) > 30.0);
        assert!(noise.reaches_mic_db(&hiss).abs() < 6.0);
    }
}
