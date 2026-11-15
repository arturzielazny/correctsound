//! Live placement and room treatment: the bass at the seat, measured over and over while
//! the speakers, the seat, the mic or a bass trap are moved, with a pinned reading to
//! compare against.
//!
//! Two ways to measure, switched with a key:
//!
//! - **Sweeps**: a short bass sweep every couple of seconds. Each gives the response at fine
//!   resolution and a waterfall of how the bass decays, which is where bass traps show.
//! - **Live**: bass-only periodic noise ([`crate::dsp::periodic`]) plays continuously and
//!   the response is recomputed from the latest third of a second ten times a second, so
//!   the curve follows a moving trap within about half a second, at coarser resolution.
//!
//! Below about 200 Hz the room decides the response, and moving things by 20–50 cm changes
//! it more than any EQ can; it is also the only cure for cancellation dips.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::audio::{AudioBackend, Channel, Program, Route, Source, Stream};
use crate::cal::Calibration;
use crate::dsp::SAMPLE_RATE;
use crate::dsp::decay::Waterfall;
use crate::dsp::level::SplScale;
use crate::dsp::periodic::PeriodicNoise;
use crate::dsp::spectrum::{Response, Smoothing, octave_average_db, smooth_power};
use crate::dsp::sweep::{Sweep, SweepParams, quick_analysis};

/// Highest frequency shown, Hz.
pub const TOP_HZ: f64 = 500.0;
/// The band the evenness score covers, Hz.
pub const SCORE_FROM_HZ: f64 = 40.0;
pub const SCORE_TO_HZ: f64 = 250.0;
/// Smoothing of a sweep reading as shown, octaves.
const SWEEP_SMOOTHING_OCT: f64 = 1.0 / 24.0;
/// Smoothing of a live reading as shown, octaves: its bins are 2.9 Hz apart.
const LIVE_SMOOTHING_OCT: f64 = 1.0 / 12.0;
/// Smoothing the score is computed at, in both modes, so their scores compare.
const SCORE_SMOOTHING_OCT: f64 = 1.0 / 24.0;
/// Width of the trend that peaks and dips are measured against, octaves.
const TREND_OCT: f64 = 2.0;
/// Short sweep: 1.4 s, 10 Hz – 1 kHz, at the measurement level.
const SWEEP_LEN: usize = 1 << 16;
const SWEEP_END_HZ: f64 = 1000.0;
/// Silence before and after each sweep (the room's bass decay and the window need it).
const PRE: usize = SAMPLE_RATE as usize / 4;
const POST: usize = SAMPLE_RATE as usize * 6 / 10;
/// Capture kept after playback ends, covering output latency.
const TAIL: Duration = Duration::from_millis(400);
/// Live noise: one period is 0.34 s, the slice each update is computed from.
const LIVE_PERIOD: usize = 1 << 14;
/// The live noise's band, Hz: a little wider than what is shown.
const LIVE_BAND: (f64, f64) = (15.0, 600.0);
/// The live curve is recomputed after every this many recorded samples (0.1 s of sound),
/// counted in audio rather than clock time so the simulator behaves like a room.
const LIVE_HOP: usize = SAMPLE_RATE as usize / 10;
/// Recording to skip when the noise starts (start-up latency, fade-in, the room filling).
const LIVE_SETTLE: usize = SAMPLE_RATE as usize * 3 / 4;
/// Fade-in of the noise, so it does not start with a click.
const LIVE_FADE_IN: usize = SAMPLE_RATE as usize / 4;
/// Weight of the newest slice in the live average (the rest is the previous average):
/// steadies the curve against room noise while following a change within a few updates.
const LIVE_NEW_WEIGHT: f64 = 0.5;
/// Below this, the noise is taken not to reach the mic, dB (see
/// [`PeriodicNoise::reaches_mic_db`]).
const LIVE_MIN_SNR_DB: f64 = 15.0;

/// How the placement screen measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A short sweep every couple of seconds: fine detail and the decay.
    Sweeps,
    /// Continuous noise, updated ten times a second.
    Live,
}

/// How even the bass is at one placement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// RMS of the peaks and dips around the two-octave trend, dB: lower is more even.
    pub evenness_db: f64,
    /// The deepest dip below the trend: (Hz, dB).
    pub deepest: (f64, f64),
    /// The highest peak above the trend: (Hz, dB).
    pub highest: (f64, f64),
}

impl Score {
    /// Scores a bass response on the analysis grid.
    #[must_use]
    pub fn of(response: &Response) -> Self {
        let trend = octave_average_db(response, TREND_OCT);
        let dev: Vec<(f64, f64)> = response
            .freqs
            .iter()
            .zip(response.db.iter().zip(&trend))
            .filter(|(f, _)| (SCORE_FROM_HZ..=SCORE_TO_HZ).contains(*f))
            .map(|(&f, (d, t))| (f, d - t))
            .collect();
        let n = dev.len().max(1) as f64;
        let evenness_db = (dev.iter().map(|d| d.1 * d.1).sum::<f64>() / n).sqrt();
        let deepest = dev
            .iter()
            .copied()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap_or_default();
        let highest = dev
            .iter()
            .copied()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap_or_default();
        Self {
            evenness_db,
            deepest,
            highest,
        }
    }
}

