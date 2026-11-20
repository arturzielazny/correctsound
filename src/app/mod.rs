//! The wizard: a linear state machine driven by key presses and periodic ticks.
//!
//! Nothing here touches the terminal; [`crate::ui`] renders an [`App`].

pub mod keys;
pub mod level;
pub mod measure;
pub mod placement;
mod result;
pub mod room;
mod save;
mod setup;

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use keys::{KeyCode, KeyEvent, KeyModifiers};
use web_time::Instant;

pub use level::{LevelPhase, LevelState};
pub use measure::{MeasureState, Purpose};
pub use placement::PlacementState;

use crate::audio::sim::{SimConfig, SimState};
use crate::audio::{AudioBackend, Channel};
use crate::cal::{Calibration, Orientation};
use crate::config::Config;
use crate::dsp::level::SplScale;
use crate::eq::fit::Design;
use crate::eq::target::DesignOptions;
use crate::platform::{Applied, Device, Devices, InputGain, System};
use crate::session::Session;

/// Command-line options that shape the wizard.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub simulate: bool,
    /// Use this calibration file instead of downloading one.
    pub cal_file: Option<PathBuf>,
    /// Use this input (by name) instead of auto-detecting a UMIK-1.
    pub source: Option<String>,
    /// Override the dBFS → dB SPL offset.
    pub spl_offset: Option<f64>,
    /// Preselect this sink.
    pub sink: Option<String>,
    /// Open this saved session for review instead of measuring.
    pub review: Option<PathBuf>,
    /// The simulated room and speakers, when simulating ([`SimConfig::living_room`] if not
    /// set).
    pub sim_config: Option<SimConfig>,
    /// The simulated devices as they start (which outputs, whether the mic is plugged in).
    pub sim_state: Option<SimState>,
}

/// A screen shown over the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// How it works; `technical` for the detailed version.
    Info { technical: bool },
    /// What the measurement says about the room and the speakers.
    Curiosities,
    /// How the bass fades after the sound stops (the waterfall), for `view_channel`.
    Decay,
    /// The room seen from above: its standing waves, the speakers and the seat.
    Room,
}

/// Wizard steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// The mic, its calibration and gain, and the output, on one screen.
    Setup,
    Level,
    Measure,
    Result,
    Save,
}

impl Step {
    pub const ALL: [Self; 5] = [
        Self::Setup,
        Self::Level,
        Self::Measure,
        Self::Result,
        Self::Save,
    ];

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Setup => "Setup",
            Self::Level => "Level",
            Self::Measure => "Measure",
            Self::Result => "Result",
            Self::Save => "Save",
        }
    }

    fn prev(self) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[i.saturating_sub(1)]
    }
}

/// Calibration download progress.
#[derive(Debug, Clone, PartialEq)]
pub enum CalStatus {
    /// Nothing downloaded in this run (a saved file may be loaded).
    Idle,
    Downloading,
    /// Downloaded from miniDSP in this run, and saved.
    Downloaded,
    Failed(String),
}

/// Save & apply progress.
#[derive(Debug, Clone, Default)]
pub struct SaveState {
    pub written: Vec<PathBuf>,
    pub error: Option<String>,
    pub applied: Option<Applied>,
    /// Verified against predicted, per channel.
    pub verify: Option<[VerifyResult; 2]>,
    /// What rolling the EQ host back did (the EQ is no longer applied).
    pub rolled_back: Option<String>,
}

/// How a verification sweep compares with the prediction in the corrected range,
/// after removing the broadband offset (preamp and sink volume).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VerifyResult {
    /// RMS of the difference, dB.
    pub rms_db: f64,
    /// Largest difference (signed) and where.
    pub worst_db: f64,
    pub worst_hz: f64,
}

impl VerifyResult {
    /// Above this RMS the EQ is not doing what was modelled.
    pub const RMS_OK_DB: f64 = 2.0;
    /// A single spot differing by more than this is worth mentioning.
    pub const SPOT_NOTE_DB: f64 = 3.0;
}

/// Where the simulated speakers start, metres from the wall behind them, once the
/// placement screen is opened.
const SIM_WALL_START_M: f64 = 0.6;

/// Where audio and system control come from.
enum Env {
    Real,
    Sim {
        config: Box<SimConfig>,
        state: Arc<Mutex<SimState>>,
    },
}

