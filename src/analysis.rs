//! Turns sweep analyses into calibrated, smoothed responses.

use serde::{Deserialize, Serialize};

use crate::cal::Calibration;
use crate::dsp::level::SplScale;
use crate::dsp::spectrum::{Response, Smoothing, average_power, smooth_power};
use crate::dsp::sweep::SweepAnalysis;

/// The two smoothings of one channel's (position-averaged) response, in dB SPL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Responses {
    /// 1/48 octave, for display.
    pub display: Response,
    /// Variable smoothing, for fitting.
    pub fit: Response,
    /// Each mic position on its own, fit-smoothed, when there are several (used to check
    /// which corrections hold across positions).
    #[serde(default)]
    pub positions: Vec<Response>,
    /// Each mic position on its own at the display smoothing, when there are several: shown
    /// faintly behind their average.
    #[serde(default)]
    pub positions_display: Vec<Response>,
}

/// Smoothing of the curves shown: 1/48 octave, fine enough to show every room resonance.
const DISPLAY_SMOOTHING: Smoothing = Smoothing::Fixed(1.0 / 48.0);

/// Power-averages the positions of one channel and applies calibration.
///
/// Levels are dB SPL at the sweep's playback level (`sweep_dbfs`).
///
/// # Panics
///
/// Panics if `positions` is empty.
#[must_use]
pub fn responses(
    positions: &[SweepAnalysis],
    cal: &Calibration,
    scale: SplScale,
    sweep_dbfs: f64,
) -> Responses {
    assert!(
        !positions.is_empty(),
        "at least one measurement is required"
    );
    let spectra: Vec<&[f64]> = positions.iter().map(|a| a.power.as_slice()).collect();
    let power = average_power(&spectra);
    let bin_hz = positions[0].bin_hz;
    let finish = |power: &[f64], smoothing| {
        let mut r = smooth_power(power, bin_hz, smoothing);
        for (d, &f) in r.db.iter_mut().zip(&r.freqs) {
            *d = scale.spl(*d + sweep_dbfs) - cal.db_at(f);
        }
        r
    };
    let each = |smoothing: Smoothing| {
        if positions.len() > 1 {
            positions
                .iter()
                .map(|a| finish(&a.power, smoothing))
                .collect()
        } else {
            Vec::new()
        }
    };
    Responses {
        display: finish(&power, DISPLAY_SMOOTHING),
        fit: finish(&power, Smoothing::Variable),
        positions: each(Smoothing::Variable),
        positions_display: each(DISPLAY_SMOOTHING),
    }
}
