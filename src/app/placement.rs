//! Live placement and room treatment: the bass at the seat, measured over and over while
//! the speakers, the seat, the mic or a bass trap are moved, with a pinned reading to
//! compare against.
//!
//! Three ways to measure, switched with a key:
//!
//! - **Sweeps**: a short bass sweep every couple of seconds. Each gives the response at fine
//!   resolution and a waterfall of how the bass decays, which is where bass traps show.
//! - **Fast sweeps**: a one-second sweep played in a seamless loop
//!   ([`crate::dsp::periodic`]), the response and the waterfall recomputed from the latest
//!   second five times a second. A change shows fully within a second.
//! - **Live**: bass-only periodic noise plays continuously and the response is recomputed
//!   from the latest third of a second ten times a second, so the curve follows a moving
//!   trap within about half a second, at coarser resolution and without the decay.
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
use crate::dsp::periodic::Periodic;
use crate::dsp::spectrum::{Response, Smoothing, octave_average_db, smooth_power};
use crate::dsp::sweep::{
    QuickAnalysis, Sweep, SweepParams, check_clipping, quick_analysis, quick_ir_analysis,
};

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
/// Fast sweeps: one sweep is 1.02 s. Shorter, and the room would still be ringing from one
/// sweep when the next begins, which hides the decay; the waterfall needs 0.7 s after the
/// direct sound, and the sweep's distortion lands 0.15 s before it (wrapped around).
const FAST_PERIOD: usize = 3 << 14;
/// The fast sweep's range, Hz: the same as a single sweep's.
const FAST_BAND: (f64, f64) = (10.0, SWEEP_END_HZ);
/// The fast-sweep result is recomputed after every this many recorded samples (0.2 s).
const FAST_HOP: usize = SAMPLE_RATE as usize / 5;
/// Recording to skip when a loop starts (start-up latency, fade-in, the room filling).
const LIVE_SETTLE: usize = SAMPLE_RATE as usize * 3 / 4;
/// Fade-in of a loop, so it does not start with a click.
const LIVE_FADE_IN: usize = SAMPLE_RATE as usize / 4;
/// Weight of the newest slice in the live average (the rest is the previous average):
/// steadies the curve against room noise while following a change within a few updates.
const LIVE_NEW_WEIGHT: f64 = 0.5;
/// Below this, the noise is taken not to reach the mic, dB (see
/// [`Periodic::reaches_mic_db`]).
const LIVE_MIN_SNR_DB: f64 = 15.0;

/// How the placement screen measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A short sweep every couple of seconds: fine detail and the decay.
    Sweeps,
    /// A sweep looping without pauses, updated five times a second: detail and the decay.
    Fast,
    /// Continuous noise, updated ten times a second.
    Live,
}

impl Mode {
    /// The next mode, for the key that cycles through them.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Sweeps => Self::Fast,
            Self::Fast => Self::Live,
            Self::Live => Self::Sweeps,
        }
    }

    /// Its name, for the screen: "Fast sweeps".
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Sweeps => "Sweeps",
            Self::Fast => "Fast sweeps",
            Self::Live => "Live",
        }
    }
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
    /// How the bass decays (sweeps and fast sweeps).
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
            Mode::Sweeps | Mode::Fast => SWEEP_SMOOTHING_OCT,
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

    /// A reading from a sweep's (or a looped sweep's) quick analysis, with its waterfall.
    fn from_sweep(
        q: QuickAnalysis,
        mode: Mode,
        channel: Channel,
        peak_dbfs: f64,
        calibration: (&Calibration, SplScale),
    ) -> Self {
        let mut reading = Self::new(&q.power, q.bin_hz, mode, channel, peak_dbfs, calibration);
        reading.waterfall = Some(q.waterfall);
        reading
    }
}

/// A signal played in a seamless loop, and how often its result is recomputed.
struct Looped {
    signal: Periodic,
    samples: Arc<[f32]>,
    /// Recorded samples between updates.
    hop: usize,
}