/// The whole wizard state.
pub struct App {
    pub step: Step,
    pub quit: bool,
    pub options: Options,
    env: Env,
    system: Box<dyn System>,
    audio: Option<Box<dyn AudioBackend>>,
    config: Config,
    /// One-line message shown at the bottom (errors, confirmations).
    pub message: Option<String>,
    /// A screen over the current step, and how far it is scrolled.
    pub overlay: Option<Overlay>,
    pub overlay_scroll: u16,
    /// Findings about the room and speakers, refreshed with the design.
    pub curiosities: Vec<crate::insights::Section>,
    /// The room screen.
    pub room: room::RoomState,
    pub missing_tools: Vec<String>,

    pub devices: Option<Devices>,
    pub sink_index: usize,
    /// The output the newest measurement (real or simulated, like this run) was made on.
    pub last_measured_sink: Option<String>,
    last_poll: Option<Instant>,

    pub serial_input: String,
    pub orientation: Orientation,
    pub cal: Option<Calibration>,
    pub cal_path: Option<PathBuf>,
    pub cal_status: CalStatus,
    cal_rx: Option<Receiver<Result<(Calibration, PathBuf)>>>,
    /// Enter was pressed before the calibration arrived: setup continues when it does.
    pub continue_after_download: bool,

    pub mic_gain: Option<InputGain>,
    /// The EQ host being bypassed beside the screen, after setup.
    eq_bypass_job: Option<Receiver<Result<()>>>,

    pub level: LevelState,
    pub measure: MeasureState,
    /// The live placement loop, while its screen is open (on the measure step).
    pub placement: Option<PlacementState>,

    pub design_options: DesignOptions,
    pub design: Option<Design>,
    /// A design being worked out in the background after a key changed the options, tagged
    /// with the measurement generation it was started from.
    design_job: Option<Receiver<(u64, Design)>>,
    /// Designs worked out so far (in the background or not).
    pub designs_run: usize,
    /// The room guessed in the background: the measurement generation and the guesses.
    room_job: Option<Receiver<(u64, Vec<crate::dsp::roommodel::Guess>)>>,
    /// True when keys changed the room screen since it was last worked out.
    room_stale: bool,
    /// The speaker the room screen and the decay view show (0 left, 1 right).
    pub view_channel: usize,
    /// The channels the response charts show (measure, result, save).
    pub shown: Shown,

    pub save: SaveState,
    session: Option<Session>,
}

/// Which channels the response charts show: both, or one of them. Tab steps through them on
/// the measure, result and save screens, and the choice stays from one screen to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Shown {
    #[default]
    Both,
    Left,
    Right,
}

impl Shown {
    /// The next one for Tab: both, the left only, the right only, both again.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Both => Self::Left,
            Self::Left => Self::Right,
            Self::Right => Self::Both,
        }
    }

    /// The channels shown, left first (0 left, 1 right).
    #[must_use]
    pub fn channels(self) -> &'static [usize] {
        match self {
            Self::Both => &[0, 1],
            Self::Left => &[0],
            Self::Right => &[1],
        }
    }
}

impl Drop for App {
    /// A bypass still under way finishes first, so what it changes is registered before the
    /// system is restored.
    fn drop(&mut self) {
        if let Some(job) = self.eq_bypass_job.take() {
            let _ = crate::background::wait(&job);
        }
    }
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("step", &self.step)
            .finish_non_exhaustive()
    }
}

