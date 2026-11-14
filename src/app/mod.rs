//! The wizard: a linear state machine driven by key presses and periodic ticks.
//!
//! Nothing here touches the terminal; [`crate::ui`] renders an [`App`].

pub mod level;
pub mod measure;
pub mod placement;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub use level::{LevelPhase, LevelState};
pub use measure::{MeasureState, Purpose};
pub use placement::PlacementState;

use crate::analysis::Responses;
use crate::audio::sim::{SimConfig, SimState};
use crate::audio::{AudioBackend, Channel};
use crate::cal::{self, Calibration, Orientation, Serial};
use crate::config::Config;
use crate::dsp::level::SplScale;
use crate::eq::export;
use crate::eq::fit::{Design, design_with_positions};
use crate::eq::target::{DesignOptions, HIGH_LIMIT_PRESETS};
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
}

/// Wizard steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Preflight,
    Calibration,
    MicGain,
    Level,
    Measure,
    Result,
    Save,
}

impl Step {
    pub const ALL: [Self; 7] = [
        Self::Preflight,
        Self::Calibration,
        Self::MicGain,
        Self::Level,
        Self::Measure,
        Self::Result,
        Self::Save,
    ];

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Preflight => "Devices",
            Self::Calibration => "Calibration",
            Self::MicGain => "Mic gain",
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
    Idle,
    Downloading,
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
    pub missing_tools: Vec<String>,

    pub devices: Option<Devices>,
    pub sink_index: usize,
    last_poll: Option<Instant>,

    pub serial_input: String,
    pub orientation: Orientation,
    pub cal: Option<Calibration>,
    pub cal_path: Option<PathBuf>,
    pub cal_status: CalStatus,
    cal_rx: Option<Receiver<Result<(Calibration, PathBuf)>>>,

    pub mic_gain: Option<InputGain>,

    pub level: LevelState,
    pub measure: MeasureState,
    /// The live placement loop, while its screen is open (on the measure step).
    pub placement: Option<PlacementState>,

    pub design_options: DesignOptions,
    pub design: Option<Design>,
    pub view_channel: usize,

