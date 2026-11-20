//! Target curve and the frequency range the correction applies to.

use serde::{Deserialize, Serialize};

use crate::dsp::spectrum::{
    GRID_HIGH_HZ, GRID_LOW_HZ, POINTS_PER_OCTAVE, Response, octave_average_db,
};

/// User-adjustable design options.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DesignOptions {
    /// Target slope in dB per octave, pivoting at 1 kHz (0 = flat).
    pub tilt_db_per_oct: f64,
    /// Highest corrected frequency; `None`: each speaker's treble limit.
    pub high_limit_hz: Option<f64>,
    /// Filters per channel; `None`: chosen per channel (see `fit::fit_auto`).
    pub max_filters: Option<usize>,
}

impl Default for DesignOptions {
    fn default() -> Self {
        Self {
            // A gentle downward slope: what a speaker that is flat on axis measures like in
            // a room (it beams more at high frequencies), and what listeners prefer.
            tilt_db_per_oct: -0.5,
            high_limit_hz: None,
            max_filters: None,
        }
    }
}

impl DesignOptions {
    /// "auto" or the frequency, for display.
    #[must_use]
    pub fn high_limit_label(&self) -> String {
        self.high_limit_hz
            .map_or_else(|| "auto".to_owned(), |h| format!("{h:.0} Hz"))
    }

    /// "auto" or the count, for display.
    #[must_use]
    pub fn filters_label(&self) -> String {
        self.max_filters
            .map_or_else(|| "auto".to_owned(), |n| n.to_string())
    }
}

/// Selectable high limits on the result screen (`None` = automatic).
pub const HIGH_LIMIT_PRESETS: [Option<f64>; 5] = [
    None,
    Some(300.0),
    Some(2_000.0),
    Some(10_000.0),
    Some(20_000.0),
];

/// Searching up from here, the first frequency where the octave-averaged response falls
/// [`ROLLOFF_DB`] under target is the speaker's treble limit (automatic high limit).
const TREBLE_SEARCH_FROM_HZ: f64 = 2_000.0;

/// Band over which the reference level is taken.
pub const REFERENCE_BAND: (f64, f64) = (300.0, 3_000.0);

/// Below the bass limit, room resonances are still cut down to this frequency.
pub const CUT_FLOOR_HZ: f64 = 25.0;

/// The target and correction range for one channel.
///
/// Between [`CUT_FLOOR_HZ`] and `low_hz` (the speaker's bass limit) the target is the
/// speaker's own roll-off, and only peaks above it are corrected, with cuts: cutting a
/// room resonance there reduces boom without asking the speaker for more excursion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub reference_db: f64,
    pub tilt_db_per_oct: f64,
    pub low_hz: f64,
    pub high_hz: f64,
    /// The speaker's roll-off below `low_hz` as (Hz, dB SPL), ascending: the
    /// octave-averaged measured response. Empty when there is no cut-only region.
    #[serde(default)]
    pub roll_off: Vec<(f64, f64)>,
}

impl Target {
    /// Target level in dB at `freq`: the speaker's roll-off below its bass limit.
    #[must_use]
    pub fn db_at(&self, freq: f64) -> f64 {
        // Below the bass limit: the roll-off (held below the cut floor).
        if !self.roll_off.is_empty() && freq < self.low_hz {
            return interpolate(&self.roll_off, freq);
        }
        self.reference_db + self.tilt_db_per_oct * (freq / 1000.0).log2()
    }

    /// True in the main correction range (cuts and boosts).
    #[must_use]
    pub fn in_range(&self, freq: f64) -> bool {
        (self.low_hz..=self.high_hz).contains(&freq)
    }

    /// True below the bass limit where resonances are still cut (no boosts).
    #[must_use]
    pub fn cut_only(&self, freq: f64) -> bool {
        !self.roll_off.is_empty() && (self.cut_floor_hz()..self.low_hz).contains(&freq)
    }

    /// Lowest frequency any filter acts on.
    #[must_use]
    pub fn cut_floor_hz(&self) -> f64 {
        if self.roll_off.is_empty() {
            self.low_hz
        } else {
            CUT_FLOOR_HZ.min(self.low_hz)
        }
    }

    /// Builds the target for a fit-smoothed response.
    ///
    /// The reference level is the channel's own mean over [`REFERENCE_BAND`]; level
    /// differences between channels are handled separately by a trim. The range starts
    /// at the speaker's bass limit, found from the octave-averaged response.
    #[must_use]
    pub fn for_response(fit: &Response, options: &DesignOptions) -> Self {
        let mut t = Self {
            reference_db: fit.mean_between(REFERENCE_BAND.0, REFERENCE_BAND.1),
            tilt_db_per_oct: options.tilt_db_per_oct,
            low_hz: GRID_LOW_HZ,
            high_hz: GRID_HIGH_HZ,
            roll_off: Vec::new(),
        };
        if let Some(r) = rolloff(fit, &t) {
            t.low_hz = r.max(GRID_LOW_HZ);
        }
        t.high_hz = match options.high_limit_hz {
            Some(h) if h.is_finite() => h.clamp(CUT_FLOOR_HZ, GRID_HIGH_HZ),
            _ => treble_limit(fit, &t),
        };
        if t.low_hz > CUT_FLOOR_HZ {
            let smooth = octave_average_db(fit, 1.0);
            t.roll_off = fit
                .freqs
                .iter()
                .zip(smooth)
                .filter(|(f, _)| (CUT_FLOOR_HZ / 1.1..=t.low_hz).contains(*f))
                .map(|(&f, d)| (f, d))
                .collect();
        }
        t
    }
}