impl App {
    /// Creates the wizard for real hardware or the simulator.
    #[must_use]
    pub fn new(options: Options) -> Self {
        let config = if options.simulate {
            Config::default()
        } else {
            Config::load()
        };
        let (env, system, missing_tools): (Env, Box<dyn System>, Vec<String>) = if options.simulate
        {
            let state = Arc::new(Mutex::new(options.sim_state.clone().unwrap_or_default()));
            let config = SimConfig {
                speed: 4.0,
                ..options
                    .sim_config
                    .clone()
                    .unwrap_or_else(SimConfig::living_room)
            };
            (
                Env::Sim {
                    config: Box::new(config.clone()),
                    state: Arc::clone(&state),
                },
                Box::new(crate::platform::sim::SimSystem::new(
                    config,
                    Arc::clone(&state),
                )),
                Vec::new(),
            )
        } else {
            let system = crate::platform::native();
            let missing = system.missing();
            (Env::Real, system, missing)
        };
        let serial_input = config
            .serial
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let orientation = config.orientation;
        let simulated = matches!(env, Env::Sim { .. });
        let last_measured_sink = Session::list()
            .into_iter()
            .find(|(_, info)| info.simulated == simulated)
            .map(|(_, info)| info.sink);
        let mut app = Self {
            step: Step::Setup,
            quit: false,
            options,
            env,
            system,
            audio: None,
            config,
            message: None,
            overlay: None,
            overlay_scroll: 0,
            curiosities: Vec::new(),
            room: room::RoomState::default(),
            missing_tools,
            devices: None,
            sink_index: 0,
            last_measured_sink,
            last_poll: None,
            serial_input,
            orientation,
            cal: None,
            cal_path: None,
            cal_status: CalStatus::Idle,
            cal_rx: None,
            continue_after_download: false,
            mic_gain: None,
            eq_bypass_job: None,
            level: LevelState::new(),
            measure: MeasureState::new(),
            placement: None,
            design_options: DesignOptions::default(),
            design: None,
            design_job: None,
            designs_run: 0,
            room_job: None,
            room_stale: false,
            view_channel: 0,
            shown: Shown::Both,
            save: SaveState::default(),
            session: None,
        };
        if app.options.simulate
            && let Env::Sim { config, .. } = &app.env
        {
            app.cal = Some(config.calibration());
            app.serial_input = "7000000".into();
        }
        if app.options.review.is_none() {
            app.load_saved_calibration();
        }
        app.poll_devices();
        if let Some(old) = app.config.room.take()
            && !app.config.rooms.contains(&old)
        {
            app.config.rooms.insert(0, old);
        }
        app.room.load(app.config.rooms.clone(), None);
        if let Some(dir) = app.options.review.clone()
            && let Err(e) = app.open_session(&dir)
        {
            app.set_error(&e);
        }
        app
    }

    /// True when a saved session is being reviewed (nothing is measured).
    #[must_use]
    pub fn reviewing(&self) -> bool {
        self.options.review.is_some()
    }

    /// Loads a saved session: analyses its recordings again (with this version's analysis)
    /// and shows the result. Nothing is played or recorded.
    ///
    /// # Errors
    ///
    /// Fails if the session, its calibration or a recording cannot be read or analysed.
    pub fn open_session(&mut self, dir: &std::path::Path) -> Result<()> {
        let session = Session::open(dir);
        let info = session.load_info()?;
        let (cal, cal_path) = match (&self.options.cal_file, &info.calibration_file) {
            (Some(path), _) => (crate::cal::load(path)?, Some(path.clone())),
            (None, Some(path)) => (crate::cal::load(path)?, Some(path.clone())),
            (None, None) if info.simulated => (SimConfig::default().calibration(), None),
            (None, None) => {
                return Err(anyhow!(
                    "the session names no calibration file; pass --cal-file"
                ));
            }
        };
        let scale = SplScale {
            offset_db: info.spl_offset_db,
        };
        let sweep = crate::dsp::sweep::Sweep::new(crate::dsp::sweep::SweepParams::default());
        // Every recording is analysed at once (each on its own thread where there are
        // threads), then taken in order.
        let recordings: Vec<(Channel, usize)> = Channel::BOTH
            .into_iter()
            .flat_map(|c| (0..info.positions[c.index()]).map(move |p| (c, p)))
            .collect();
        let mut analysed = crate::background::map(recordings, |(c, p)| {
            let rec = crate::session::read_wav(&session.capture_path(c, p))?;
            crate::dsp::sweep::SweepAnalysis::analyze(
                &sweep,
                &rec,
                crate::dsp::sweep::IrWindow::default(),
            )
            .map_err(|e| anyhow!("{} position {}: {e}", c.name(), p + 1))
        })
        .into_iter();
        for c in Channel::BOTH {
            let runs = analysed
                .by_ref()
                .take(info.positions[c.index()])
                .collect::<Result<Vec<_>>>()?;
            self.measure.responses[c.index()] = Some(crate::analysis::responses(
                &runs,
                &cal,
                scale,
                info.sweep_dbfs,
            ));
            self.measure.positions[c.index()] = runs;
        }
        self.measure.generation += 1;
        self.cal_path = cal_path;
        self.cal = Some(cal);
        self.level.ambient_spl = info.ambient_spl;
        if let Some(l) = info.needed_spl.or(info.level_spl) {
            self.level.target_spl = l;
        }
        self.level.reviewed_set_spl = info.level_spl;
        self.room.load(self.room.yours.clone(), info.room);
        // The output it was measured on, for applying.
        self.options.sink = Some(info.sink.clone());
        self.poll_devices();
        self.session = Some(session);
        self.step = Step::Result;
        self.redesign();
        self.start_room_guess();
        Ok(())
    }