impl Looped {
    fn new(signal: Periodic, hop: usize) -> Self {
        let samples = signal.samples_f32().into();
        Self {
            signal,
            samples,
            hop,
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
    /// Fast sweeps or live noise.
    Loop {
        stream: Stream,
        /// The latest recording, at most one period.
        recent: Vec<f32>,
        /// Samples recorded since the noise started.
        received: usize,
        /// `received` at the last update.
        updated_at: usize,
        /// The running average of the response power per bin (live noise).
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
    /// The most even reading of each speaker (by [`Channel::index`]) since the pin was set
    /// or the screen opened: where to put the trap back.
    pub best: [Option<Reading>; 2],
    /// The latest bass response of each speaker (by [`Channel::index`]), so the chart can
    /// keep one scale for both.
    pub last: [Option<Response>; 2],
    /// Readings since the screen opened (sweeps, or live updates).
    pub count: usize,
    pub issue: Option<String>,
    sweep: Sweep,
    signal: Arc<[f32]>,
    noise: Looped,
    fast: Looped,
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
    /// Sweeps mode on `channel`, at the measurement level `sweep_dbfs` (the looped signals
    /// peak at the same level).
    #[must_use]
    pub fn new(channel: Channel, sweep_dbfs: f64) -> Self {
        let sweep = Sweep::new(SweepParams {
            end_hz: SWEEP_END_HZ,
            len: SWEEP_LEN,
            peak_dbfs: sweep_dbfs,
            ..SweepParams::default()
        });
        let signal = sweep.samples_f32().into();
        let rate = f64::from(SAMPLE_RATE);
        let noise = Periodic::pink_noise(LIVE_PERIOD, LIVE_BAND, sweep_dbfs, rate);
        let fast = Periodic::sweep(FAST_PERIOD, FAST_BAND, sweep_dbfs, rate);
        Self {
            channel,
            mode: Mode::Sweeps,
            paused: false,
            latest: None,
            reference: None,
            best: [None, None],
            last: [None, None],
            count: 0,
            issue: None,
            sweep,
            signal,
            noise: Looped::new(noise, LIVE_HOP),
            fast: Looped::new(fast, FAST_HOP),
            run: None,
        }
    }

    /// True while something plays or is recorded.
    #[must_use]
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Starts measuring unless already running or paused: one sweep, or a loop.
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
            Mode::Fast | Mode::Live => {
                let looped = if self.mode == Mode::Fast {
                    &self.fast
                } else {
                    &self.noise
                };
                let program = Program::Loop {
                    signal: Arc::clone(&looped.samples),
                    fade_in: LIVE_FADE_IN,
                };
                Run::Loop {
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

    /// Switches to the next mode (sweeps, fast sweeps, live); the next start uses it.
    pub fn switch_mode(&mut self) {
        self.mode = self.mode.next();
        self.stop();
    }

    /// Pins the latest reading as the reference (or unpins when it already is). Either way
    /// the search for the best reading starts again.
    pub fn pin(&mut self) {
        if self.reference.is_some() && self.reference == self.latest {
            self.reference = None;
        } else {
            self.reference.clone_from(&self.latest);
        }
        self.best = [None, None];
        if let Some(r) = &self.reference {
            self.best[r.channel.index()] = Some(r.clone());
        }
    }

    /// The best reading of the speaker that is playing, when it is not the latest one.
    #[must_use]
    pub fn best_other_than_latest(&self) -> Option<&Reading> {
        let best = self.best[self.channel.index()].as_ref()?;
        (Some(best) != self.latest.as_ref()).then_some(best)
    }

    fn arrived(&mut self, reading: Reading) {
        let best = &mut self.best[reading.channel.index()];
        if best
            .as_ref()
            .is_none_or(|b| reading.score.evenness_db < b.score.evenness_db)
        {
            *best = Some(reading.clone());
        }
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
                        let reading =
                            Reading::from_sweep(q, mode, channel, peak_dbfs, (cal, scale));
                        self.arrived(reading);
                        true
                    }
                    Err(e) => {
                        self.issue = Some(e.to_string());
                        false
                    }
                }
            }
            Some(Run::Loop {
                stream,
                recent,
                received,
                updated_at,
                average,
            }) => {
                let looped = if mode == Mode::Fast {
                    &self.fast
                } else {
                    &self.noise
                };
                let new = stream.drain();
                *received += new.len();
                recent.extend(new);
                let period = looped.signal.len();
                if recent.len() > period {
                    recent.drain(..recent.len() - period);
                }
                if *received < LIVE_SETTLE + period || *received < *updated_at + looped.hop {
                    return false;
                }
                *updated_at = *received;
                if let Err(e) = check_clipping(recent) {
                    self.issue = Some(e.to_string());
                    return false;
                }
                if mode == Mode::Fast {
                    let ir = looped.signal.impulse_response(recent);
                    return match quick_ir_analysis(&ir, f64::from(SAMPLE_RATE)) {
                        Ok(q) => {
                            let reading =
                                Reading::from_sweep(q, mode, channel, peak_dbfs, (cal, scale));
                            self.arrived(reading);
                            true
                        }
                        Err(e) => {
                            self.issue = Some(e.to_string());
                            false
                        }
                    };
                }
                if looped.signal.reaches_mic_db(recent) < LIVE_MIN_SNR_DB {
                    self.issue = Some(
                        "the noise is not reaching the mic; check the output, the amplifier \
                         and that the mic points at the speakers"
                            .to_owned(),
                    );
                    return false;
                }
                let fresh = looped.signal.response_power(recent);
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
                    looped.signal.bin_hz(),
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
