//! Step 5 (and the verification pass): sweeps per channel, with checks and analysis.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use crate::analysis::{Responses, responses};
use crate::audio::pipewire::PLAYBACK_NODE;
use crate::audio::{AudioBackend, Channel, Program, Route, Source, Stream};
use crate::cal::Calibration;
use crate::dsp::SAMPLE_RATE;
use crate::dsp::level::SplScale;
use crate::dsp::sweep::{IrWindow, Sweep, SweepAnalysis, SweepIssue, SweepParams};
use crate::system::System;

/// Silence recorded before the sweep, used as the noise-floor estimate.
const PRE_ROLL: usize = SAMPLE_RATE as usize / 2;
/// Silence recorded after the sweep, for the room's decay.
const POST_ROLL: usize = SAMPLE_RATE as usize * 3 / 2;
/// Extra capture kept after playback ends, covering output latency.
const TAIL: Duration = Duration::from_millis(400);
/// When to check where the playback stream is linked.
const LINK_CHECK_AFTER: Duration = Duration::from_millis(900);
const LINK_CHECK_GIVE_UP: Duration = Duration::from_millis(3000);

/// Why sweeps are being run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Measuring the room, straight to the hardware sink.
    Measure,
    /// Checking the applied EQ, through EasyEffects.
    Verify,
}

impl Purpose {
    fn route(self) -> Route {
        match self {
            Self::Measure => Route::Hardware,
            Self::Verify => Route::EasyEffects,
        }
    }
}

/// A sweep in progress.
struct Run {
    channel: Channel,
    stream: Stream,
    recording: Vec<f32>,
    total: usize,
    started: Instant,
    finished_at: Option<Instant>,
    links_ok: bool,
}

/// Result of one tick.
pub enum Tick {
    Idle,
    Running,
    Failed,
    Captured(Channel, Vec<f32>),
}

/// Sweep runs and their results.
pub struct MeasureState {
    sweep: Arc<Sweep>,
    signal: Arc<[f32]>,
    run: Option<Run>,
    pub queue: Vec<Channel>,
    pub purpose: Purpose,
    /// Analyses per channel, one per mic position.
    pub positions: [Vec<SweepAnalysis>; 2],
    /// Position-averaged responses.
    pub responses: [Option<Responses>; 2],
    /// Responses measured through EasyEffects.
    pub verify: [Option<Responses>; 2],
    /// A problem with the last attempt, in plain language.
    pub issue: Option<String>,
    /// Sink volume when the level was set.
    pub frozen_volume: Option<f64>,
}

impl std::fmt::Debug for MeasureState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeasureState")
            .field("running", &self.running())
            .field(
                "positions",
                &[self.positions[0].len(), self.positions[1].len()],
            )
            .finish_non_exhaustive()
    }
}

impl MeasureState {
    #[must_use]
    pub fn new() -> Self {
        let sweep = Sweep::new(SweepParams::default());
        let signal: Arc<[f32]> = sweep.samples_f32().into();
        Self {
            sweep: Arc::new(sweep),
            signal,
            run: None,
            queue: Vec::new(),
            purpose: Purpose::Measure,
            positions: [Vec::new(), Vec::new()],
            responses: [None, None],
            verify: [None, None],
            issue: None,
            frozen_volume: None,
        }
    }

    /// Sweep level, dBFS.
    #[must_use]
    pub fn sweep_dbfs(&self) -> f64 {
        self.sweep.params().peak_dbfs
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    /// The channel being swept and its progress (0..1).
    #[must_use]
    pub fn progress(&self) -> Option<(Channel, f64)> {
        let run = self.run.as_ref()?;
        Some((
            run.channel,
            (run.recording.len() as f64 / run.total as f64).min(1.0),
        ))
    }

    /// Stops any sweep and clears the queue.
    pub fn cancel(&mut self) {
        self.run = None;
        self.queue.clear();
    }

    /// Starts the next queued channel.
    ///
    /// # Errors
    ///
    /// Fails if the audio streams cannot start.
    pub fn start_next(&mut self, audio: &mut dyn AudioBackend) -> Result<()> {
        if self.queue.is_empty() {
            return Ok(());
        }
        let channel = self.queue.remove(0);
        let program = Program::Once {
            signal: Arc::clone(&self.signal),
            pre: PRE_ROLL,
            post: POST_ROLL,
        };
        let total = PRE_ROLL + self.signal.len() + POST_ROLL;
        let stream = audio.start(Source::new(program, channel), self.purpose.route())?;
        self.run = Some(Run {
            channel,
            stream,
            recording: Vec::with_capacity(total + SAMPLE_RATE as usize),
            total,
            started: Instant::now(),
            finished_at: None,
            links_ok: false,
        });
        Ok(())
    }

    /// Collects audio, checks routing, and reports a finished recording.
    pub fn tick(&mut self, system: &mut dyn System, check_links: bool, sink: &str) -> Tick {
        let Some(run) = &mut self.run else {
            return Tick::Idle;
        };
        run.recording.extend(run.stream.drain());
        let elapsed = run.started.elapsed();
        if check_links && !run.links_ok && elapsed > LINK_CHECK_AFTER {
            let expected = match self.purpose {
                Purpose::Measure => sink,
                Purpose::Verify => "easyeffects_sink",
            };
            match system.playback_links(PLAYBACK_NODE) {
                Ok(links) if links.iter().all(|l| l == expected) && !links.is_empty() => {
                    run.links_ok = true
                }
                Ok(links) if links.is_empty() && elapsed < LINK_CHECK_GIVE_UP => {}
                Ok(links) => {
                    let to = if links.is_empty() {
                        "nothing".to_owned()
                    } else {
                        links.join(", ")
                    };
                    self.fail(anyhow!(
                        "the sweep was routed to {to} instead of {expected}; nothing was measured. \
                         Check EasyEffects' settings and try again"
                    ));
                    return Tick::Failed;
                }
                Err(e) => {
                    self.fail(e);
                    return Tick::Failed;
                }
            }
        }
        if run.stream.is_finished() && run.finished_at.is_none() {
            run.finished_at = Some(Instant::now());
        }
        if run.finished_at.is_some_and(|t| t.elapsed() >= TAIL) {
            let run = self.run.take().expect("run checked above");
            return Tick::Captured(run.channel, run.recording);
        }
        Tick::Running
    }

    fn fail(&mut self, e: anyhow::Error) {
        self.issue = Some(format!("{e:#}"));
        self.cancel();
    }

    /// Analyses a recording.
    ///
    /// # Errors
    ///
    /// Returns the [`SweepIssue`] that makes the recording unusable.
    pub fn analyze(&self, recording: &[f32]) -> Result<SweepAnalysis, SweepIssue> {
        SweepAnalysis::analyze(&self.sweep, recording, IrWindow::default())
    }

    /// Stores an analysis and refreshes the channel's averaged response.
    pub fn accept(
        &mut self,
        channel: Channel,
        analysis: SweepAnalysis,
        cal: &Calibration,
        scale: SplScale,
    ) {
        let c = channel.index();
        let sweep_dbfs = self.sweep_dbfs();
        match self.purpose {
            Purpose::Measure => {
                self.positions[c].push(analysis);
                self.responses[c] = Some(responses(&self.positions[c], cal, scale, sweep_dbfs));
            }
            Purpose::Verify => {
                self.verify[c] = Some(responses(
                    std::slice::from_ref(&analysis),
                    cal,
                    scale,
                    sweep_dbfs,
                ));
            }
        }
    }
}

impl Default for MeasureState {
    fn default() -> Self {
        Self::new()
    }
}