    /// True when running against the simulator.
    #[must_use]
    pub fn simulated(&self) -> bool {
        matches!(self.env, Env::Sim { .. })
    }

    // ----- shared helpers -----

    /// The selected sink.
    #[must_use]
    pub fn sink(&self) -> Option<&Device> {
        self.devices.as_ref()?.sinks.get(self.sink_index)
    }

    /// The mic node.
    #[must_use]
    pub fn mic(&self) -> Option<&Device> {
        self.devices.as_ref()?.mic.as_ref()
    }

    /// dBFS → dB SPL conversion for the loaded calibration.
    #[must_use]
    pub fn spl_scale(&self) -> SplScale {
        let sens = self.cal.as_ref().map_or(0.0, |c| c.sens_factor);
        let mut s = SplScale::from_sens_factor(sens);
        if let Some(o) = self.options.spl_offset {
            s.offset_db = o;
        }
        s
    }

    /// True when the SPL reading is approximate (old-generation mic or no UMIK).
    #[must_use]
    pub fn spl_is_approximate(&self) -> bool {
        self.options.spl_offset.is_none()
            && (self.options.source.is_some()
                || self
                    .cal
                    .as_ref()
                    .is_some_and(Calibration::is_old_generation))
    }

    fn set_error(&mut self, e: &anyhow::Error) {
        self.message = Some(format!("{e:#}"));
    }

    fn session(&mut self) -> Result<&Session> {
        if self.session.is_none() {
            self.session = Some(Session::create()?);
        }
        self.session.as_ref().ok_or_else(|| anyhow!("no session"))
    }

    /// Session directory, once created.
    #[must_use]
    pub fn session_dir(&self) -> Option<PathBuf> {
        self.session.as_ref().map(|s| s.dir().to_path_buf())
    }

    fn go(&mut self, step: Step) {
        self.leave_step();
        self.message = None;
        self.step = step;
        self.enter_step();
    }

    fn leave_step(&mut self) {
        match self.step {
            Step::Level => self.level.stop(),
            Step::Measure | Step::Save => {
                self.measure.cancel();
                self.placement = None;
            }
            _ => {}
        }
    }

    fn enter_step(&mut self) {
        match self.step {
            Step::Setup => self.read_mic_gain(),
            Step::Level => {
                let volume = self.current_sink_volume();
                self.level.enter(volume);
                if !self.bypassing_eq() {
                    self.start_listening();
                }
            }
            Step::Result => {
                self.redesign();
                self.start_room_guess();
            }
            Step::Save => self.write_outputs(),
            _ => {}
        }
    }

    /// Listens to the room, the level step's start.
    fn start_listening(&mut self) {
        if let Some(audio) = self.audio.as_mut()
            && let Err(e) = self.level.start_listening(audio.as_mut())
        {
            self.set_error(&e);
        }
    }

    fn current_sink_volume(&mut self) -> Option<f64> {
        let sink = self.sink()?.clone();
        self.system.output_volume(&sink).ok()
    }

    // ----- events -----

