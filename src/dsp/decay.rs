//! Cumulative spectral decay ("waterfall") of the bass: how each frequency fades after the
//! sound stops, and which ones keep ringing. Informational only; the EQ does not use it.

use std::f64::consts::PI;

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;
use serde::{Deserialize, Serialize};

use super::spectrum::{Response, Smoothing, smooth_power};

/// Highest frequency the waterfall covers, Hz; above it rooms do not ring audibly.
pub const TOP_HZ: f64 = 500.0;
/// Time between slices, ms.
pub const SLICE_STEP_MS: f64 = 20.0;
/// Number of slices: 0 to 400 ms.
pub const SLICES: usize = 21;
/// Length of the analysis window that slides along the impulse response, ms.
const WINDOW_MS: f64 = 300.0;
/// A rough waterfall, from an impulse response only a third of a second long (a short
/// looped sweep): a 120 ms window and slices to 120 ms, so the last window ends before the
/// sweep's distortion, which wraps around to the end of the loop, about 270 ms in.
const SHORT_WINDOW_MS: f64 = 120.0;
const SHORT_SLICES: usize = 7;
/// Its rising edge, ms. A short edge cuts into a ringing note mid-cycle and spreads its
/// energy over neighbouring frequencies, which then seem to ring too; 30 ms keeps that
/// spread within about a third of an octave at 100 Hz.
const RISE_MS: f64 = 30.0;
/// The last part of the window fades out, as a share of it.
const TAPER_SHARE: f64 = 0.5;
/// FFT length: 0.68 s at 48 kHz, longer than the window.
const FFT_LEN: usize = 1 << 15;
/// Smoothing of each slice.
const SMOOTHING_OCT: f64 = 1.0 / 24.0;
/// A frequency has "faded" once its level is this far below its start, dB.
pub const FADE_DB: f64 = 20.0;

/// The bass waterfall of one sweep (or several, power-averaged).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Waterfall {
    /// Frequencies, Hz (the analysis grid from 20 Hz to [`TOP_HZ`]).
    pub freqs: Vec<f64>,
    /// Start of each slice after the direct sound, ms.
    pub times_ms: Vec<f64>,
    /// Power per slice and frequency, uncalibrated (relative levels are what matter).
    pub power: Vec<Vec<f64>>,
}

/// A bass note that keeps sounding after the music stops.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ringing {
    pub hz: f64,
    /// Time to fade by [`FADE_DB`], ms.
    pub fade_ms: f64,
}

impl Waterfall {
    /// Slices of the impulse response `ir`, whose direct sound is at `peak`.
    #[must_use]
    pub fn new(ir: &[f64], peak: usize, rate: f64) -> Self {
        Self::with_window(ir, peak, rate, WINDOW_MS, SLICES)
    }

    /// A rough waterfall from a short circular impulse response (a third of a second): a
    /// shorter window, so coarser in frequency (about 8 Hz), and only the first 120 ms. A
    /// room ringing longer than the loop also wraps around into it, so the longest notes
    /// look as if they faded less than they do.
    #[must_use]
    pub fn short(ir: &[f64], peak: usize, rate: f64) -> Self {
        Self::with_window(ir, peak, rate, SHORT_WINDOW_MS, SHORT_SLICES)
    }

    fn with_window(ir: &[f64], peak: usize, rate: f64, window_ms: f64, slices: usize) -> Self {
        let n = ir.len();
        let ms = |m: f64| (m * rate / 1000.0).round() as usize;
        let (len, rise) = (ms(window_ms).min(FFT_LEN), ms(RISE_MS).max(1));
        let taper = (len as f64 * TAPER_SHARE) as usize;
        let window: Vec<f64> = (0..len)
            .map(|i| {
                if i < rise {
                    0.5 - 0.5 * (PI * i as f64 / rise as f64).cos()
                } else if i >= len - taper {
                    0.5 + 0.5 * (PI * (i - (len - taper)) as f64 / taper as f64).cos()
                } else {
                    1.0
                }
            })
            .collect();
        let mut planner = RealFftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(FFT_LEN);
        let bin_hz = rate / FFT_LEN as f64;
        let mut freqs = Vec::new();
        let times_ms: Vec<f64> = (0..slices).map(|k| k as f64 * SLICE_STEP_MS).collect();
        let power = times_ms
            .iter()
            .map(|&t| {
                // Start a little before the direct sound for the first slice, so the
                // window's rise does not cut into it.
                let start = (peak + n + ms(t)).saturating_sub(rise) % n;
                let mut buf = vec![0.0; FFT_LEN];
                for (i, (b, w)) in buf.iter_mut().zip(&window).enumerate() {
                    *b = ir[(start + i) % n] * w;
                }
                let mut out = fft.make_output_vec();
                fft.process(&mut buf, &mut out)
                    .expect("buffer sizes match the plan");
                let spectrum: Vec<f64> = out.iter().map(Complex::norm_sqr).collect();
                let r = smooth_power(&spectrum, bin_hz, Smoothing::Fixed(SMOOTHING_OCT));
                let keep = r.freqs.partition_point(|&f| f <= TOP_HZ);
                if freqs.is_empty() {
                    freqs = r.freqs[..keep].to_vec();
                }
                r.db[..keep]
                    .iter()
                    .map(|&d| super::db_to_power(d))
                    .collect()
            })
            .collect();
        Self {
            freqs,
            times_ms,
            power,
        }
    }