/// The speaker's treble limit: above it the octave-averaged response stays more than
/// [`ROLLOFF_DB`] below target all the way to the top (a roll-off, unlike a broad dip it
/// recovers from); the top of the grid when it never does. Not below
/// [`TREBLE_SEARCH_FROM_HZ`]. Up to there the whole passband is corrected (with wide
/// filters only, above 5 kHz).
fn treble_limit(response: &Response, target: &Target) -> f64 {
    let dev: Vec<f64> = response
        .freqs
        .iter()
        .zip(octave_average_db(response, 1.0))
        .map(|(&f, d)| d - target.db_at(f))
        .collect();
    let last_ok = response
        .freqs
        .iter()
        .zip(&dev)
        .rposition(|(_, d)| *d >= -ROLLOFF_DB);
    match last_ok {
        Some(i) if i + 1 < response.freqs.len() => response.freqs[i + 1].max(TREBLE_SEARCH_FROM_HZ),
        _ => GRID_HIGH_HZ,
    }
}

/// Linear interpolation in log frequency over ascending `(Hz, dB)` points, held at the ends.
fn interpolate(points: &[(f64, f64)], freq: f64) -> f64 {
    match points.iter().position(|p| p.0 >= freq) {
        None => points.last().map_or(0.0, |p| p.1),
        Some(0) => points[0].1,
        Some(i) => {
            let (a, b) = (points[i - 1], points[i]);
            let t = (freq / a.0).ln() / (b.0 / a.0).ln();
            a.1 + (b.1 - a.1) * t
        }
    }
}

/// Searching down from here, the first frequency where the octave-averaged response
/// is [`ROLLOFF_DB`] under target is the speaker's low-frequency limit.
const ROLLOFF_SEARCH_FROM_HZ: f64 = 200.0;
const ROLLOFF_DB: f64 = 6.0;

/// The speaker's bass limit: where the response, averaged in dB over one octave, first
/// falls [`ROLLOFF_DB`] below target going down. Averaging dB (not power) over a whole
/// octave keeps a single room-mode spike below the roll-off from looking like bass
/// extension, and a single narrow cancellation above it from looking like a roll-off.
fn rolloff(response: &Response, target: &Target) -> Option<f64> {
    let dev: Vec<f64> = response
        .freqs
        .iter()
        .zip(&response.db)
        .map(|(&f, &d)| d - target.db_at(f))
        .collect();
    let half = (POINTS_PER_OCTAVE / 2.0).round() as usize;
    let start = response
        .freqs
        .iter()
        .rposition(|&f| f <= ROLLOFF_SEARCH_FROM_HZ)?;
    (0..=start).rev().find_map(|i| {
        let window = &dev[i.saturating_sub(half)..(i + half + 1).min(dev.len())];
        let mean = window.iter().sum::<f64>() / window.len() as f64;
        (mean < -ROLLOFF_DB).then_some(response.freqs[i])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::spectrum::grid;

    fn response(f: impl Fn(f64) -> f64) -> Response {
        let freqs = grid();
        let db = freqs.iter().map(|&x| f(x)).collect();
        Response { freqs, db }
    }

    #[test]
    fn finds_rolloff() {
        // Flat 80 dB above 60 Hz, -24 dB/oct below.
        let r = response(|f| {
            if f >= 60.0 {
                80.0
            } else {
                80.0 - 24.0 * (60.0 / f).log2()
            }
        });
        let t = Target::for_response(&r, &DesignOptions::default());
        assert!((t.reference_db - 80.0).abs() < 1e-9);
        // The octave-averaged response is 6 dB down about a quarter octave below 60 Hz.
        assert!(t.low_hz > 48.0 && t.low_hz < 58.0, "low {}", t.low_hz);
    }

    #[test]
    fn room_mode_below_rolloff_is_not_bass_extension() {
        // A small speaker rolling off at 70 Hz, with a narrow room-mode spike at 45 Hz
        // that pokes back up to the target level (as measured on a desktop pair).
        let r = response(|f| {
            let base = if f >= 70.0 {
                80.0
            } else {
                80.0 - 24.0 * (70.0 / f).log2()
            };
            let spike = 20.0 * (-((f / 45.0).log2() * 24.0).powi(2)).exp();
            base + spike
        });
        let t = Target::for_response(&r, &DesignOptions::default());
        assert!(t.low_hz > 55.0 && t.low_hz < 70.0, "low {}", t.low_hz);
    }

    #[test]
    fn full_range_speaker_starts_at_20hz() {
        let r = response(|_| 80.0);
        let t = Target::for_response(&r, &DesignOptions::default());
        assert_eq!(t.low_hz, 20.0);
        assert!((t.high_hz - 20_000.0).abs() < 200.0, "high {}", t.high_hz);
    }

    #[test]
    fn automatic_high_limit_follows_the_treble_roll_off() {
        // Flat to 12 kHz, then -24 dB/oct.
        let r = response(|f| {
            if f <= 12_000.0 {
                80.0
            } else {
                80.0 - 24.0 * (f / 12_000.0).log2()
            }
        });
        let flat = DesignOptions {
            tilt_db_per_oct: 0.0,
            ..DesignOptions::default()
        };
        let t = Target::for_response(&r, &flat);
        assert!(
            t.high_hz > 12_000.0 && t.high_hz < 16_000.0,
            "high {}",
            t.high_hz
        );
        let fixed = DesignOptions {
            high_limit_hz: Some(10_000.0),
            ..flat
        };
        assert_eq!(Target::for_response(&r, &fixed).high_hz, 10_000.0);
    }
}
