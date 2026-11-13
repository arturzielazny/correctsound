//! Correction design: target curve, parametric EQ fitting and export.

pub mod export;
pub mod fit;
pub mod target;

use serde::{Deserialize, Serialize};

use crate::dsp::SAMPLE_RATE;
use crate::dsp::biquad::Biquad;

/// One peaking (bell) filter.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub freq: f64,
    pub gain: f64,
    pub q: f64,
}

impl Filter {
    /// Magnitude in dB at `freq`, as EasyEffects' `APO (DR)` bell renders it.
    #[must_use]
    pub fn db_at(&self, freq: f64) -> f64 {
        Biquad::peaking(f64::from(SAMPLE_RATE), self.freq, self.gain, self.q)
            .magnitude_db(f64::from(SAMPLE_RATE), freq)
    }

    /// Rounds to the precision the output formats carry.
    #[must_use]
    pub fn rounded(&self) -> Self {
        Self {
            freq: (self.freq * 10.0).round() / 10.0,
            gain: (self.gain * 10.0).round() / 10.0,
            q: (self.q * 100.0).round() / 100.0,
        }
    }
}

/// Sum of the filters' responses in dB at each frequency.
#[must_use]
pub fn response_db(filters: &[Filter], freqs: &[f64]) -> Vec<f64> {
    let rate = f64::from(SAMPLE_RATE);
    let mut out = vec![0.0; freqs.len()];
    for f in filters {
        let bq = Biquad::peaking(rate, f.freq, f.gain, f.q);
        for (o, &x) in out.iter_mut().zip(freqs) {
            *o += bq.magnitude_db(rate, x);
        }
    }
    out
}

/// A complete stereo correction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqSet {
    /// Shared negative gain that keeps the EQ from clipping, dB.
    pub preamp_db: f64,
    /// Per-channel level trim (≤ 0 dB) that evens out left/right loudness.
    pub trim_db: [f64; 2],
    /// Filters per channel, sorted by frequency.
    pub channels: [Vec<Filter>; 2],
}
