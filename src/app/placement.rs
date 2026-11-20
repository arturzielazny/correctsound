//! Live placement and room treatment: the bass at the seat, measured over and over while
//! the speakers, the seat, the mic or a bass trap are moved, with a pinned reading to
//! compare against. The screen's state, its measuring loop and its keys.
//!
//! Two ways to measure, both with sweeps, switched with a key:
//!
//! - **Sweeps**: a short bass sweep every couple of seconds. Each gives the response at fine
//!   resolution and a waterfall of how the bass decays, which is where bass traps show.
//! - **Fast sweeps**: a one-second sweep played in a seamless loop
//!   ([`crate::dsp::periodic`]), the response and the waterfall recomputed from the latest
//!   second five times a second. A change shows fully within a second.
//!
//! Below about 200 Hz the room decides the response, and moving things by 20–50 cm changes
//! it more than any EQ can; it is also the only cure for cancellation dips.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use web_time::Instant;

use super::keys::KeyCode;
use super::{App, Env, Purpose, SIM_WALL_START_M};
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
/// Fast sweeps: one sweep is 1.02 s. Shorter, and the room would still be ringing from one
/// sweep when the next begins, which hides the decay; the waterfall needs 0.7 s after the
/// direct sound, and the sweep's distortion lands 0.15 s before it (wrapped around).
const FAST_PERIOD: usize = 3 << 14;
/// The fast sweep's range, Hz: the same as a single sweep's.
const FAST_BAND: (f64, f64) = (10.0, SWEEP_END_HZ);
/// The fast-sweep result is recomputed after every this many recorded samples (0.2 s of
/// sound), counted in audio rather than clock time so the simulator behaves like a room.
const FAST_HOP: usize = SAMPLE_RATE as usize / 5;
/// Recording to skip when a loop starts (start-up latency, fade-in, the room filling).
const LOOP_SETTLE: usize = SAMPLE_RATE as usize * 3 / 4;
/// Fade-in of a loop, so it does not start with a click.
const LOOP_FADE_IN: usize = SAMPLE_RATE as usize / 4;
/// A problem stays on screen at least this long, even if the next reading is fine, so it
/// can be read rather than flash by.
const ISSUE_HOLD: Duration = Duration::from_secs(4);

/// How the placement screen measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A short sweep every couple of seconds: fine detail and the decay.
    Sweeps,
    /// A sweep looping without pauses, updated five times a second: detail and the decay.
    Fast,
}

