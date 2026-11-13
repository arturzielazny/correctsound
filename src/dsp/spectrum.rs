//! Log-frequency grids, fractional-octave smoothing and frequency responses.

use serde::{Deserialize, Serialize};

/// Points per octave of the analysis grid.
pub const POINTS_PER_OCTAVE: f64 = 96.0;
pub const GRID_LOW_HZ: f64 = 20.0;
pub const GRID_HIGH_HZ: f64 = 20_000.0;

/// The fixed log-spaced frequency grid every response is expressed on.
#[must_use]
pub fn grid() -> Vec<f64> {
    let octaves = (GRID_HIGH_HZ / GRID_LOW_HZ).log2();
    let n = (octaves * POINTS_PER_OCTAVE).floor() as usize + 1;
    (0..n)
        .map(|i| GRID_LOW_HZ * 2f64.powf(i as f64 / POINTS_PER_OCTAVE))
        .collect()
}

/// A magnitude response in dB on [`grid`] frequencies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub freqs: Vec<f64>,
    pub db: Vec<f64>,
}

impl Response {
    /// Linear-in-log-frequency interpolation, holding end values outside the grid.
    #[must_use]
    pub fn at(&self, freq: f64) -> f64 {
        interpolate_log(&self.freqs, &self.db, freq)
    }

    /// Mean dB value between two frequencies.
    #[must_use]
    pub fn mean_between(&self, lo: f64, hi: f64) -> f64 {
        let (sum, n) = self
            .freqs
            .iter()
            .zip(&self.db)
            .filter(|(f, _)| (lo..=hi).contains(*f))
            .fold((0.0, 0usize), |(s, n), (_, d)| (s + d, n + 1));
        if n == 0 {
            self.at((lo * hi).sqrt())
        } else {
            sum / n as f64
        }
    }

    /// Adds `offset` dB at every point.
    #[must_use]
    pub fn shifted(&self, offset: f64) -> Self {
        Self {
            freqs: self.freqs.clone(),
            db: self.db.iter().map(|d| d + offset).collect(),
        }
    }
}

/// Interpolates `ys` over ascending `xs` linearly in log(x); holds the end values outside.
#[must_use]
pub fn interpolate_log(xs: &[f64], ys: &[f64], x: f64) -> f64 {
    match xs.len() {
        0 => 0.0,
        1 => ys[0],
        _ if x <= xs[0] => ys[0],
        _ if x >= xs[xs.len() - 1] => ys[ys.len() - 1],
        _ => {
            let i = xs.partition_point(|&v| v <= x).max(1);
            let (x0, x1) = (xs[i - 1].ln(), xs[i].ln());
            let t = (x.ln() - x0) / (x1 - x0);
            ys[i - 1] + t * (ys[i] - ys[i - 1])
        }
    }
}

/// How much fractional-octave smoothing to apply at each frequency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Smoothing {
    /// Fixed width in octaves.
    Fixed(f64),
    /// 1/48 octave below 100 Hz widening log-linearly to 1/3 octave at 1 kHz and above.
    Variable,
}

impl Smoothing {
    #[must_use]
    pub fn width_octaves(self, freq: f64) -> f64 {
        match self {
            Self::Fixed(w) => w,
            Self::Variable => {
                let t = ((freq / 100.0).log10()).clamp(0.0, 1.0);
                let (lo, hi) = ((1.0f64 / 48.0).ln(), (1.0f64 / 3.0).ln());
                (lo + t * (hi - lo)).exp()
            }
        }
    }
}

/// Smooths a per-bin power spectrum onto the grid and returns it in dB.
///
/// Averages power over each fractional-octave band; where the band holds fewer than
/// two bins, the power is linearly interpolated at the band centre instead.
#[must_use]
pub fn smooth_power(power: &[f64], bin_hz: f64, smoothing: Smoothing) -> Response {
    // cum[k] = sum of power[0..k]; each bin covers [k - 0.5, k + 0.5).
    let mut cum = Vec::with_capacity(power.len() + 1);
    cum.push(0.0);
    for &p in power {
        cum.push(cum.last().copied().unwrap_or(0.0) + p);
    }
    let last = power.len() as f64 - 0.5;
    let integral = |x: f64| {
        let x = (x + 0.5).clamp(0.0, last + 0.5);
        let k = x.floor() as usize;
        let frac = x - k as f64;
        cum[k]
            + if k < power.len() {
                frac * power[k]
            } else {
                0.0
            }
    };
    let freqs = grid();
    let db = freqs
        .iter()
        .map(|&f| {
            let half = smoothing.width_octaves(f) / 2.0;
            let a = f * 2f64.powf(-half) / bin_hz;
            let b = f * 2f64.powf(half) / bin_hz;
            let p = if b - a >= 2.0 {
                (integral(b) - integral(a)) / (b - a)
            } else {
                let x = (f / bin_hz).min(power.len() as f64 - 1.0);
                let k = x.floor() as usize;
                let t = x - k as f64;
                let next = power.get(k + 1).copied().unwrap_or(power[k]);
                power[k] * (1.0 - t) + next * t
            };
            super::power_to_db(p)
        })
        .collect();
    Response { freqs, db }
}

/// Averages several per-bin power spectra of equal length.
#[must_use]
pub fn average_power(spectra: &[&[f64]]) -> Vec<f64> {
    let Some(first) = spectra.first() else {
        return Vec::new();
    };
    let n = spectra.len() as f64;
    (0..first.len())
        .map(|k| spectra.iter().map(|s| s[k]).sum::<f64>() / n)
        .collect()
}

/// The response averaged in dB over `octaves` around each point (fewer points at the
/// grid's ends). dB, not power, so a narrow resonance does not lift its surroundings.
/// Expects a log grid with [`POINTS_PER_OCTAVE`] points per octave.
#[must_use]
pub fn octave_average_db(response: &Response, octaves: f64) -> Vec<f64> {
    let half = (POINTS_PER_OCTAVE * octaves / 2.0).round() as usize;
    let n = response.db.len();
    (0..n)
        .map(|i| {
            let window = &response.db[i.saturating_sub(half)..(i + half + 1).min(n)];
            window.iter().sum::<f64>() / window.len() as f64
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_spans_audio_band() {
        let g = grid();
        assert!((g[0] - 20.0).abs() < 1e-9);
        assert!((g[g.len() - 1] - 20_000.0).abs() < 150.0);
        assert!(g.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn flat_power_smooths_to_flat() {
        let power = vec![2.0; 32_769];
        for s in [Smoothing::Fixed(1.0 / 48.0), Smoothing::Variable] {
            let r = smooth_power(&power, 0.732, s);
            assert!(r.db.iter().all(|d| (d - 10.0 * 2f64.log10()).abs() < 1e-9));
        }
    }

    #[test]
    fn variable_smoothing_widths() {
        let s = Smoothing::Variable;
        assert!((s.width_octaves(50.0) - 1.0 / 48.0).abs() < 1e-12);
        assert!((s.width_octaves(5000.0) - 1.0 / 3.0).abs() < 1e-12);
        assert!(s.width_octaves(300.0) > 1.0 / 48.0 && s.width_octaves(300.0) < 1.0 / 3.0);
    }

    #[test]
    fn interpolation_holds_ends() {
        let xs = [10.0, 100.0, 1000.0];
        let ys = [1.0, 2.0, 4.0];
        assert_eq!(interpolate_log(&xs, &ys, 5.0), 1.0);
        assert_eq!(interpolate_log(&xs, &ys, 5000.0), 4.0);
        assert!((interpolate_log(&xs, &ys, 10f64.powf(1.5)) - 1.5).abs() < 1e-12);
    }
}
