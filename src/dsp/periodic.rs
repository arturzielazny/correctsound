//! Periodic signals, for measurements that update several times a second.
//!
//! The signal (a sweep) is played in a seamless loop. Every frequency in it is a whole
//! multiple of the loop rate, so once the room has settled, any one period of the recording
//! holds the room's response at exactly those frequencies: `Y(k) = H(k)·X(k)`. Where the
//! period starts only shifts the impulse response circularly, so no alignment between
//! playback and recording is needed.
//!
//! The price of a short period is coarser frequency resolution (the bin width is the loop
//! rate) and a limit on how long the room may ring before its tail wraps around into the
//! next period: a 1 s sweep gives 1 Hz bins and room for the waterfall.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use super::db_to_amplitude;

/// A signal that repeats every `len` samples, with known spectrum.
#[derive(Debug, Clone)]
pub struct Periodic {
    samples: Vec<f64>,
    /// Spectrum of the played samples (`len / 2 + 1` bins).
    spectrum: Vec<Complex<f64>>,
    band: (f64, f64),
    rate: f64,
}

/// Bins with less power than this share of the strongest carry no signal: no response.
const EMPTY_BIN: f64 = 1e-9;

fn spectrum(samples: impl Iterator<Item = f64>, len: usize) -> Vec<Complex<f64>> {
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(len);
    let mut buf: Vec<f64> = samples.collect();
    let mut out = fft.make_output_vec();
    fft.process(&mut buf, &mut out)
        .expect("buffer sizes match the plan");
    out
}

/// Scales `samples` so the highest is at `peak_dbfs`, rounded to what is played (f32).
fn to_level(samples: &[f64], peak_dbfs: f64) -> Vec<f64> {
    let peak = samples.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let gain = db_to_amplitude(peak_dbfs) / peak.max(1e-30);
    samples
        .iter()
        .map(|v| f64::from((v * gain) as f32))
        .collect()
}

impl Periodic {
    fn from_samples(samples: Vec<f64>, band: (f64, f64), rate: f64) -> Self {
        let spectrum = spectrum(samples.iter().copied(), samples.len());
        Self {
            samples,
            spectrum,
            band,
            rate,
        }
    }

    /// A logarithmic sweep from `band.0` to `band.1` Hz filling the whole period, so it
    /// loops straight into the next one, peaking at `peak_dbfs`. Its frequency rises
    /// continuously, so it is the smoothest periodic sweep; the jump from the top back to
    /// the bottom at the loop point is faded over a few milliseconds.
    #[must_use]
    pub fn sweep(len: usize, band: (f64, f64), peak_dbfs: f64, rate: f64) -> Self {
        let duration = len as f64 / rate;
        let l = duration / (band.1 / band.0).ln();
        let fade = (0.005 * rate) as usize;
        let samples: Vec<f64> = (0..len)
            .map(|i| {
                let t = i as f64 / rate;
                let v = (std::f64::consts::TAU * band.0 * l * ((t / l).exp() - 1.0)).sin();
                let edge = i.min(len - 1 - i);
                if edge < fade {
                    v * (0.5 - 0.5 * (std::f64::consts::PI * edge as f64 / fade as f64).cos())
                } else {
                    v
                }
            })
            .collect();
        Self::from_samples(to_level(&samples, peak_dbfs), band, rate)
    }

    /// Samples in one period.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// True for a period of no samples (never, for a constructed signal).
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

    fn floor(&self) -> f64 {
        self.spectrum
            .iter()
            .map(Complex::norm_sqr)
            .fold(0.0, f64::max)
            * EMPTY_BIN
    }

