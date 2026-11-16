//! RMS and SPL metering.

use super::biquad::{Biquad, BiquadState};

/// Constant `C` in `SPL = dBFS + C - sens_factor` for current-generation UMIK-1 mics.
///
/// Matches REW's reading on the target machine ("36 dB SPL (-88 dBFS)" with sens -0.276).
pub const UMIK1_SPL_CONSTANT: f64 = 124.0;

/// RMS level in dBFS where a full-scale sine reads 0 dBFS.
#[must_use]
pub fn rms_dbfs(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return -300.0;
    }
    let mean_sq = samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len() as f64;
    10.0 * (2.0 * mean_sq).max(1e-30).log10()
}

/// Band-pass for metering band-limited test noise: two high-pass and two low-pass
/// sections, so room rumble (mostly below 20 Hz on a UMIK-1) does not count as signal.
#[derive(Debug, Clone)]
pub struct BandFilter {
    sections: [(Biquad, BiquadState); 4],
}

impl BandFilter {
    /// Pass band roughly `low_hz`..`high_hz` (each edge is −6 dB).
    #[must_use]
    pub fn new(rate: u32, low_hz: f64, high_hz: f64) -> Self {
        let rate = f64::from(rate);
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let hp = Biquad::high_pass(rate, low_hz, q);
        let lp = Biquad::low_pass(rate, high_hz, q);
        let s = BiquadState::default();
        Self {
            sections: [(hp, s), (hp, s), (lp, s), (lp, s)],
        }
    }

    /// Filters a chunk, continuing from the previous chunk's state.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        samples
            .iter()
            .map(|&x| {
                let y = self
                    .sections
                    .iter_mut()
                    .fold(f64::from(x), |v, (f, s)| f.tick(s, v));
                y as f32
            })
            .collect()
    }

    /// Clears the filter state.
    pub fn reset(&mut self) {
        for (_, s) in &mut self.sections {
            *s = BiquadState::default();
        }
    }
}

/// Median of the RMS levels of `block_len`-sample blocks, in dBFS: robust against a
/// short loud event (a cough, a key press) in an otherwise steady recording.
#[must_use]
pub fn median_block_dbfs(samples: &[f32], block_len: usize) -> f64 {
    let mut levels: Vec<f64> = samples
        .chunks_exact(block_len.max(1))
        .map(rms_dbfs)
        .collect();
    if levels.is_empty() {
        return rms_dbfs(samples);
    }
    levels.sort_by(f64::total_cmp);
    levels[levels.len() / 2]
}

/// Peak absolute sample value in dBFS (0 dBFS = 1.0).
#[must_use]
pub fn peak_dbfs(samples: &[f32]) -> f64 {
    let peak = samples.iter().fold(0f32, |m, &s| m.max(s.abs()));
    super::amplitude_to_db(f64::from(peak))
}

/// Converts mic dBFS to dB SPL.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplScale {
    /// dB added to a dBFS reading to get dB SPL.
    pub offset_db: f64,
}

impl SplScale {
    /// Scale for a mic with the given calibration sensitivity factor.
    #[must_use]
    pub fn from_sens_factor(sens_factor: f64) -> Self {
        Self {
            offset_db: UMIK1_SPL_CONSTANT - sens_factor,
        }
    }

    /// Converts a dBFS level to dB SPL.
    #[must_use]
    pub fn spl(&self, dbfs: f64) -> f64 {
        dbfs + self.offset_db
    }
}

/// Block RMS meter with exponential averaging, fed with arbitrary-sized chunks.
#[derive(Debug, Clone)]
pub struct LevelMeter {
    block_len: usize,
    smoothing: f64,
    acc: f64,
    acc_len: usize,
    mean_sq: Option<f64>,
    last_block_peak: f32,
}

impl LevelMeter {
    /// Meter computing RMS over `block_len` samples, averaged with time constant `tau_blocks` blocks.
    #[must_use]
    pub fn new(block_len: usize, tau_blocks: f64) -> Self {
        Self {
            block_len,
            smoothing: (-1.0 / tau_blocks).exp(),
            acc: 0.0,
            acc_len: 0,
            mean_sq: None,
            last_block_peak: 0.0,
        }
    }

    /// Meter with 100 ms blocks and ~0.3 s averaging at `rate`: quick enough to follow an
    /// amplifier's knob as it turns, steady to a few tenths of a dB on band-limited noise.
    #[must_use]
    pub fn quick(rate: u32) -> Self {
        Self::new(rate as usize / 10, 3.0)
    }