    pub save: SaveState,
    session: Option<Session>,
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
            let state = Arc::new(Mutex::new(SimState::default()));
            let config = SimConfig {
                speed: 4.0,
                ..SimConfig::default()
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
        let mut app = Self {
            step: Step::Preflight,
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
            missing_tools,
            devices: None,
            sink_index: 0,
            last_poll: None,
            serial_input,
            orientation,
            cal: None,
            cal_path: None,
            cal_status: CalStatus::Idle,
            cal_rx: None,
            mic_gain: None,
            level: LevelState::new(),
            measure: MeasureState::new(),
            placement: None,
            design_options: DesignOptions::default(),
            design: None,
            view_channel: 0,
            save: SaveState::default(),
            session: None,
        };
        if app.options.simulate
            && let Env::Sim { config, .. } = &app.env
        {
            app.cal = Some(config.calibration());
            app.serial_input = "7000000".into();
        }
        app.poll_devices();
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
        let cal = match &info.calibration_file {
            Some(path) => crate::cal::load(path)?,
            None if info.simulated => SimConfig::default().calibration(),
            None => return Err(anyhow!("the session names no calibration file")),
        };
        let scale = SplScale {
            offset_db: info.spl_offset_db,
        };
        let sweep = crate::dsp::sweep::Sweep::new(crate::dsp::sweep::SweepParams::default());
        for c in Channel::BOTH {
            let mut runs = Vec::new();
            for p in 0..info.positions[c.index()] {
                let rec = crate::session::read_wav(&session.capture_path(c, p))?;
                let analysis = crate::dsp::sweep::SweepAnalysis::analyze(
                    &sweep,
                    &rec,
                    crate::dsp::sweep::IrWindow::default(),
                )
                .map_err(|e| anyhow!("{} position {}: {e}", c.name(), p + 1))?;
                runs.push(analysis);
            }
            self.measure.responses[c.index()] = Some(crate::analysis::responses(
                &runs,
                &cal,
                scale,
                info.sweep_dbfs,
            ));
            self.measure.positions[c.index()] = runs;
        }
        self.cal_path.clone_from(&info.calibration_file);
        self.cal = Some(cal);
        self.level.ambient_spl = info.ambient_spl;
        // The output it was measured on, for applying.
        self.options.sink = Some(info.sink.clone());
        self.poll_devices();
        self.session = Some(session);
        self.step = Step::Result;
        self.redesign();
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
            Step::MicGain => self.read_mic_gain(),
            Step::Level => {
                let volume = self.current_sink_volume();
                self.level.enter(volume);
                if let Some(audio) = self.audio.as_mut()
                    && let Err(e) = self.level.start_ambient(audio.as_mut())
                {
                    self.set_error(&e);
                }
            }
            Step::Result => self.redesign(),
            Step::Save => self.write_outputs(),
            _ => {}
        }
    }

    fn current_sink_volume(&mut self) -> Option<f64> {
        let sink = self.sink()?.clone();
        self.system.output_volume(&sink).ok()
    }

    // ----- events -----

    /// Handles one key press.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.overlay.is_some() {
            self.key_overlay(key.code);
            return;
        }
        if self.placement.is_some() && self.key_placement(key.code) {
            return;
        }
        let typing = self.step == Step::Calibration && self.cal_status != CalStatus::Downloading;
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
            KeyCode::Char('w') if !typing => {
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
            KeyCode::Char('q') if !typing => {
                self.quit = true;
                return;
            }
            KeyCode::Esc if self.reviewing() && self.step == Step::Result => {
                self.message =
                    Some("Reviewing a saved session: nothing to measure. q quits.".into());
                return;
            }
            KeyCode::Esc => {
                if self.step != Step::Preflight {
                    let prev = self.step.prev();
                    self.go(prev);
                }
                return;
            }
            _ => {}
        }
        match self.step {
            Step::Preflight => self.key_preflight(key.code),
            Step::Calibration => self.key_calibration(key.code),
            Step::MicGain => self.key_mic_gain(key.code),
            Step::Level => self.key_level(key.code),
            Step::Measure => self.key_measure(key.code),
            Step::Result => self.key_result(key.code),
            Step::Save => self.key_save(key.code),
        }
    }

    fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
        self.overlay_scroll = 0;
    }

    /// Keys while an overlay is shown: scrolling, `m` for the technical version, and the
    /// overlay's own key, Esc or q to close it.
    fn key_overlay(&mut self, code: KeyCode) {
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
        match self.step {
            Step::Preflight => {
                let due = self
                    .last_poll
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(1));
                if due && self.mic().is_none() {
                    self.poll_devices();
                }
            }
            Step::Calibration => self.poll_download(),
            Step::Level => {
                let scale = self.spl_scale();
                if self.level.tick(scale) {
                    self.level_ambient_done();
                }
            }
            Step::Measure if self.placement.is_some() => self.tick_placement(),
            Step::Measure | Step::Save => self.tick_measure(),
            _ => {}
        }
    }

    // ----- 1. preflight -----

    fn poll_devices(&mut self) {
        self.last_poll = Some(Instant::now());
        match self.system.devices() {
            Ok(mut d) => {
                if let Some(name) = self.options.source.clone() {
                    d.mic = match self.system.find_input(&name) {
                        Ok(found) => found,
                        Err(e) => {
                            self.set_error(&e);
                            None
                        }
                    };
                }
                let wanted = self
                    .options
                    .sink
                    .clone()
                    .or_else(|| self.config.sink.clone().filter(|_| self.devices.is_none()))
                    .or_else(|| d.preferred_sink.clone());
                if let Some(w) = wanted
                    && let Some(i) = d.sinks.iter().position(|s| s.name == w)
                {
                    self.sink_index = i;
                }
                if self.sink_index >= d.sinks.len() {
                    self.sink_index = 0;
                }
                self.devices = Some(d);
            }
            // Reviewing needs devices only for applying; where there are none (a platform
            // without support), the review itself still works.
            Err(_) if self.reviewing() => {}
            Err(e) => self.set_error(&e),
        }
    }

    fn key_preflight(&mut self, code: KeyCode) {
        let n = self.devices.as_ref().map_or(0, |d| d.sinks.len());
        match code {
            KeyCode::Up if n > 0 => self.sink_index = (self.sink_index + n - 1) % n,
            KeyCode::Down if n > 0 => self.sink_index = (self.sink_index + 1) % n,
            KeyCode::Char('r') => self.poll_devices(),
            KeyCode::Enter => {
                if let Err(e) = self.confirm_devices() {
                    self.set_error(&e);
                }
            }
            _ => {}
        }
    }

    fn confirm_devices(&mut self) -> Result<()> {
        if !self.missing_tools.is_empty() {
            return Err(anyhow!("missing tools: {}", self.missing_tools.join(", ")));
        }
        let mic = self
            .mic()
            .cloned()
            .ok_or_else(|| anyhow!("plug in the UMIK-1 first"))?;
        let sink = self
            .sink()
            .cloned()
            .ok_or_else(|| anyhow!("no output device found"))?;
        if !sink.stereo {
            return Err(anyhow!("{} has no left/right channels", sink.description));
        }
        if self.eq_host_running() {
            self.system.set_eq_bypass(true)?;
        }
        self.audio = Some(self.system.audio(&mic, &sink));
        if !self.simulated() {
            self.config.sink = Some(sink.name.clone());
            self.config.save();
        }
        let next = if self.options.cal_file.is_some() || self.simulated() {
            if let Some(path) = self.options.cal_file.clone() {
                self.cal = Some(cal::load(&path)?);
                self.cal_path = Some(path);
            }
            Step::MicGain
        } else {
            Step::Calibration
        };
        self.go(next);
        Ok(())
    }

    // ----- 2. calibration -----

    fn key_calibration(&mut self, code: KeyCode) {
        if self.cal_status == CalStatus::Downloading {
            return;
        }
        match code {
            KeyCode::Char(c) if c.is_ascii_digit() || c == '-' => {
                if self.serial_input.len() < 9 {
                    self.serial_input.push(c);
                }
                self.cal = None;
            }
            KeyCode::Backspace => {
                self.serial_input.pop();
                self.cal = None;
            }
            KeyCode::Tab | KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                self.orientation = match self.orientation {
                    Orientation::OnAxis => Orientation::Vertical,
                    Orientation::Vertical => Orientation::OnAxis,
                };
                self.cal = None;
            }
            KeyCode::Enter => {
                if self.cal.is_some() {
                    self.go(Step::MicGain);
                } else {
                    self.start_download();
                }
            }
            _ => {}
        }
    }

    fn start_download(&mut self) {
        let serial: Serial = match self.serial_input.parse() {
            Ok(s) => s,
            Err(e) => {
                self.cal_status = CalStatus::Failed(e.to_string());
                return;
            }
        };
        let orientation = self.orientation;
        let (tx, rx) = mpsc::channel();
        self.config.serial = Some(serial.clone());
        self.config.orientation = orientation;
        self.config.save();
        thread::spawn(move || {
            let _ = tx.send(cal::fetch(&serial, orientation, &cal::cache_dir()));
        });
        self.cal_rx = Some(rx);
        self.cal_status = CalStatus::Downloading;
    }

    fn poll_download(&mut self) {
        let Some(rx) = &self.cal_rx else { return };
        match rx.try_recv() {
            Ok(Ok((c, path))) => {
                self.cal = Some(c);
                self.cal_path = Some(path);
                self.cal_status = CalStatus::Idle;
                self.cal_rx = None;
            }
            Ok(Err(e)) => {
                self.cal_status = CalStatus::Failed(format!("{e:#}"));
                self.cal_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.cal_status = CalStatus::Failed("download stopped unexpectedly".into());
                self.cal_rx = None;
            }
        }
    }

    // ----- 3. mic gain -----

    fn read_mic_gain(&mut self) {
        let Some(mic) = self.mic().cloned() else {
            return;
        };
        match self.system.input_gain(&mic) {
            Ok(g) => self.mic_gain = Some(g),
            Err(e) => self.set_error(&e),
        }
    }

    fn key_mic_gain(&mut self, code: KeyCode) {
        if code != KeyCode::Enter {
            return;
        }
        if self.mic_gain.as_ref().is_some_and(InputGain::is_unity) {
            self.go(Step::Level);
            return;
        }
        let Some(mic) = self.mic().cloned() else {
            return;
        };
        if let Err(e) = self.system.set_input_unity(&mic) {
            self.set_error(&e);
            return;
        }
        self.read_mic_gain();
        if self.mic_gain.as_ref().is_some_and(InputGain::is_unity) {
            self.message = Some("Mic gain set to unity.".into());
        } else {
            self.message = Some(format!(
                "The mic gain did not change; {}",
                self.system.input_gain_help()
            ));
        }
    }

    // ----- 4. level -----

    fn key_level(&mut self, code: KeyCode) {
        match code {
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                let ch = self.level.channel.other();
                self.start_noise(ch);
            }
            KeyCode::Char(' ') => {
                let ch = self.level.channel;
                self.start_noise(ch);
            }
            KeyCode::Up | KeyCode::Down => {
                let step = if code == KeyCode::Up { 0.02 } else { -0.02 };
                let Some(sink) = self.sink().cloned() else {
                    return;
                };
                let current = self.system.output_volume(&sink).unwrap_or(1.0);
                let new = (current + step).clamp(0.0, 1.0);
                match self.system.set_output_volume(&sink, new) {
                    Ok(()) => self.level.sink_volume = Some(new),
                    Err(e) => self.set_error(&e),
                }
            }
            KeyCode::Enter => {
                if self.level.heard.iter().all(Option::is_none) {
                    self.message = Some("Let the noise play and set the level first.".into());
                    return;
                }
                self.measure.frozen_volume = self.current_sink_volume();
                self.go(Step::Measure);
            }
            _ => {}
        }
    }

    fn start_noise(&mut self, channel: Channel) {
        let Some(audio) = self.audio.as_mut() else {
            return;
        };
        if let Err(e) = self.level.start_noise(audio.as_mut(), channel) {
            self.set_error(&e);
        }
    }

    /// Called by the level state when ambient measurement completes.
    fn level_ambient_done(&mut self) {
        let ch = self.level.channel;
        self.start_noise(ch);
    }

    // ----- 5. measure -----

    fn key_measure(&mut self, code: KeyCode) {
        if self.measure.running() {
            if code == KeyCode::Char('s') {
                self.measure.cancel();
                self.message = Some("Measurement stopped.".into());
            }
            return;
        }
        let have_both = self.measure.positions.iter().all(|p| !p.is_empty());
        match code {
            KeyCode::Char('p') => self.open_placement(),
            KeyCode::Enter if have_both => self.go(Step::Result),
            KeyCode::Enter | KeyCode::Char('m') => self.start_sweeps(Purpose::Measure),
            KeyCode::Char('a') if have_both => self.start_sweeps(Purpose::Measure),
            KeyCode::Char('x') if have_both => {
                self.measure.positions.iter_mut().for_each(Vec::clear);
                self.measure.responses = [None, None];
                self.message = Some("Measurements cleared.".into());
            }
            _ => {}
        }
    }

    // ----- 5b. placement -----

    fn open_placement(&mut self) {
        if let Err(e) = self.pre_sweep_checks(Purpose::Measure) {
            self.measure.issue = Some(format!("{e:#}"));
            return;
        }
        self.measure.issue = None;
        let mut p = PlacementState::new(Channel::Left, self.measure.sweep_dbfs());
        if let Env::Sim { state, .. } = &self.env {
            let mut st = state.lock().expect("sim state lock poisoned");
            st.wall_m.get_or_insert(SIM_WALL_START_M);
        }
        if let Some(audio) = self.audio.as_mut()
            && let Err(e) = p.start(audio.as_mut())
        {
            p.issue = Some(format!("{e:#}"));
        }
        self.placement = Some(p);
    }

    /// Keys on the placement screen; false lets the key through to the global ones.
    fn key_placement(&mut self, code: KeyCode) -> bool {
        let Some(p) = self.placement.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Esc | KeyCode::Char('p') => {
                self.placement = None;
                return true;
            }
            KeyCode::Char(' ') => p.pin(),
            KeyCode::Tab => p.channel = p.channel.other(),
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
                let mut st = state.lock().expect("sim state lock poisoned");
                let d = (st.wall_m.unwrap_or(SIM_WALL_START_M) + step).clamp(0.1, 2.0);
                st.wall_m = Some(d);
                self.message = Some(format!(
                    "Simulated: the speakers are now {:.0} cm from the wall behind them.",
                    d * 100.0
                ));
            }
            _ => return false,
        }
        if let Some(audio) = self.audio.as_mut()
            && let Some(p) = self.placement.as_mut()
            && let Err(e) = p.start(audio.as_mut())
        {
            p.issue = Some(format!("{e:#}"));
        }
        true
    }

    fn tick_placement(&mut self) {
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
            p.issue = Some(format!("{e:#}"));
            p.paused = true;
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

    /// The simulated wall distance, when simulating.
    #[must_use]
    pub fn sim_wall_m(&self) -> Option<f64> {
        match &self.env {
            Env::Sim { state, .. } => state.lock().expect("sim state lock poisoned").wall_m,
            Env::Real => None,
        }
    }

    fn start_sweeps(&mut self, purpose: Purpose) {
        if let Err(e) = self.pre_sweep_checks(purpose) {
            self.measure.issue = Some(format!("{e:#}"));
            return;
        }
        self.measure.issue = None;
        self.measure.queue = vec![Channel::Left, Channel::Right];
        self.measure.purpose = purpose;
        self.next_sweep();
    }

    fn pre_sweep_checks(&mut self, purpose: Purpose) -> Result<()> {
        let mic = self
            .mic()
            .cloned()
            .ok_or_else(|| anyhow!("the mic disappeared"))?;
        let gain = self.system.input_gain(&mic)?;
        if !gain.is_unity() {
            let now = gain.off_unity();
            self.mic_gain = Some(gain);
            return Err(anyhow!(
                "the mic gain changed since step 3 (now {now}); press Esc and fix it"
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

    fn next_sweep(&mut self) {
        let Some(audio) = self.audio.as_mut() else {
            return;
        };
        if let Err(e) = self.measure.start_next(audio.as_mut()) {
            self.measure.issue = Some(format!("{e:#}"));
            self.measure.queue.clear();
        }
    }

    fn tick_measure(&mut self) {
        let sink = self.sink().cloned();
        match self.measure.tick(self.system.as_mut(), sink.as_ref()) {
            measure::Tick::Idle | measure::Tick::Running => {}
            measure::Tick::Failed => {}
            measure::Tick::Captured(channel, recording) => self.sweep_done(channel, &recording),
        }
    }

    fn sweep_done(&mut self, channel: Channel, recording: &[f32]) {
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

    fn save_session_info(&mut self) {
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
        };
        let result = self.session().and_then(|s| s.save_info(&info));
        if let Err(e) = result {
            self.set_error(&e);
        }
    }

    // ----- 6. result -----

    fn redesign(&mut self) {
        let [Some(l), Some(r)] = &self.measure.responses else {
            return;
        };
        let design = design_with_positions(
            [&l.fit, &r.fit],
            [&l.positions, &r.positions],
            &self.design_options,
        );
        self.curiosities = crate::insights::insights(&crate::insights::Inputs {
            responses: [l, r],
            analyses: [&self.measure.positions[0], &self.measure.positions[1]],
            design: &design,
            ambient_spl: self.level.ambient_spl,
        });
        self.design = Some(design);
    }

    fn key_result(&mut self, code: KeyCode) {
        let o = &mut self.design_options;
        match code {
            KeyCode::Tab => {
                self.view_channel = 1 - self.view_channel;
                return;
            }
            KeyCode::Char('t') => o.tilt_db_per_oct = (o.tilt_db_per_oct - 0.25).max(-1.5),
            KeyCode::Char('T') => o.tilt_db_per_oct = (o.tilt_db_per_oct + 0.25).min(0.0),
            KeyCode::Char('h') => {
                let i = HIGH_LIMIT_PRESETS
                    .iter()
                    .position(|&h| h == o.high_limit_hz)
                    .unwrap_or(0);
                o.high_limit_hz = HIGH_LIMIT_PRESETS[(i + 1) % HIGH_LIMIT_PRESETS.len()];
            }
            // From automatic, f/F step from the count it chose (the larger of the two).
            KeyCode::Char('f' | 'F') => {
                let auto = self.design.as_ref().map_or(10, |d| {
                    d.channels
                        .iter()
                        .map(|c| c.filters.len())
                        .max()
                        .unwrap_or(10)
                });
                let current = o.max_filters.unwrap_or(auto);
                o.max_filters = Some(if code == KeyCode::Char('f') {
                    (current + 1).min(20)
                } else {
                    current.saturating_sub(1).max(1)
                });
            }
            KeyCode::Char('d') => *o = DesignOptions::default(),
            KeyCode::Enter => {
                self.go(Step::Save);
                return;
            }
            _ => return,
        }
        self.redesign();
    }

    // ----- 7. save -----

    fn write_outputs(&mut self) {
        self.save = SaveState::default();
        let Some(design) = self.design.clone() else {
            return;
        };
        let header = format!(
            "CorrectSound report, {}\nOutput: {}\nMic: {}{}",
            chrono::Local::now().format("%Y-%m-%d %H:%M"),
            self.sink()
                .map(|s| s.description.clone())
                .unwrap_or_default(),
            self.cal
                .as_ref()
                .and_then(|c| c.serial.clone())
                .unwrap_or_else(|| "unknown".into()),
            if self.simulated() { " (simulated)" } else { "" }
        );
        let files = [
            ("left.txt", export::apo_text(&design.eq, 0)),
            ("right.txt", export::apo_text(&design.eq, 1)),
            (
                "CorrectSound.json",
                serde_json::to_string_pretty(&export::easyeffects_preset(&design.eq))
                    .unwrap_or_default(),
            ),
            (
                "report.txt",
                format!(
                    "{}\n{}",
                    export::report(&design, &header),
                    crate::insights::text(&self.curiosities)
                ),
            ),
        ];
        let responses = self.measure.responses.clone();
        let result = (|| -> Result<Vec<PathBuf>> {
            let session = self.session()?;
            let mut written = Vec::new();
            for (name, text) in &files {
                session.write(name, text)?;
                written.push(session.dir().join(name));
            }
            for (c, r) in responses.iter().enumerate() {
                if let Some(r) = r {
                    let name = if c == 0 {
                        "left-response"
                    } else {
                        "right-response"
                    };
                    session.save_response(name, &r.display)?;
                }
            }
            for (c, ch) in design.channels.iter().enumerate() {
                let name = Channel::BOTH[c].name().to_lowercase();
                session.save_response(&format!("{name}-predicted"), &ch.predicted)?;
            }
            Ok(written)
        })();
        match result {
            Ok(w) => self.save.written = w,
            Err(e) => self.save.error = Some(format!("{e:#}")),
        }
    }

    /// True when the apply step is available.
    #[must_use]
    pub fn can_apply(&self) -> bool {
        self.eq_host_running() && self.design.is_some()
    }

    /// True when the platform's EQ host (EasyEffects on Linux) is running.
    #[must_use]
    pub fn eq_host_running(&self) -> bool {
        self.devices
            .as_ref()
            .and_then(|d| d.eq_host.as_ref())
            .is_some_and(|h| h.running)
    }

    /// The EQ host's name, for messages: "EasyEffects".
    #[must_use]
    pub fn eq_host_name(&self) -> &str {
        self.devices
            .as_ref()
            .and_then(|d| d.eq_host.as_ref())
            .map_or("the EQ host", |h| h.name.as_str())
    }

    fn key_save(&mut self, code: KeyCode) {
        if self.measure.running() {
            if code == KeyCode::Char('s') {
                self.measure.cancel();
            }
            return;
        }
        match code {
            KeyCode::Char('a') if self.can_apply() && self.save.applied.is_none() => self.apply(),
            KeyCode::Char('v') if self.reviewing() => {
                self.message = Some(
                    "Verifying plays sweeps and records them; start a new run (correctsound) \
                     for that."
                        .into(),
                );
            }
            KeyCode::Char('v') if self.save.applied.is_some() => self.start_sweeps(Purpose::Verify),
            KeyCode::Char('v') => {
                let host = self.eq_host_name();
                self.message = Some(if self.can_apply() {
                    format!(
                        "Verify measures again through {host} with the new EQ in it, so it comes \
                         after applying: press a first."
                    )
                } else {
                    format!("Verify measures through {host}, which is not running.")
                });
            }
            KeyCode::Enter => self.quit = true,
            _ => {}
        }
    }

    fn apply(&mut self) {
        let Some(design) = &self.design else { return };
        let eq = design.eq.clone();
        let Some(sink) = self.sink().cloned() else {
            self.save.error = Some("no output device selected".into());
            return;
        };
        let parent = match self.session() {
            Ok(s) => s.dir().to_path_buf(),
            Err(e) => {
                self.set_error(&e);
                return;
            }
        };
        match self.system.apply_eq(&eq, &parent, &sink) {
            Ok(applied) => self.save.applied = Some(applied),
            Err(e) => self.save.error = Some(format!("{e:#}")),
        }
    }

    /// Compares the verification sweep with the prediction. The broadband offset is
    /// removed first (the EQ host's output has its own volume); what remains is shape.
    fn finish_verify(&mut self) {
        if let Some(session) = &self.session {
            for (c, v) in self.measure.verify.iter().enumerate() {
                if let Some(v) = v {
                    let name = Channel::BOTH[c].name().to_lowercase();
                    // Same smoothing as `<channel>-predicted.csv`, for comparison.
                    if let Err(e) = session.save_response(&format!("verify-{name}"), &v.fit) {
                        self.save.error = Some(format!("{e:#}"));
                    }
                }
            }
        }
        let Some(design) = &self.design else { return };
        let mut results = [VerifyResult::default(); 2];
        for (c, result) in results.iter_mut().enumerate() {
            let Some(v) = &self.measure.verify[c] else {
                continue;
            };
            let ch = &design.channels[c];
            let diffs: Vec<(f64, f64)> = ch
                .predicted
                .freqs
                .iter()
                .zip(&ch.predicted.db)
                .zip(&v.fit.db)
                .filter(|((f, _), _)| ch.target.in_range(**f))
                .map(|((&f, p), m)| (f, m - p))
                .collect();
            if diffs.is_empty() {
                continue;
            }
            let mean = diffs.iter().map(|d| d.1).sum::<f64>() / diffs.len() as f64;
            let sq = diffs.iter().map(|d| (d.1 - mean).powi(2)).sum::<f64>();
            let (worst_hz, worst) = diffs
                .iter()
                .copied()
                .max_by(|a, b| (a.1 - mean).abs().total_cmp(&(b.1 - mean).abs()))
                .expect("not empty");
            *result = VerifyResult {
                rms_db: (sq / diffs.len() as f64).sqrt(),
                worst_db: worst - mean,
                worst_hz,
            };
        }
        self.save.verify = Some(results);
    }

    /// Verification responses, if measured.
    #[must_use]
    pub fn verify_responses(&self) -> &[Option<Responses>; 2] {
        &self.measure.verify
    }
}
