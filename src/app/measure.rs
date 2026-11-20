//! Step 3 (and the verification pass): sweeps per channel, with checks and analysis; the
//! step's keys, and the session's settings saved with each measurement.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use web_time::Instant;

use super::keys::KeyCode;
use super::{App, Step};
use crate::analysis::{Responses, responses};
use crate::audio::{AudioBackend, Channel, Program, Route, Source, Stream};
use crate::cal::Calibration;
use crate::dsp::SAMPLE_RATE;
use crate::dsp::level::SplScale;
use crate::dsp::sweep::{IrWindow, Sweep, SweepAnalysis, SweepIssue, SweepParams};
use crate::platform::{Device, Routing, System};

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
            Self::Verify => Route::ThroughEq,
        }
    }
}

/// Phase of a single sweep in flight.
enum RunPhase {
    /// Waiting to confirm the playback stream is routed to the expected output.
    CheckingRouting,
    /// Routing confirmed (or not checked); playing and capturing until playback ends.
    Streaming,
    /// Playback ended; keeping capture open for [`TAIL`] to cover output latency.
    DrainingTail(Instant),
}

/// A sweep in progress.
struct Run {
    channel: Channel,
    stream: Stream,
    recording: Vec<f32>,
    total: usize,
    started: Instant,
    phase: RunPhase,
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
    /// Incremented whenever measurements are accepted or cleared, so derived state
    /// (designs, room guesses) can detect stale results even when sweep counts match.
    pub generation: u64,
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
            generation: 0,
        }
    }

    /// True when both channels have at least one measurement and the same number of positions.
    #[must_use]
    pub fn have_both(&self) -> bool {
        !self.positions[0].is_empty() && self.positions[0].len() == self.positions[1].len()
    }

    /// When one channel has fewer positions than the other (e.g. after a single-channel sweep
    /// failure), returns the lagging channel so the next sweep can complete the pair.
    #[must_use]
    pub fn missing_channel(&self) -> Option<Channel> {
        match self.positions[0].len().cmp(&self.positions[1].len()) {
            std::cmp::Ordering::Greater => Some(Channel::Right),
            std::cmp::Ordering::Less => Some(Channel::Left),
            std::cmp::Ordering::Equal => None,
        }
    }

    /// Clears all measurements and advances [`Self::generation`].
    pub fn clear(&mut self) {
        self.cancel();
        self.positions.iter_mut().for_each(Vec::clear);
        self.responses = [None, None];
        self.verify = [None, None];
        self.issue = None;
        self.generation += 1;
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
            phase: RunPhase::CheckingRouting,
        });
        Ok(())
    }

    /// Collects audio, checks routing (where `sink` is given), and reports a finished
    /// recording.
    pub fn tick(&mut self, system: &mut dyn System, sink: Option<&Device>) -> Tick {
        let Some(run) = &mut self.run else {
            return Tick::Idle;
        };
        run.recording.extend(run.stream.drain());
        if let Some(err) = run.stream.error() {
            self.fail(anyhow!("{err}"));
            return Tick::Failed;
        }
        match run.phase {
            RunPhase::CheckingRouting => {
                if run.stream.is_finished() {
                    run.phase = RunPhase::DrainingTail(Instant::now());
                } else if let Some(sink) = sink {
                    let elapsed = run.started.elapsed();
                    if elapsed > LINK_CHECK_AFTER {
                        let route = self.purpose.route();
                        let problem = match system.check_routing(route, sink) {
                            Ok(Routing::Ok) => {
                                run.phase = RunPhase::Streaming;
                                None
                            }
                            Ok(Routing::Pending) if elapsed < LINK_CHECK_GIVE_UP => None,
                            Ok(Routing::Pending) => Some(anyhow!(
                                "the sweep did not reach any output; nothing was measured. Try again"
                            )),
                            Ok(Routing::Wrong(why)) => {
                                Some(anyhow!("{why}; nothing was measured. Try again"))
                            }
                            Err(e) => Some(e),
                        };
                        if let Some(e) = problem {
                            self.fail(e);
                            return Tick::Failed;
                        }
                    }
                } else {
                    run.phase = RunPhase::Streaming;
                }
            }
            RunPhase::Streaming => {
                if run.stream.is_finished() {
                    run.phase = RunPhase::DrainingTail(Instant::now());
                }
            }
            RunPhase::DrainingTail(since) => {
                if since.elapsed() >= TAIL {
                    let run = self.run.take().expect("run checked above");
                    return Tick::Captured(run.channel, run.recording);
                }
            }
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
                self.generation += 1;
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

// ----- the step's keys and background work -----

impl App {
    pub(super) fn key_measure(&mut self, code: KeyCode) {
        if self.measure.running() {
            if code == KeyCode::Char('s') {
                self.measure.cancel();
                self.message = Some("Measurement stopped.".into());
            }
            return;
        }
        let have_both = self.measure.have_both();
        let has_any = self.measure.positions.iter().any(|p| !p.is_empty());
        match code {
            KeyCode::Char('p') => self.open_placement(),
            KeyCode::Tab => self.show_next_channels(),
            KeyCode::Enter if have_both => self.go(Step::Result),
            KeyCode::Enter | KeyCode::Char('m') => self.start_sweeps(Purpose::Measure),
            KeyCode::Char('a') if have_both => {
                // The simulated mic moves to its next position, as the person would.
                if let Some(state) = self.sim_state() {
                    state.lock().expect("sim state lock poisoned").mic_position += 1;
                }
                self.start_sweeps(Purpose::Measure);
            }
            KeyCode::Char('x') if has_any => {
                if let Some(state) = self.sim_state() {
                    state.lock().expect("sim state lock poisoned").mic_position = 0;
                }
                self.measure.clear();
                self.design = None;
                self.design_job = None;
                self.curiosities.clear();
                self.room_job = None;
                self.room.invalidate_measurement();
                self.room_stale = false;
                self.save.written.clear();
                self.save.error = None;
                if let super::InstallState::Installed { verify, .. } = &mut self.save.install {
                    *verify = None;
                }
                self.message = Some("Measurements cleared.".into());
            }
            _ => {}
        }
    }

    pub(super) fn start_sweeps(&mut self, purpose: Purpose) {
        if let Err(e) = self.pre_sweep_checks(purpose) {
            self.measure.issue = Some(format!("{e:#}"));
            return;
        }
        self.measure.issue = None;
        self.measure.queue = match (purpose, self.measure.missing_channel()) {
            (Purpose::Measure, Some(ch)) => vec![ch],
            _ => vec![Channel::Left, Channel::Right],
        };
        self.measure.purpose = purpose;
        self.next_sweep();
    }

    pub(super) fn pre_sweep_checks(&mut self, purpose: Purpose) -> Result<()> {
        let mic = self
            .mic()
            .cloned()
            .ok_or_else(|| anyhow!("the mic disappeared"))?;
        let gain = self.system.input_gain(&mic)?;
        if !gain.is_unity() {
            let now = gain.off_unity();
            self.mic_gain = Some(gain);
            return Err(anyhow!(
                "the mic gain changed (now {now}); go back to setup with Esc, where Enter sets it to unity again"
            ));
        }
        let now = self.current_sink_volume();
        if let (Some(frozen), Some(now)) = (self.measure.frozen_volume, now)
            && (frozen - now).abs() > 0.005
        {
            self.measure.frozen_volume = Some(now);
            return Err(anyhow!(
                "the output volume changed from {frozen:.2} to {now:.2} since the level was set; \
                     press Enter to measure at the new volume, or Esc to set the level again"
            ));
        }
        if purpose == Purpose::Verify && self.system.eq_bypass()? != Some(false) {
            return Err(anyhow!(
                "{} is bypassed or not running",
                self.eq_host_name()
            ));
        }
        Ok(())
    }

    pub(super) fn next_sweep(&mut self) {
        let Some(audio) = self.audio.as_mut() else {
            return;
        };
        if let Err(e) = self.measure.start_next(audio.as_mut()) {
            self.measure.issue = Some(format!("{e:#}"));
            self.measure.queue.clear();
        }
    }

    pub(super) fn tick_measure(&mut self) {
        let sink = self.sink().cloned();
        match self.measure.tick(self.system.as_mut(), sink.as_ref()) {
            Tick::Idle | Tick::Running => {}
            Tick::Failed => {}
            Tick::Captured(channel, recording) => self.sweep_done(channel, &recording),
        }
    }

    pub(super) fn sweep_done(&mut self, channel: Channel, recording: &[f32]) {
        let purpose = self.measure.purpose;
        let position = self.measure.positions[channel.index()].len();
        let saved = self.session().and_then(|s| match purpose {
            Purpose::Measure => s.save_capture(channel, position, recording),
            Purpose::Verify => s.save_verify_capture(channel, recording),
        });
        if let Err(e) = saved {
            self.set_error(&e);
        }
        match self.measure.analyze(recording) {
            Ok(analysis) => {
                let cal = self.cal.clone().unwrap_or_else(|| {
                    Calibration::from_points(0.0, &[(20.0, 0.0), (20_000.0, 0.0)])
                });
                let scale = self.spl_scale();
                self.measure.accept(channel, analysis, &cal, scale);
                if purpose == Purpose::Measure {
                    self.save_session_info();
                }
                if self.measure.queue.is_empty() {
                    if purpose == Purpose::Verify {
                        self.finish_verify();
                    }
                } else {
                    self.next_sweep();
                }
            }
            Err(issue) => {
                self.measure.issue = Some(format!("{} channel: {issue}", channel.name()));
                self.measure.queue.clear();
            }
        }
    }

    pub(super) fn save_session_info(&mut self) {
        let info = crate::session::SessionInfo {
            created: chrono::Local::now().to_rfc3339(),
            mic_serial: self.cal.as_ref().and_then(|c| c.serial.clone()),
            calibration_file: self.cal_path.clone(),
            sens_factor: self.cal.as_ref().map_or(0.0, |c| c.sens_factor),
            spl_offset_db: self.spl_scale().offset_db,
            sweep_dbfs: self.measure.sweep_dbfs(),
            sink: self.sink().map(|s| s.name.clone()).unwrap_or_default(),
            simulated: self.simulated(),
            positions: [
                self.measure.positions[0].len(),
                self.measure.positions[1].len(),
            ],
            ambient_spl: self.level.ambient_spl,
            level_spl: Some(self.level.set_spl().unwrap_or(self.level.target_spl)),
            needed_spl: self.level.room_noise.map(|_| self.level.target_spl),
            room_noise_spl: self.level.room_noise.map(|n| n.to_vec()),
            room: self.room.setup().filter(|_| !self.room.showing_guess()),
        };
        let result = self.session().and_then(|s| s.save_info(&info));
        if let Err(e) = result {
            self.set_error(&e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Options;
    use crate::app::keys::{KeyEvent, KeyModifiers};
    use crate::dsp::roommodel::{Guess, Room, Setup};

    fn synthetic_analysis() -> SweepAnalysis {
        let sweep = Sweep::new(SweepParams::default());
        let mut rec = vec![0.0f32; 24_000];
        rec.extend_from_slice(&sweep.samples_f32());
        rec.resize(rec.len() + 72_000, 0.0);
        SweepAnalysis::analyze(&sweep, &rec, IrWindow::default()).expect("synthetic sweep")
    }

    #[test]
    fn unbalanced_channels_retry_only_the_missing_channel() {
        let mut m = MeasureState::new();
        let cal = Calibration::from_points(0.0, &[(20.0, 0.0), (20_000.0, 0.0)]);
        let scale = SplScale { offset_db: 124.0 };
        let a = synthetic_analysis();

        assert!(!m.have_both());
        assert_eq!(m.missing_channel(), None);

        // Left succeeded, right failed.
        m.accept(Channel::Left, a.clone(), &cal, scale);
        assert!(!m.have_both());
        assert_eq!(m.missing_channel(), Some(Channel::Right));

        // Right catches up.
        m.accept(Channel::Right, a.clone(), &cal, scale);
        assert!(m.have_both());
        assert_eq!(m.missing_channel(), None);

        // Second position: left succeeded, right failed -> still unbalanced.
        m.accept(Channel::Left, a, &cal, scale);
        assert!(!m.have_both());
        assert_eq!(m.missing_channel(), Some(Channel::Right));
    }

    #[test]
    fn clear_invalidates_derived_state_and_advances_generation() {
        let mut app = App::new(Options {
            simulate: true,
            ..Options::default()
        });
        app.step = Step::Measure;
        let cal = Calibration::from_points(0.0, &[(20.0, 0.0), (20_000.0, 0.0)]);
        let scale = SplScale { offset_db: 124.0 };
        let a = synthetic_analysis();
        app.measure.accept(Channel::Left, a.clone(), &cal, scale);
        app.measure.accept(Channel::Right, a, &cal, scale);
        let gen_before = app.measure.generation;
        app.redesign();
        assert!(app.design.is_some());
        assert!(!app.curiosities.is_empty());
        app.room.set_guesses(
            gen_before,
            vec![Guess {
                setup: Setup::typical(Room {
                    length: 5.0,
                    width: 4.0,
                    height: 2.6,
                }),
                agreement_db: 2.0,
                balance_error_db: None,
            }],
        );
        assert!(app.room.guessed_for(gen_before));

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

        assert!(app.measure.positions.iter().all(Vec::is_empty));
        assert!(app.measure.responses.iter().all(Option::is_none));
        assert!(app.measure.generation > gen_before);
        assert!(app.design.is_none());
        assert!(app.curiosities.is_empty());
        assert!(app.room.guesses.is_empty());
        assert!(!app.room.guessed_for(gen_before));
        assert!(!app.room.guessed_for(app.measure.generation));
    }
}
