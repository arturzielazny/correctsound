//! The room's background noise per octave, and the measurement level it calls for.
//!
//! A measurement is only as good as its margin over the background noise, octave by
//! octave. For the logarithmic sweep the wizard plays, that margin follows a simple rule,
//! checked on 30 real captures (in every octave, 63 Hz to 8 kHz, the constant came out at
//! 6 ± 2 dB) and by [`tests::the_suggested_level_gives_the_margin_it_promises`]:
//!
//! ```text
//! margin(octave) = level + speaker(octave) − noise(octave) + MODEL_OFFSET_DB
//! ```
//!
//! where `level` is what the level step's meter reads for its noise (dB SPL),
//! `speaker(octave)` is how much louder the speakers are in that octave than in the meter's
//! band, and `noise(octave)` the background noise in the octave (dB SPL). So the quietest
//! level that still gives every octave a good margin can be worked out before measuring,
//! from a few seconds of listening to the room.

use std::collections::VecDeque;

use realfft::RealFftPlanner;

use super::SAMPLE_RATE;

/// Centres of the octaves analysed, Hz. The measurement's margin is checked from 63 Hz to
/// 8 kHz ([`super::sweep::SNR_BANDS_HZ`]); 31.5 Hz and 16 kHz are shown for information.
pub const OCTAVES: [f64; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16_000.0,
];
/// Analysis block: 0.5 s, so the 63 Hz octave holds about 20 bins.
const BLOCK: usize = SAMPLE_RATE as usize / 2;
/// A new block every 0.25 s.
const HOP: usize = BLOCK / 2;
/// Blocks kept: the last 3 s. The median over them ignores a cough or a passing car.
const KEEP: usize = 11;
/// Blocks needed before the levels are trusted (1.5 s).
const MIN_BLOCKS: usize = 5;

/// Margin per octave the suggested level aims for, dB. The measurement is rejected below
/// [`super::sweep::MIN_OCTAVE_SNR_DB`] (20 dB); at 30 dB the background adds at most a few
/// tenths of a dB of error, and dips 10 dB deep still get 20.
pub const MARGIN_GOAL_DB: f64 = 30.0;
/// The constant in the margin rule (see the module documentation), dB.
pub const MODEL_OFFSET_DB: f64 = 6.0;
/// What the speakers are assumed to lose against the meter's band before they have been
/// measured, dB: small speakers roll off in the lowest octave, and every speaker dips here
/// and there.
const SPEAKER_ALLOWANCE_63_DB: f64 = 6.0;
const SPEAKER_ALLOWANCE_DB: f64 = 3.0;
/// The suggested level is kept within these, dB SPL. Below 60 the margin rests on the room
/// staying exactly as quiet as it was while listening; above 80 it gets uncomfortable, and
/// a room that needs more should wait for a quieter moment.
pub const MIN_LEVEL_SPL: f64 = 60.0;
pub const MAX_LEVEL_SPL: f64 = 80.0;

/// Background noise per octave in dB, as mean-square levels in dBFS or, scaled, dB SPL.
pub type OctaveLevels = [f64; OCTAVES.len()];

/// Listens to the room and keeps the steady background level per octave.
pub struct AmbientMeter {
    pending: Vec<f32>,
    /// Mean square per octave of each recent block.
    blocks: VecDeque<OctaveLevels>,
    fft: std::sync::Arc<dyn realfft::RealToComplex<f64>>,
    window: Vec<f64>,
}

impl std::fmt::Debug for AmbientMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AmbientMeter")
            .field("blocks", &self.blocks.len())
            .finish_non_exhaustive()
    }
}