    /// Handles one key press.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if matches!(key.code, KeyCode::Char('c' | 'C')) {
                self.quit = true;
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            return;
        }
        if self.overlay.is_some() {
            self.key_overlay(key.code);
            return;
        }
        if self.placement.is_some() && self.key_placement(key.code) {
            return;
        }
        match key.code {
            KeyCode::Char('i') => {
                self.open_overlay(Overlay::Info { technical: false });
                return;
            }
            KeyCode::Char('c') => {
                if self.curiosities.is_empty() {
                    self.message =
                        Some("Curiosities appear once both speakers are measured.".into());
                } else {
                    self.open_overlay(Overlay::Curiosities);
                }
                return;
            }
            KeyCode::Char('w') => {
                if self.measure.positions.iter().any(|p| !p.is_empty()) {
                    if self.measure.positions[self.view_channel].is_empty() {
                        self.view_channel = 1 - self.view_channel;
                    }
                    self.open_overlay(Overlay::Decay);
                } else {
                    self.message =
                        Some("The decay view appears once a speaker is measured.".into());
                }
                return;
            }
            KeyCode::Char('r') if self.step >= Step::Measure => {
                if self.measure.responses.iter().all(Option::is_some) {
                    self.refresh_room();
                    self.open_overlay(Overlay::Room);
                } else {
                    self.message =
                        Some("The room screen appears once both speakers are measured.".into());
                }
                return;
            }
            KeyCode::Char('q') => {
                self.quit = true;
                return;
            }
            KeyCode::Esc if self.reviewing() && self.step == Step::Result => {
                self.message =
                    Some("Reviewing a saved session: nothing to measure. q quits.".into());
                return;
            }
            KeyCode::Esc => {
                if self.step != Step::Setup {
                    let prev = self.step.prev();
                    self.go(prev);
                }
                return;
            }
            _ => {}
        }
        match self.step {
            Step::Setup => self.key_setup(key.code),
            Step::Level => self.key_level(key.code),
            Step::Measure => self.key_measure(key.code),
            Step::Result => self.key_result(key.code),
            Step::Save => self.key_save(key.code),
        }
    }

    /// Tab on a screen of response charts: the next channels. The decay view, opened from
    /// there, then starts on the one channel shown.
    pub(super) fn show_next_channels(&mut self) {
        self.shown = self.shown.next();
        if let [c] = self.shown.channels() {
            self.view_channel = *c;
        }
    }

    fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
        self.overlay_scroll = 0;
    }

    /// Keys while an overlay is shown: scrolling, `m` for the technical version, and the
    /// overlay's own key, Esc or q to close it.
    fn key_overlay(&mut self, code: KeyCode) {
        if self.overlay == Some(Overlay::Room) {
            self.key_room(code);
            return;
        }
        match (code, self.overlay) {
            (KeyCode::Char('m'), Some(Overlay::Info { technical })) => {
                self.open_overlay(Overlay::Info {
                    technical: !technical,
                });
            }
            (KeyCode::Char('c'), Some(Overlay::Info { .. })) if !self.curiosities.is_empty() => {
                self.open_overlay(Overlay::Curiosities);
            }
            (KeyCode::Char('i'), Some(Overlay::Curiosities | Overlay::Decay)) => {
                self.open_overlay(Overlay::Info { technical: false });
            }
            (KeyCode::Tab, Some(Overlay::Decay)) => {
                let other = 1 - self.view_channel;
                if !self.measure.positions[other].is_empty() {
                    self.view_channel = other;
                }
            }
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                self.overlay_scroll = self.overlay_scroll.saturating_add(1);
            }
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                self.overlay_scroll = self.overlay_scroll.saturating_sub(1);
            }
            (KeyCode::PageDown | KeyCode::Char(' '), _) => {
                self.overlay_scroll = self.overlay_scroll.saturating_add(10);
            }
            (KeyCode::PageUp, _) => self.overlay_scroll = self.overlay_scroll.saturating_sub(10),
            (KeyCode::Home, _) => self.overlay_scroll = 0,
            (KeyCode::Esc | KeyCode::Char('q' | 'i' | 'c' | 'w') | KeyCode::Enter, _) => {
                self.overlay = None;
            }
            _ => {}
        }
    }

    /// Advances background work: device polling, downloads, audio streams.
    pub fn tick(&mut self) {
        self.poll_eq_bypass();
        self.poll_design();
        self.collect_room_guess(false);
        self.settle_room();
        match self.step {
            Step::Setup => {
                let due = self
                    .last_poll
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(1));
                if due && self.mic().is_none() {
                    self.poll_devices();
                }
                self.poll_download();
            }
            Step::Level => {
                let scale = self.spl_scale();
                self.level.tick(scale, self.cal.as_ref());
            }
            Step::Measure if self.placement.is_some() => self.tick_placement(),
            Step::Measure | Step::Save => self.tick_measure(),
            _ => {}
        }
    }

    /// Channel `c`'s bass waterfall, power-averaged over its mic positions.
    #[must_use]
    pub fn waterfall(&self, c: usize) -> Option<crate::dsp::decay::Waterfall> {
        let all: Vec<&crate::dsp::decay::Waterfall> = self.measure.positions[c]
            .iter()
            .map(|a| &a.waterfall)
            .collect();
        crate::dsp::decay::Waterfall::average(&all)
    }

    /// The room modes heard from speaker `c`, merged over its mic positions.
    #[must_use]
    pub fn modes(&self, c: usize) -> Vec<crate::dsp::modes::Mode> {
        let all: Vec<&[crate::dsp::modes::Mode]> = self.measure.positions[c]
            .iter()
            .map(|a| a.modes.as_slice())
            .collect();
        crate::dsp::modes::merge(&all)
    }

    /// The room's typical reverberation time for speaker `c`, s: the median of its
    /// per-octave T60s (125 Hz–4 kHz) over the mic positions.
    #[must_use]
    pub fn typical_reverb_s(&self, c: usize) -> Option<f64> {
        let mut all: Vec<f64> = self.measure.positions[c]
            .iter()
            .flat_map(|a| a.decay.iter().map(|d| d.1))
            .collect();
        all.sort_by(f64::total_cmp);
        all.get(all.len() / 2).copied()
    }

    /// True while work runs in the background (a design, the room guess); the screen keeps
    /// answering keys meanwhile.
    #[must_use]
    pub fn working(&self) -> bool {
        self.designing() || self.room_job.is_some()
    }

    /// The simulator's state, when simulating: tests plug the mic in, add outputs or move
    /// things through it.
    #[must_use]
    pub fn sim_state(&self) -> Option<Arc<Mutex<SimState>>> {
        match &self.env {
            Env::Sim { state, .. } => Some(Arc::clone(state)),
            Env::Real => None,
        }
    }

    /// The simulated wall distance, when simulating.
    #[must_use]
    pub fn sim_wall_m(&self) -> Option<f64> {
        match &self.env {
            Env::Sim { state, .. } => state.lock().expect("sim state lock poisoned").wall_m,
            Env::Real => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_session_honors_cal_file_override() {
        let dir = std::env::temp_dir().join(format!(
            "correctsound-review-cal-override-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("session dir");

        let cal_override = dir.join("override.txt");
        std::fs::write(
            &cal_override,
            "\"Sens Factor =-3.5dB, SERNO: 7009999\"\n20.0 0.0\n20000.0 0.0\n",
        )
        .expect("override cal");

        let sweep = crate::dsp::sweep::Sweep::new(crate::dsp::sweep::SweepParams::default());
        let mut rec = vec![0.0f32; 24_000];
        rec.extend_from_slice(&sweep.samples_f32());
        rec.resize(rec.len() + 72_000, 0.0);

        let session = Session::open(&dir);
        session.save_capture(Channel::Left, 0, &rec).expect("left");
        session
            .save_capture(Channel::Right, 0, &rec)
            .expect("right");
        let info = crate::session::SessionInfo {
            created: "2026-11-20T00:00:00+00:00".into(),
            mic_serial: None,
            calibration_file: Some(dir.join("missing.txt")),
            sens_factor: 0.0,
            spl_offset_db: 124.0,
            sweep_dbfs: sweep.params().peak_dbfs,
            sink: "sim-sink".into(),
            simulated: false,
            positions: [1, 1],
            ambient_spl: None,
            level_spl: Some(75.0),
            needed_spl: None,
            room_noise_spl: None,
            room: None,
        };
        session.save_info(&info).expect("info");

        let mut without_override = App::new(Options {
            simulate: true,
            ..Options::default()
        });
        assert!(without_override.open_session(&dir).is_err());

        let mut with_override = App::new(Options {
            simulate: true,
            cal_file: Some(cal_override.clone()),
            ..Options::default()
        });
        with_override
            .open_session(&dir)
            .expect("override calibration lets review open");
        assert_eq!(
            with_override.cal_path.as_deref(),
            Some(cal_override.as_path())
        );
        assert!((with_override.cal.as_ref().expect("cal").sens_factor + 3.5).abs() < 1e-9);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ctrl_and_alt_modified_shortcuts_are_ignored_except_ctrl_c() {
        let mut app = App::new(Options {
            simulate: true,
            ..Options::default()
        });
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT));
        assert!(!app.quit);
        assert!(app.overlay.is_none());

        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }
}