    /// Power average of several waterfalls of the same shape (mic positions).
    #[must_use]
    pub fn average(all: &[&Self]) -> Option<Self> {
        let first = *all.first()?;
        let n = all.len() as f64;
        let power = (0..first.power.len())
            .map(|k| {
                (0..first.freqs.len())
                    .map(|i| all.iter().map(|w| w.power[k][i]).sum::<f64>() / n)
                    .collect()
            })
            .collect();
        Some(Self {
            freqs: first.freqs.clone(),
            times_ms: first.times_ms.clone(),
            power,
        })
    }

    /// The loudest point of the first slice, as power.
    #[must_use]
    pub fn peak(&self) -> f64 {
        self.power[0].iter().copied().fold(0.0, f64::max)
    }

    /// Slice `k` in dB relative to the loudest point of the first slice.
    #[must_use]
    pub fn slice_db(&self, k: usize) -> Response {
        self.slice_db_from(k, self.peak())
    }

    /// Slice `k` in dB relative to the power `top`, so that waterfalls measured the same way
    /// (both speakers) share one scale.
    #[must_use]
    pub fn slice_db_from(&self, k: usize, top: f64) -> Response {
        Response {
            freqs: self.freqs.clone(),
            db: self.power[k]
                .iter()
                .map(|&p| super::power_to_db(p / top.max(1e-30)))
                .collect(),
        }
    }

    /// Time each frequency takes to fade by [`FADE_DB`], ms, interpolated between slices
    /// (the last slice's time when it has not faded by then). The fall is counted from
    /// the loudest start within a third of an octave: next to a ringing note, its skirt
    /// outlasts the quieter direct sound there, but the note itself is what rings.
    #[must_use]
    pub fn fade_ms(&self) -> Vec<f64> {
        let start: Vec<f64> = self.power[0]
            .iter()
            .map(|&p| super::power_to_db(p))
            .collect();
        let reach = (super::spectrum::POINTS_PER_OCTAVE / 3.0) as usize;
        (0..self.freqs.len())
            .map(|i| {
                let from = i.saturating_sub(reach);
                let to = (i + reach + 1).min(start.len());
                let reference = start[from..to].iter().copied().fold(f64::MIN, f64::max);
                let mut prev = (0.0, reference - start[i]);
                for (k, slice) in self.power.iter().enumerate() {
                    let drop = reference - super::power_to_db(slice[i]);
                    let t = self.times_ms[k];
                    if drop >= FADE_DB {
                        if k == 0 {
                            return 0.0;
                        }
                        let share = (FADE_DB - prev.1) / (drop - prev.1).max(1e-9);
                        return prev.0 + share * (t - prev.0);
                    }
                    prev = (t, drop);
                }
                self.times_ms.last().copied().unwrap_or(0.0)
            })
            .collect()
    }

    /// Median fade time between `lo` and `hi` Hz, ms.
    #[must_use]
    pub fn median_fade_ms(&self, lo: f64, hi: f64) -> Option<f64> {
        let mut v: Vec<f64> = self
            .freqs
            .iter()
            .zip(self.fade_ms())
            .filter(|(f, _)| (lo..=hi).contains(*f))
            .map(|(_, t)| t)
            .collect();
        if v.is_empty() {
            return None;
        }
        v.sort_by(f64::total_cmp);
        Some(v[v.len() / 2])
    }