impl Default for AmbientMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl AmbientMeter {
    #[must_use]
    pub fn new() -> Self {
        let window = (0..BLOCK)
            .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / BLOCK as f64).cos())
            .collect();
        Self {
            pending: Vec::new(),
            blocks: VecDeque::new(),
            fft: RealFftPlanner::<f64>::new().plan_fft_forward(BLOCK),
            window,
        }
    }

    /// Forgets everything heard so far.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.blocks.clear();
    }

    /// Adds recorded samples.
    pub fn push(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= BLOCK {
            let block = self.analyse(&self.pending[..BLOCK]);
            self.blocks.push_back(block);
            if self.blocks.len() > KEEP {
                self.blocks.pop_front();
            }
            self.pending.drain(..HOP);
        }
    }

    /// Mean square per octave of one block.
    fn analyse(&self, block: &[f32]) -> OctaveLevels {
        let mut buf: Vec<f64> = block
            .iter()
            .zip(&self.window)
            .map(|(&s, w)| f64::from(s) * w)
            .collect();
        let mut out = self.fft.make_output_vec();
        self.fft
            .process(&mut buf, &mut out)
            .expect("buffer sizes match the plan");
        // A sine of amplitude A (mean square A²/2) peaks at |X|² = (A·Σw/2)²; for noise,
        // each bin's |X|² is spread by Σw², so the mean square of a band is
        // Σ 2|X|² / (N·Σw²) over its bins.
        let norm = 2.0 / (BLOCK as f64 * self.window.iter().map(|w| w * w).sum::<f64>());
        let bin_hz = f64::from(SAMPLE_RATE) / BLOCK as f64;
        let mut levels = [0.0; OCTAVES.len()];
        for (level, &c) in levels.iter_mut().zip(&OCTAVES) {
            let (lo, hi) = (c / std::f64::consts::SQRT_2, c * std::f64::consts::SQRT_2);
            let (a, b) = ((lo / bin_hz).ceil() as usize, (hi / bin_hz).ceil() as usize);
            *level = out[a..b.min(out.len())]
                .iter()
                .map(|c| c.norm_sqr() * norm)
                .sum();
        }
        levels
    }

    /// True once enough has been heard for [`Self::steady_dbfs`].
    #[must_use]
    pub fn ready(&self) -> bool {
        self.blocks.len() >= MIN_BLOCKS
    }

    /// Seconds of the room heard so far, up to the 3 s that are kept.
    #[must_use]
    pub fn heard_secs(&self) -> f64 {
        (self.blocks.len() as f64 * HOP as f64 + self.pending.len() as f64) / f64::from(SAMPLE_RATE)
    }

    /// The steady level per octave over the last 3 s, dBFS: the median of the blocks, so
    /// that short sounds do not count.
    #[must_use]
    pub fn steady_dbfs(&self) -> Option<OctaveLevels> {
        if !self.ready() {
            return None;
        }
        let mut levels = [0.0; OCTAVES.len()];
        for (i, level) in levels.iter_mut().enumerate() {
            let mut v: Vec<f64> = self.blocks.iter().map(|b| b[i]).collect();
            v.sort_by(f64::total_cmp);
            *level = super::power_to_db(v[v.len() / 2]);
        }
        Some(levels)
    }
}

/// The measurement level the room calls for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Suggestion {
    /// dB SPL on the level step's meter, within [`MIN_LEVEL_SPL`]–[`MAX_LEVEL_SPL`].
    pub level_spl: f64,
    /// The level each octave alone would need (31.5 Hz and 16 kHz: none), dB SPL.
    pub needs: [Option<f64>; OCTAVES.len()],
    /// The octave that needs the most, Hz.
    pub limited_by_hz: f64,
    /// True when the room would need more than [`MAX_LEVEL_SPL`].
    pub too_noisy: bool,
}