    /// The room's impulse response from one period of recording, circular: it is one
    /// period long, starts wherever the period happened to start, and the direct sound is
    /// its largest sample. Outside the signal's band it fades to nothing over a third of an
    /// octave, so the band edges do not ring.
    ///
    /// # Panics
    ///
    /// If `period` is not one period long.
    #[must_use]
    pub fn impulse_response(&self, period: &[f32]) -> Vec<f64> {
        assert_eq!(period.len(), self.len(), "one period of recording");
        let len = self.len();
        let mut y = spectrum(period.iter().map(|&v| f64::from(v)), len);
        let floor = self.floor();
        let bin_hz = self.bin_hz();
        let (lo, hi) = (self.band.0 * 1.2, self.band.1 / 1.05);
        let last = y.len() - 1;
        for (k, (yk, xk)) in y.iter_mut().zip(&self.spectrum).enumerate() {
            let f = k as f64 * bin_hz;
            // 1 inside the band, 0 a third of an octave outside it.
            let keep = 1.0 - (((lo / f).log2().max((f / hi).log2())) * 3.0).clamp(0.0, 1.0);
            let x = xk.norm_sqr();
            *yk = if k == 0 || k == last || x <= floor || keep <= 0.0 {
                Complex::new(0.0, 0.0)
            } else {
                *yk * xk.conj() / x * keep
            };
        }
        let mut planner = RealFftPlanner::<f64>::new();
        let ifft = planner.plan_fft_inverse(len);
        let mut ir = ifft.make_output_vec();
        ifft.process(&mut y, &mut ir)
            .expect("buffer sizes match the plan");
        let scale = 1.0 / len as f64;
        ir.iter_mut().for_each(|v| *v *= scale);
        ir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::biquad::{Biquad, BiquadState};

    const RATE: f64 = 48_000.0;

    /// Plays the signal through `filter` for `periods` and returns the last period.
    fn through(signal: &Periodic, filter: &Biquad, periods: usize, shift: usize) -> Vec<f32> {
        let one = signal.samples_f32();
        let len = one.len();
        let mut state = BiquadState::default();
        let out: Vec<f32> = (0..periods * len + shift)
            .map(|i| filter.tick(&mut state, f64::from(one[i % len])) as f32)
            .collect();
        out[out.len() - len..].to_vec()
    }

    #[test]
    fn recovers_a_resonance_wherever_the_period_starts() {
        let signal = Periodic::sweep(3 << 14, (10.0, 1000.0), -12.0, RATE);
        let peak = signal
            .samples_f32()
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((f64::from(peak) - db_to_amplitude(-12.0)).abs() < 1e-3);
        // A +10 dB resonance at 60 Hz, Q 8: a room mode.
        let mode = Biquad::peaking(RATE, 60.0, 10.0, 8.0);
        let bin_hz = signal.bin_hz();
        for shift in [0, 1234, 9000] {
            let ir = signal.impulse_response(&through(&signal, &mode, 6, shift));
            let h = spectrum(ir.into_iter(), signal.len());
            for f in [30.0, 60.0, 100.0, 300.0] {
                let k = (f / bin_hz).round() as usize;
                let expected = mode.magnitude_db(RATE, k as f64 * bin_hz);
                let got = crate::dsp::power_to_db(h[k].norm_sqr());
                assert!((got - expected).abs() < 0.05, "{f} Hz: {got} vs {expected}");
            }
        }
    }

    #[test]
    fn a_looped_sweep_gives_the_impulse_response() {
        let signal = Periodic::sweep(3 << 14, (10.0, 1000.0), -12.0, RATE);
        // A pure delay of 1000 samples and half the level: the impulse response is a
        // single spike there, wherever the recorded period starts.
        let one = signal.samples_f32();
        let len = one.len();
        for shift in [0, 5000] {
            let period: Vec<f32> = (0..len)
                .map(|i| 0.5 * one[(i + 2 * len + shift - 1000) % len])
                .collect();
            let ir = signal.impulse_response(&period);
            let (at, peak) = ir
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .expect("not empty");
            assert_eq!(at, (1000 + len - shift) % len);
            // Band-limited to 1 kHz, so the spike is spread over about 24 samples and its
            // height is about 0.5 · 2 kHz / 48 kHz.
            assert!(*peak > 0.015 && *peak < 0.025, "{peak}");
            let elsewhere = ir
                .iter()
                .enumerate()
                .filter(|(i, _)| i.abs_diff(at).min(len - i.abs_diff(at)) > 2400)
                .fold(0.0f64, |m, (_, v)| m.max(v.abs()));
            assert!(elsewhere < peak * 1e-2, "{elsewhere} vs {peak}");
        }
    }
}