    /// Frequencies that keep ringing: local peaks of the fade time between `lo` and
    /// 300 Hz that last at least 1.5 times the median and over 100 ms, longest first.
    #[must_use]
    pub fn ringing(&self, lo: f64) -> Vec<Ringing> {
        let fade = self.fade_ms();
        let Some(median) = self.median_fade_ms(lo, 300.0) else {
            return Vec::new();
        };
        let threshold = (median * 1.5).max(100.0);
        // Peaks at least a sixth of an octave apart.
        let apart = (super::spectrum::POINTS_PER_OCTAVE / 6.0) as usize;
        let mut found: Vec<Ringing> = Vec::new();
        for i in 0..fade.len() {
            let f = self.freqs[i];
            if !(lo..=300.0).contains(&f) || fade[i] < threshold {
                continue;
            }
            let from = i.saturating_sub(apart);
            let to = (i + apart + 1).min(fade.len());
            if fade[from..to].iter().any(|&t| t > fade[i]) {
                continue;
            }
            // Halfway through its fade a ringing note is a peak in its slice; the skirt of a
            // louder note next to it only slopes towards that one.
            let k = self
                .times_ms
                .partition_point(|&t| t < fade[i] / 2.0)
                .min(self.power.len() - 1);
            if self.power[k][from..to]
                .iter()
                .any(|&p| p > self.power[k][i])
            {
                continue;
            }
            if found.iter().any(|r| (r.hz / f).log2().abs() < 1.0 / 6.0) {
                continue;
            }
            found.push(Ringing {
                hz: f,
                fade_ms: fade[i],
            });
        }
        found.sort_by(|a, b| b.fade_ms.total_cmp(&a.fade_ms));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Rng;

    const RATE: f64 = 48_000.0;

    /// A room: the direct sound, reverberation fading 60 dB in 0.3 s, and a 60 Hz mode
    /// fading 60 dB in `t60` s.
    fn ir_with_mode(t60: f64) -> (Vec<f64>, usize) {
        let n = 1 << 17;
        let peak = 4800;
        let mut rng = Rng::new(5);
        let mut ir: Vec<f64> = (0..n).map(|_| rng.next_gaussian() * 1e-7).collect();
        ir[peak] += 1.0;
        for (i, v) in ir.iter_mut().enumerate().skip(peak) {
            let t = (i - peak) as f64 / RATE;
            *v += 0.01 * rng.next_gaussian() * 10f64.powf(-3.0 * t / 0.3);
            *v += 0.01 * (std::f64::consts::TAU * 60.0 * t).sin() * 10f64.powf(-3.0 * t / t60);
        }
        (ir, peak)
    }

    #[test]
    fn a_ringing_mode_is_found_with_its_fade_time() {
        let t60 = 0.9;
        let (ir, peak) = ir_with_mode(t60);
        let w = Waterfall::new(&ir, peak, RATE);
        assert_eq!(w.power.len(), SLICES);
        assert!(w.freqs.last().is_some_and(|&f| f <= TOP_HZ));
        let ringing = w.ringing(30.0);
        let first = ringing.first().expect("the mode rings");
        assert!((first.hz / 60.0).log2().abs() < 1.0 / 12.0, "{ringing:?}");
        // An exponential decay falls 20 dB in a third of its 60 dB time.
        let expected = t60 * 1000.0 * FADE_DB / 60.0;
        assert!(
            (first.fade_ms - expected).abs() < 30.0,
            "{:.0} ms, expected {expected:.0}",
            first.fade_ms
        );
        // Away from the mode, the direct sound dominates the start, and once the window has
        // passed it the reverberation is already 20 dB down.
        let fade = w.fade_ms();
        let at = |hz: f64| fade[w.freqs.partition_point(|&f| f < hz)];
        for hz in [100.0, 150.0, 200.0, 300.0] {
            assert!(at(hz) < 80.0, "{hz} Hz: {:.0} ms", at(hz));
        }
        assert_eq!(ringing.len(), 1, "{ringing:?}");
    }

    #[test]
    fn slices_fall_over_time_and_average_keeps_shape() {
        let (ir, peak) = ir_with_mode(0.6);
        let w = Waterfall::new(&ir, peak, RATE);
        let i = w.freqs.partition_point(|&f| f < 60.0);
        let first = w.slice_db(0).db[i];
        let last = w.slice_db(SLICES - 1).db[i];
        assert!(first > last + 10.0, "{first} → {last}");
        let avg = Waterfall::average(&[&w, &w]).expect("two");
        assert_eq!(avg, w);
    }
}