    /// Meter with 300 ms blocks and ~1 s averaging at `rate`.
    #[must_use]
    pub fn standard(rate: u32) -> Self {
        Self::new((rate as usize * 3) / 10, 1.0 / 0.3)
    }

    /// Feeds samples; returns the number of completed blocks.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let mut blocks = 0;
        for &s in samples {
            self.acc += f64::from(s).powi(2);
            self.acc_len += 1;
            self.last_block_peak = self.last_block_peak.max(s.abs());
            if self.acc_len == self.block_len {
                let block = self.acc / self.block_len as f64;
                self.mean_sq = Some(match self.mean_sq {
                    None => block,
                    Some(prev) => self.smoothing * prev + (1.0 - self.smoothing) * block,
                });
                self.acc = 0.0;
                self.acc_len = 0;
                blocks += 1;
            }
        }
        blocks
    }

    /// Averaged level in dBFS (sine = 0 dBFS), once at least one block has completed.
    #[must_use]
    pub fn dbfs(&self) -> Option<f64> {
        self.mean_sq.map(|m| 10.0 * (2.0 * m).max(1e-30).log10())
    }

    /// Peak sample since the last call, then resets it.
    pub fn take_peak(&mut self) -> f32 {
        std::mem::take(&mut self.last_block_peak)
    }

    /// Clears the averaged state, e.g. when the signal changes.
    pub fn reset(&mut self) {
        self.mean_sq = None;
        self.acc = 0.0;
        self.acc_len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_sine_is_zero_dbfs() {
        let s: Vec<f32> = (0..48_000)
            .map(|i| (std::f64::consts::TAU * 1000.0 * f64::from(i) / 48_000.0).sin() as f32)
            .collect();
        assert!(rms_dbfs(&s).abs() < 0.01);
        let mut m = LevelMeter::standard(48_000);
        m.push(&s);
        assert!(m.dbfs().expect("blocks completed").abs() < 0.01);
    }

    #[test]
    fn the_quick_meter_follows_a_knob_within_a_second() {
        let rate = 48_000;
        let noise = crate::dsp::noise::pink_band(rate, 500.0, 2000.0, -20.0, 2);
        let second = rate as usize;
        let quiet: Vec<f32> = noise.iter().copied().cycle().take(second).collect();
        let mut m = LevelMeter::quick(rate);
        m.push(&quiet);
        let before = m.dbfs().expect("a reading");
        // The knob turned up 6 dB: within a second the reading is there.
        let louder: Vec<f32> = quiet.iter().map(|v| v * 2.0).collect();
        m.push(&louder);
        let after = m.dbfs().expect("a reading");
        assert!((after - before - 6.02).abs() < 0.5, "{before} → {after}");
    }

    #[test]
    fn band_filter_keeps_test_noise_and_removes_rumble() {
        let rate = 48_000;
        let noise = crate::dsp::noise::pink_band(rate, 500.0, 2000.0, -20.0, 1);
        let mut band = BandFilter::new(rate, 250.0, 4000.0);
        let passed = band.process(&noise);
        // Skip the filter's start-up transient.
        let loss = rms_dbfs(&noise[4800..]) - rms_dbfs(&passed[4800..]);
        assert!(loss.abs() < 0.4, "in-band loss {loss:.2} dB");

        band.reset();
        let rumble: Vec<f32> = (0..rate)
            .map(|i| {
                (0.1 * (std::f64::consts::TAU * 8.0 * f64::from(i) / f64::from(rate)).sin()) as f32
            })
            .collect();
        let out = band.process(&rumble);
        assert!(rms_dbfs(&out[24_000..]) - rms_dbfs(&rumble) < -80.0);
    }

    #[test]
    fn median_ignores_a_short_burst() {
        let mut x = vec![0.001f32; 48_000];
        for v in &mut x[10_000..12_000] {
            *v = 0.5;
        }
        let steady = rms_dbfs(&x[..4800]);
        assert!((median_block_dbfs(&x, 4800) - steady).abs() < 0.01);
    }

    #[test]
    fn spl_matches_rew_reading() {
        let scale = SplScale::from_sens_factor(-0.276);
        assert!((scale.spl(-88.0) - 36.276).abs() < 1e-9);
    }
}