/// One result.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub channel: Channel,
    pub mode: Mode,
    /// dB SPL from 20 Hz to [`TOP_HZ`].
    pub response: Response,
    pub score: Score,
    /// How the bass decays (sweeps only).
    pub waterfall: Option<Waterfall>,
}

impl Reading {
    /// A reading from a per-bin response power `|H|²`, calibrated to dB SPL for a signal
    /// peaking at `peak_dbfs`.
    fn new(
        power: &[f64],
        bin_hz: f64,
        mode: Mode,
        channel: Channel,
        peak_dbfs: f64,
        (cal, scale): (&Calibration, SplScale),
    ) -> Self {
        let calibrated = |smoothing: f64| {
            let mut r = smooth_power(power, bin_hz, Smoothing::Fixed(smoothing));
            let keep = r.freqs.partition_point(|&f| f <= TOP_HZ);
            r.freqs.truncate(keep);
            r.db.truncate(keep);
            for (d, &f) in r.db.iter_mut().zip(&r.freqs) {
                *d = scale.spl(*d + peak_dbfs) - cal.db_at(f);
            }
            r
        };
        let shown = match mode {
            Mode::Sweeps => SWEEP_SMOOTHING_OCT,
            Mode::Live => LIVE_SMOOTHING_OCT,
        };
        Self {
            channel,
            mode,
            score: Score::of(&calibrated(SCORE_SMOOTHING_OCT)),
            response: calibrated(shown),
            waterfall: None,
        }
    }
}

/// What is playing and being recorded.
enum Run {
    Sweep {
        stream: Stream,
        recording: Vec<f32>,
        finished_at: Option<Instant>,
    },
    Live {
        stream: Stream,
        /// The latest recording, at most one period plus what arrived since.
        recent: Vec<f32>,
        /// Samples recorded since the noise started.
        received: usize,
        /// `received` at the last update.
        updated_at: usize,
        /// The running average of the response power per bin.
        average: Option<Vec<f64>>,
    },
}

/// The placement screen's state and its measuring loop.
pub struct PlacementState {
    /// The speaker that plays.
    pub channel: Channel,
    pub mode: Mode,
    pub paused: bool,
    pub latest: Option<Reading>,
    /// A pinned reading to compare against.
    pub reference: Option<Reading>,
    /// The latest bass response of each speaker (by [`Channel::index`]), so the chart can
    /// keep one scale for both.
    pub last: [Option<Response>; 2],
    /// Readings since the screen opened (sweeps, or live updates).
    pub count: usize,
    pub issue: Option<String>,
    sweep: Sweep,
    signal: Arc<[f32]>,
    noise: PeriodicNoise,
    noise_signal: Arc<[f32]>,
    run: Option<Run>,
}

