//! Live placement: short bass sweeps in a loop while the speakers, the seat or the mic are
//! moved, each redrawn as it arrives, with a pinned curve to compare against.
//!
//! Below about 200 Hz the room decides the response, and moving things by 20–50 cm changes
//! it more than any EQ can; it is also the only cure for cancellation dips.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::audio::{AudioBackend, Channel, Program, Route, Source, Stream};
use crate::cal::Calibration;
use crate::dsp::SAMPLE_RATE;
use crate::dsp::level::SplScale;
use crate::dsp::spectrum::{Response, Smoothing, octave_average_db, smooth_power};
use crate::dsp::sweep::{Sweep, SweepParams, quick_power};

/// Highest frequency shown, Hz.
pub const TOP_HZ: f64 = 500.0;
/// The band the evenness score covers, Hz.
pub const SCORE_FROM_HZ: f64 = 40.0;
pub const SCORE_TO_HZ: f64 = 250.0;
/// Smoothing of each reading, octaves.
const SMOOTHING_OCT: f64 = 1.0 / 24.0;
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

/// One sweep's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub channel: Channel,
    /// dB SPL from 20 Hz to [`TOP_HZ`].
    pub response: Response,
    pub score: Score,
}

struct Run {
    stream: Stream,
    recording: Vec<f32>,
    finished_at: Option<Instant>,
}

/// The placement screen's state and its sweep loop.
pub struct PlacementState {
    /// The speaker the next sweep plays from.
    pub channel: Channel,
    pub paused: bool,
    pub latest: Option<Reading>,
    /// A pinned reading to compare against.
    pub reference: Option<Reading>,
    /// The latest bass response of each speaker (by [`Channel::index`]), so the chart can
    /// keep one scale for both.
    pub last: [Option<Response>; 2],
    /// Sweeps analysed since the screen opened.
    pub count: usize,
    pub issue: Option<String>,
    sweep: Sweep,
    signal: Arc<[f32]>,
    run: Option<Run>,
}

impl std::fmt::Debug for PlacementState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlacementState")
            .field("channel", &self.channel)
            .field("paused", &self.paused)
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl PlacementState {
    #[must_use]
    pub fn new(channel: Channel, sweep_dbfs: f64) -> Self {
        let sweep = Sweep::new(SweepParams {
            end_hz: SWEEP_END_HZ,
            len: SWEEP_LEN,
            peak_dbfs: sweep_dbfs,
            ..SweepParams::default()
        });
        let signal = sweep.samples_f32().into();
        Self {
            channel,
            paused: false,
            latest: None,
            reference: None,
            last: [None, None],
            count: 0,
            issue: None,
            sweep,
            signal,
            run: None,
        }
    }

    /// True while a sweep plays or its tail is recorded.
    #[must_use]
    pub fn sweeping(&self) -> bool {
        self.run.is_some()
    }

    /// Starts a sweep unless one is running or the loop is paused.
    ///
    /// # Errors
    ///
    /// Fails if the audio streams cannot start.
    pub fn start(&mut self, audio: &mut dyn AudioBackend) -> Result<()> {
        if self.run.is_some() || self.paused {
            return Ok(());
        }
        let program = Program::Once {
            signal: Arc::clone(&self.signal),
            pre: PRE,
            post: POST,
        };
        let stream = audio.start(Source::new(program, self.channel), Route::Hardware)?;
        self.run = Some(Run {
            stream,
            recording: Vec::new(),
            finished_at: None,
        });
        Ok(())
    }

    /// Stops any sweep.
    pub fn stop(&mut self) {
        self.run = None;
    }

    /// Pins the latest reading as the reference (or unpins when it already is).
    pub fn pin(&mut self) {
        if self.reference.is_some() && self.reference == self.latest {
            self.reference = None;
        } else {
            self.reference.clone_from(&self.latest);
        }
    }

    /// Collects audio; returns true when a new reading arrived. Call [`Self::start`]
    /// afterwards to keep the loop going.
    pub fn tick(&mut self, cal: &Calibration, scale: SplScale) -> bool {
        let Some(run) = &mut self.run else {
            return false;
        };
        run.recording.extend(run.stream.drain());
        if run.stream.is_finished() && run.finished_at.is_none() {
            run.finished_at = Some(Instant::now());
        }
        if run.finished_at.is_none_or(|t| t.elapsed() < TAIL) {
            return false;
        }
        let run = self.run.take().expect("run checked above");
        match quick_power(&self.sweep, &run.recording) {
            Ok((power, bin_hz)) => {
                let mut r = smooth_power(&power, bin_hz, Smoothing::Fixed(SMOOTHING_OCT));
                let keep = r.freqs.partition_point(|&f| f <= TOP_HZ);
                r.freqs.truncate(keep);
                r.db.truncate(keep);
                let dbfs = self.sweep.params().peak_dbfs;
                for (d, &f) in r.db.iter_mut().zip(&r.freqs) {
                    *d = scale.spl(*d + dbfs) - cal.db_at(f);
                }
                self.last[self.channel.index()] = Some(r.clone());
                self.latest = Some(Reading {
                    channel: self.channel,
                    score: Score::of(&r),
                    response: r,
                });
                self.count += 1;
                self.issue = None;
                true
            }
            Err(e) => {
                self.issue = Some(e.to_string());
                false
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