impl Mode {
    /// The next mode, for the key that cycles through them.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Sweeps => Self::Fast,
            Self::Fast => Self::Sweeps,
        }
    }

    /// Its name, for the screen: "Fast sweeps".
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Sweeps => "Sweeps",
            Self::Fast => "Fast sweeps",
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
        Self {
            channel,
            mode,
            score: Score::of(&calibrated(SCORE_SMOOTHING_OCT)),
            response: calibrated(SWEEP_SMOOTHING_OCT),
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
    /// Fast sweeps.
    Loop {
        stream: Stream,
        /// The latest recording, at most one period.
        recent: Vec<f32>,
        /// Samples recorded since the loop started.
        received: usize,
        /// `received` at the last update.
        updated_at: usize,
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
    /// Readings since the screen opened (sweeps, or updates of a looping sweep).
    pub count: usize,
    /// The latest problem, shown for at least [`ISSUE_HOLD`]; set with [`Self::report`].
    pub issue: Option<String>,
    issue_at: Option<Instant>,
    sweep: Sweep,
    signal: Arc<[f32]>,
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
        let fast = Periodic::sweep(FAST_PERIOD, FAST_BAND, sweep_dbfs, rate);
        Self {
            channel,
            mode: Mode::Sweeps,
            paused: false,
            latest: None,
            reference: None,
            last: [None, None],
            count: 0,
            issue: None,
            issue_at: None,
            sweep,
            signal,
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
            Mode::Fast => {
                let program = Program::Loop {
                    signal: Arc::clone(&self.fast.samples),
                    fade_in: LOOP_FADE_IN,
                };
                Run::Loop {
                    stream: audio.start(Source::new(program, self.channel), Route::Hardware)?,
                    recent: Vec::new(),
                    received: 0,
                    updated_at: 0,
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

    /// Switches to the next mode (sweeps, fast, fastest); the next start uses it.
    pub fn switch_mode(&mut self) {
        self.mode = self.mode.next();
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

    /// Shows a problem (it stays on screen for a few seconds).
    pub fn report(&mut self, issue: String) {
        self.issue = Some(issue);
        self.issue_at = Some(Instant::now());
    }

    fn arrived(&mut self, reading: Reading) {
        self.last[reading.channel.index()] = Some(reading.response.clone());
        self.latest = Some(reading);
        self.count += 1;
        if self.issue_at.is_none_or(|t| t.elapsed() >= ISSUE_HOLD) {
            self.issue = None;
        }
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
                        self.report(e.to_string());
                        false
                    }
                }
            }
            Some(Run::Loop {
                stream,
                recent,
                received,
                updated_at,
            }) => {
                let looped = &self.fast;
                let new = stream.drain();
                *received += new.len();
                recent.extend(new);
                let period = looped.signal.len();
                if recent.len() > period {
                    recent.drain(..recent.len() - period);
                }
                if *received < LOOP_SETTLE + period || *received < *updated_at + looped.hop {
                    return false;
                }
                *updated_at = *received;
                if let Err(e) = check_clipping(recent) {
                    self.report(e.to_string());
                    return false;
                }
                let ir = looped.signal.impulse_response(recent);
                match quick_ir_analysis(&ir, f64::from(SAMPLE_RATE)) {
                    Ok(q) => {
                        let reading =
                            Reading::from_sweep(q, mode, channel, peak_dbfs, (cal, scale));
                        self.arrived(reading);
                        true
                    }
                    Err(e) => {
                        self.report(e.to_string());
                        false
                    }
                }
            }
        }
    }
}

// ----- the step's keys and background work -----

impl App {
    pub(super) fn open_placement(&mut self) {
        if let Err(e) = self.pre_sweep_checks(Purpose::Measure) {
            self.measure.issue = Some(format!("{e:#}"));
            return;
        }
        self.measure.issue = None;
        self.message = None;
        let mut p = PlacementState::new(Channel::Left, self.measure.sweep_dbfs());
        if let Env::Sim { state, .. } = &self.env {
            let mut st = state.lock().expect("sim state lock poisoned");
            st.wall_m.get_or_insert(SIM_WALL_START_M);
        }
        if let Some(audio) = self.audio.as_mut()
            && let Err(e) = p.start(audio.as_mut())
        {
            p.report(format!("{e:#}"));
        }
        self.placement = Some(p);
    }

    /// Keys on the placement screen; false lets the key through to the global ones.
    pub(super) fn key_placement(&mut self, code: KeyCode) -> bool {
        let Some(p) = self.placement.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Esc | KeyCode::Char('p') => {
                self.placement = None;
                return true;
            }
            KeyCode::Char(' ') => p.pin(),
            KeyCode::Tab => p.switch_channel(),
            // The screen shows the mode; no message that would stay behind.
            KeyCode::Char('m') => p.switch_mode(),
            KeyCode::Char('s') => {
                p.paused = !p.paused;
                if p.paused {
                    p.stop();
                }
            }
            KeyCode::Left | KeyCode::Right => {
                let Env::Sim { state, .. } = &self.env else {
                    return true;
                };
                let step = if code == KeyCode::Right { 0.05 } else { -0.05 };
                // The status line shows the new distance.
                let mut st = state.lock().expect("sim state lock poisoned");
                let d = (st.wall_m.unwrap_or(SIM_WALL_START_M) + step).clamp(0.1, 2.0);
                st.wall_m = Some(d);
            }
            _ => return false,
        }
        if let Some(audio) = self.audio.as_mut()
            && let Some(p) = self.placement.as_mut()
            && let Err(e) = p.start(audio.as_mut())
        {
            p.report(format!("{e:#}"));
        }
        true
    }

    pub(super) fn tick_placement(&mut self) {
        let cal = self
            .cal
            .clone()
            .unwrap_or_else(|| Calibration::from_points(0.0, &[(20.0, 0.0), (20_000.0, 0.0)]));
        let scale = self.spl_scale();
        let Some(p) = self.placement.as_mut() else {
            return;
        };
        p.tick(&cal, scale);
        if let Some(audio) = self.audio.as_mut()
            && let Err(e) = p.start(audio.as_mut())
        {
            p.report(format!("{e:#}"));
            p.paused = true;
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