impl std::fmt::Debug for PlacementState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlacementState")
            .field("channel", &self.channel)
            .field("mode", &self.mode)
            .field("paused", &self.paused)
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl PlacementState {
    /// Sweeps mode on `channel`, at the measurement level `sweep_dbfs` (the live noise
    /// peaks at the same level).
    #[must_use]
    pub fn new(channel: Channel, sweep_dbfs: f64) -> Self {
        let sweep = Sweep::new(SweepParams {
            end_hz: SWEEP_END_HZ,
            len: SWEEP_LEN,
            peak_dbfs: sweep_dbfs,
            ..SweepParams::default()
        });
        let signal = sweep.samples_f32().into();
        let noise = PeriodicNoise::new(LIVE_PERIOD, LIVE_BAND, sweep_dbfs, f64::from(SAMPLE_RATE));
        let noise_signal = noise.samples_f32().into();
        Self {
            channel,
            mode: Mode::Sweeps,
            paused: false,
            latest: None,
            reference: None,
            last: [None, None],
            count: 0,
            issue: None,
            sweep,
            signal,
            noise,
            noise_signal,
            run: None,
        }
    }

    /// True while something plays or is recorded.
    #[must_use]
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Starts measuring unless already running or paused: one sweep, or the live noise.
    ///
    /// # Errors
    ///
    /// Fails if the audio streams cannot start.
    pub fn start(&mut self, audio: &mut dyn AudioBackend) -> Result<()> {
        if self.run.is_some() || self.paused {
            return Ok(());
        }
        self.run = Some(match self.mode {
            Mode::Sweeps => {
                let program = Program::Once {
                    signal: Arc::clone(&self.signal),
                    pre: PRE,
                    post: POST,
                };
                Run::Sweep {
                    stream: audio.start(Source::new(program, self.channel), Route::Hardware)?,
                    recording: Vec::new(),
                    finished_at: None,
                }
            }
            Mode::Live => {
                let program = Program::Loop {
                    signal: Arc::clone(&self.noise_signal),
                    fade_in: LIVE_FADE_IN,
                };
                Run::Live {
                    stream: audio.start(Source::new(program, self.channel), Route::Hardware)?,
                    recent: Vec::new(),
                    received: 0,
                    updated_at: 0,
                    average: None,
                }
            }
        });
        Ok(())
    }

    /// Stops playing.
    pub fn stop(&mut self) {
        self.run = None;
    }

    /// Switches to the other speaker; the next start plays from it.
    pub fn switch_channel(&mut self) {
        self.channel = self.channel.other();
        self.stop();
    }

    /// Switches between sweeps and live noise; the next start uses the new mode.
    pub fn switch_mode(&mut self) {
        self.mode = match self.mode {
            Mode::Sweeps => Mode::Live,
            Mode::Live => Mode::Sweeps,
        };
        self.stop();
    }

    /// Pins the latest reading as the reference (or unpins when it already is).
    pub fn pin(&mut self) {
        if self.reference.is_some() && self.reference == self.latest {
            self.reference = None;
        } else {
            self.reference.clone_from(&self.latest);
        }
    }

    fn arrived(&mut self, reading: Reading) {
        self.last[reading.channel.index()] = Some(reading.response.clone());
        self.latest = Some(reading);
        self.count += 1;
        self.issue = None;
    }

    /// Collects audio; returns true when a new reading arrived. Call [`Self::start`]
    /// afterwards to keep the loop going.
    pub fn tick(&mut self, cal: &Calibration, scale: SplScale) -> bool {
        let peak_dbfs = self.sweep.params().peak_dbfs;
        let (channel, mode) = (self.channel, self.mode);
        match &mut self.run {
            None => false,
            Some(Run::Sweep {
                stream,
                recording,
                finished_at,
            }) => {
                recording.extend(stream.drain());
                if stream.is_finished() && finished_at.is_none() {
                    *finished_at = Some(Instant::now());
                }
                if finished_at.is_none_or(|t| t.elapsed() < TAIL) {
                    return false;
                }
                let recording = std::mem::take(recording);
                self.run = None;
                match quick_analysis(&self.sweep, &recording) {
                    Ok(q) => {
                        let mut reading = Reading::new(
                            &q.power,
                            q.bin_hz,
                            mode,
                            channel,
                            peak_dbfs,
                            (cal, scale),
                        );
                        reading.waterfall = Some(q.waterfall);
                        self.arrived(reading);
                        true
                    }
                    Err(e) => {
                        self.issue = Some(e.to_string());
                        false
                    }
                }
            }
            Some(Run::Live {
                stream,
                recent,
                received,
                updated_at,
                average,
            }) => {
                let new = stream.drain();
                *received += new.len();
                recent.extend(new);
                let period = self.noise.len();
                if recent.len() > period {
                    recent.drain(..recent.len() - period);
                }
                if *received < LIVE_SETTLE + period || *received < *updated_at + LIVE_HOP {
                    return false;
                }
                *updated_at = *received;
                if crate::dsp::level::peak_dbfs(recent) > -1.0 {
                    self.issue =
                        Some("the mic signal clipped; turn the amplifier down a little".to_owned());
                    return false;
                }
                if self.noise.reaches_mic_db(recent) < LIVE_MIN_SNR_DB {
                    self.issue = Some(
                        "the noise is not reaching the mic; check the output, the amplifier \
                         and that the mic points at the speakers"
                            .to_owned(),
                    );
                    return false;
                }
                let fresh = self.noise.response_power(recent);
                let power = match average.take() {
                    Some(old) => old
                        .iter()
                        .zip(&fresh)
                        .map(|(o, n)| o * (1.0 - LIVE_NEW_WEIGHT) + n * LIVE_NEW_WEIGHT)
                        .collect(),
                    None => fresh,
                };
                let reading = Reading::new(
                    &power,
                    self.noise.bin_hz(),
                    mode,
                    channel,
                    peak_dbfs,
                    (cal, scale),
                );
                *average = Some(power);
                self.arrived(reading);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::spectrum::grid;

    fn bass(shape: impl Fn(f64) -> f64) -> Response {
        let freqs: Vec<f64> = grid().into_iter().filter(|&f| f <= TOP_HZ).collect();
        let db = freqs.iter().map(|&f| 80.0 + shape(f)).collect();
        Response { freqs, db }
    }

    #[test]
    fn a_flat_bass_scores_zero_and_a_dip_is_located() {
        let flat = Score::of(&bass(|_| 0.0));
        assert!(flat.evenness_db < 1e-9);
        let dip = Score::of(&bass(|f| {
            -15.0 * (-((f / 90.0).log2() * 12.0).powi(2)).exp()
        }));
        assert!(dip.evenness_db > 1.0);
        assert!((dip.deepest.0 / 90.0).log2().abs() < 1.0 / 24.0, "{dip:?}");
        assert!(dip.deepest.1 < -10.0, "{dip:?}");
        // A slope alone (the speaker's roll-off) is not unevenness.
        let slope = Score::of(&bass(|f| 6.0 * (f / 100.0).log2()));
        assert!(slope.evenness_db < 0.5, "{slope:?}");
    }
}