impl Suggestion {
    /// The quietest level that gives every checked octave [`MARGIN_GOAL_DB`] of margin over
    /// `noise_spl` (the background per octave, dB SPL, not corrected for the mic's
    /// calibration: the margin is computed on the raw recording too). Whole dB, rounded up.
    #[must_use]
    pub fn for_noise(noise_spl: &OctaveLevels) -> Self {
        let mut needs = [None; OCTAVES.len()];
        for (i, (&c, &noise)) in OCTAVES.iter().zip(noise_spl).enumerate() {
            if !(63.0..=8000.0).contains(&c) {
                continue;
            }
            let allowance = if c < 100.0 {
                SPEAKER_ALLOWANCE_63_DB
            } else {
                SPEAKER_ALLOWANCE_DB
            };
            needs[i] = Some(MARGIN_GOAL_DB - MODEL_OFFSET_DB + allowance + noise);
        }
        let (worst, need) = needs
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.map(|n| (i, n)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("some octaves are checked");
        Self {
            level_spl: need.ceil().clamp(MIN_LEVEL_SPL, MAX_LEVEL_SPL),
            needs,
            limited_by_hz: OCTAVES[worst],
            too_noisy: need > MAX_LEVEL_SPL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::level::{BandFilter, SplScale, rms_dbfs};
    use crate::dsp::sweep::{IrWindow, Sweep, SweepAnalysis, SweepParams};
    use crate::dsp::{Rng, db_to_amplitude};

    /// Room-like background: white hiss plus a rumble falling 12 dB per octave above 60 Hz.
    fn background(len: usize, hiss_dbfs: f64, rumble_dbfs: f64, seed: u64) -> Vec<f32> {
        let mut rng = Rng::new(seed);
        let a = 1.0 - (-std::f64::consts::TAU * 60.0 / f64::from(SAMPLE_RATE)).exp();
        let (mut y1, mut y2) = (0.0, 0.0);
        (0..len)
            .map(|_| {
                y1 += a * (db_to_amplitude(rumble_dbfs) * rng.next_gaussian() - y1);
                y2 += a * (y1 - y2);
                (y2 + db_to_amplitude(hiss_dbfs) * rng.next_gaussian()) as f32
            })
            .collect()
    }

    #[test]
    fn measures_a_sine_per_octave_and_ignores_a_short_noise() {
        let mut meter = AmbientMeter::new();
        let mut rng = Rng::new(3);
        let mut samples: Vec<f32> = (0..SAMPLE_RATE as usize * 3)
            .map(|i| {
                let t = i as f64 / f64::from(SAMPLE_RATE);
                (0.01 * (std::f64::consts::TAU * 1000.0 * t).sin()) as f32
            })
            .collect();
        // A loud bang, 0.3 s long: the median leaves it out.
        for s in &mut samples[48_000..62_400] {
            *s += (0.3 * rng.next_gaussian()) as f32;
        }
        meter.push(&samples);
        let levels = meter.steady_dbfs().expect("3 s heard");
        // 0.01 amplitude: mean square 5e-5, −43 dBFS.
        assert!((levels[5] + 43.0).abs() < 0.5, "{levels:?}");
        assert!(levels[2] < -100.0 && levels[8] < -100.0, "{levels:?}");
    }

    #[test]
    fn a_quiet_room_gets_the_floor_and_a_noisy_one_is_flagged() {
        let quiet =
            Suggestion::for_noise(&[30.0, 25.0, 20.0, 15.0, 12.0, 10.0, 10.0, 10.0, 10.0, 10.0]);
        assert_eq!(quiet.level_spl, MIN_LEVEL_SPL);
        assert!(!quiet.too_noisy);
        let rumble =
            Suggestion::for_noise(&[50.0, 42.0, 30.0, 25.0, 20.0, 20.0, 20.0, 20.0, 20.0, 20.0]);
        // 30 − 6 + 6 + 42 = 72, set by the 63 Hz octave; 31.5 Hz does not count.
        assert_eq!(rumble.level_spl, 72.0);
        assert_eq!(rumble.limited_by_hz, 63.0);
        let loud =
            Suggestion::for_noise(&[60.0, 55.0, 50.0, 45.0, 40.0, 40.0, 40.0, 40.0, 40.0, 40.0]);
        assert_eq!(loud.level_spl, MAX_LEVEL_SPL);
        assert!(loud.too_noisy);
    }

    /// The whole chain on a synthetic room: listen to the background, take the suggested
    /// level, set the level noise to it, measure a sweep in the same background, and check
    /// that every octave got the margin promised (the speaker here is flat, so it gets the
    /// allowance on top).
    #[test]
    fn the_suggested_level_gives_the_margin_it_promises() {
        let scale = SplScale { offset_db: 124.0 };
        let rate = SAMPLE_RATE as usize;
        let (hiss, rumble) = (-98.0, -52.0);
        let mut meter = AmbientMeter::new();
        meter.push(&background(rate * 3, hiss, rumble, 1));
        let noise_spl = meter.steady_dbfs().expect("heard").map(|d| scale.spl(d));
        let s = Suggestion::for_noise(&noise_spl);
        assert!(
            s.level_spl > MIN_LEVEL_SPL && !s.too_noisy && s.limited_by_hz == 63.0,
            "{s:?} from {noise_spl:?}"
        );
        // The gain that makes the level step's noise read the suggested level.
        let level_noise = crate::dsp::noise::pink_band(SAMPLE_RATE, 500.0, 2000.0, -20.0, 1);
        let read = scale.spl(rms_dbfs(
            &BandFilter::new(SAMPLE_RATE, 250.0, 4000.0).process(&level_noise),
        ));
        let gain = db_to_amplitude(s.level_spl - read);
        let sweep = Sweep::new(SweepParams::default());
        let pre = rate / 2;
        let played = sweep.samples_f32();
        let bg = background(pre + played.len() + rate * 3 / 2, hiss, rumble, 2);
        let recording: Vec<f32> = bg
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                let x = i
                    .checked_sub(pre)
                    .and_then(|j| played.get(j))
                    .copied()
                    .unwrap_or(0.0);
                (f64::from(x) * gain) as f32 + b
            })
            .collect();
        let a = SweepAnalysis::analyze(&sweep, &recording, IrWindow::default()).expect("measured");
        for &(c, snr) in &a.octave_snr_db {
            let i = OCTAVES.iter().position(|&o| o == c).expect("an octave");
            let promised = s.level_spl - s.needs[i].expect("checked") + MARGIN_GOAL_DB;
            let allowance = if c < 100.0 {
                SPEAKER_ALLOWANCE_63_DB
            } else {
                SPEAKER_ALLOWANCE_DB
            };
            let expected = promised + allowance;
            eprintln!("{c} Hz: margin {snr:.1} dB, predicted {expected:.1} dB");
            assert!(
                (snr - expected).abs() < 3.0,
                "{c} Hz: margin {snr:.1}, expected {expected:.1}"
            );
            assert!(snr >= MARGIN_GOAL_DB, "{c} Hz: {snr:.1}");
        }
    }
}
