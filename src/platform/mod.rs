//! The seam between CorrectSound and the operating system.
//!
//! Everything that depends on the OS, its audio stack or the program that applies the EQ
//! lives behind [`System`] (devices, volumes, the EQ host) and
//! [`AudioBackend`](crate::audio::AudioBackend) (playing and recording), in one submodule
//! per platform. The rest of the app (DSP, EQ design, the wizard, the UI) only sees the
//! neutral types in this file and must not know which OS it runs on.
//!
//! Adding a platform means adding a module next to [`linux`] and one line in [`native`];
//! see AGENTS.md. [`sim`] is the simulated platform the tests and `--simulate` use: it
//! builds and runs everywhere.

#[cfg(target_os = "linux")]
pub mod linux;
pub mod sim;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::audio::{AudioBackend, Route};
use crate::eq::EqSet;

/// Work for the system that may take a while (EasyEffects answers each command in about a
/// fifth of a second), so the app runs it beside the screen instead of on a key.
pub type Job = Box<dyn FnOnce() -> Result<()> + Send>;

/// An audio input or output device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The platform's identifier for it (a PipeWire object id, a CoreAudio device id, a
    /// Windows endpoint id). Only the platform module interprets it.
    pub id: String,
    /// A stable name, used to remember the choice (`--sink`, `--source`, config.toml).
    pub name: String,
    /// What to show the user.
    pub description: String,
    /// True when it has separate left and right channels.
    pub stereo: bool,
}

/// What the platform found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Devices {
    pub mic: Option<Device>,
    /// Outputs the speakers can be connected to (hardware only, no virtual devices).
    pub sinks: Vec<Device>,
    /// Name of the output to preselect: the one the EQ host plays to, else the default.
    pub preferred_sink: Option<String>,
    /// Other applications with output streams (they may be playing audio).
    pub other_streams: Vec<String>,
    /// The program that applies the EQ system-wide, when this platform has one.
    pub eq_host: Option<EqHost>,
}

/// The program that applies the EQ to everything the computer plays (EasyEffects on
/// Linux; for example Equalizer APO on Windows, CamillaDSP on macOS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqHost {
    /// Its name, for messages: "EasyEffects".
    pub name: String,
    pub running: bool,
}

/// One setting on the mic's way in that the calibration assumes is at unity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GainSetting {
    /// What it is, in the platform's words: "PipeWire input volume".
    pub label: String,
    /// Its current value, as text: "0.42 (muted)".
    pub value: String,
    pub at_unity: bool,
}

/// The mic's input gain, as a list of the settings that scale it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputGain {
    pub settings: Vec<GainSetting>,
}

impl InputGain {
    /// True when nothing scales the mic signal.
    #[must_use]
    pub fn is_unity(&self) -> bool {
        self.settings.iter().all(|s| s.at_unity)
    }

    /// The settings that are not at unity, for messages: "PipeWire input volume 0.42".
    #[must_use]
    pub fn off_unity(&self) -> String {
        self.settings
            .iter()
            .filter(|s| !s.at_unity)
            .map(|s| format!("{} {}", s.label, s.value))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Where a running sweep's playback actually goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routing {
    /// Not connected yet; ask again shortly.
    Pending,
    /// Going where it should.
    Ok,
    /// Going somewhere else: why, for the user.
    Wrong(String),
}

/// Outcome of loading the EQ into the EQ host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub preset_name: String,
    pub preset_path: PathBuf,
    /// Where the host's previous settings were backed up.
    pub backup_dir: PathBuf,
    /// The output the host now loads the preset for automatically, if it could be set.
    pub autoload: Option<String>,
}

/// Everything the wizard needs from the operating system, real or simulated.
///
/// Implementations register an undo step with [`crate::restore`] before every change they
/// make, so the system is put back when the app exits.
pub trait System: Send {
    /// What this is, for `correctsound devices`: "Linux (PipeWire, EasyEffects)".
    fn name(&self) -> &'static str;

    /// Programs or packages this platform needs that are missing, for the user.
    fn missing(&self) -> Vec<String>;

    /// Finds the mic, the outputs, other playing applications and the EQ host.
    ///
    /// # Errors
    ///
    /// Fails if the audio system cannot be queried.
    fn devices(&mut self) -> Result<Devices>;

    /// The input called `name` (for `--source`), if it exists.
    ///
    /// # Errors
    ///
    /// Fails if the audio system cannot be queried.
    fn find_input(&mut self, name: &str) -> Result<Option<Device>>;

    /// Reads the settings that scale the mic signal.
    ///
    /// # Errors
    ///
    /// Fails if they cannot be read.
    fn input_gain(&mut self, mic: &Device) -> Result<InputGain>;

    /// Sets the mic path to unity, registering undo steps first.
    ///
    /// # Errors
    ///
    /// Fails if a setting cannot be changed.
    fn set_input_unity(&mut self, mic: &Device) -> Result<()>;

    /// Where to look when the mic gain cannot be set: "check it with wpctl and alsamixer".
    fn input_gain_help(&self) -> &'static str;

    /// An output's volume, 1.0 being unity.
    ///
    /// # Errors
    ///
    /// Fails if it cannot be read.
    fn output_volume(&mut self, sink: &Device) -> Result<f64>;

    /// Sets an output's volume, registering an undo step first.
    ///
    /// # Errors
    ///
    /// Fails if it cannot be set.
    fn set_output_volume(&mut self, sink: &Device, volume: f64) -> Result<()>;

    /// Whether the EQ host is bypassed; `None` when there is none or it is not running.
    ///
    /// # Errors
    ///
    /// Fails if the host cannot be asked.
    fn eq_bypass(&mut self) -> Result<Option<bool>>;

    /// Bypasses the EQ host or turns it back on, registering the undo step first. An EQ host
    /// may be slow to answer, so this is a job the app runs beside the screen; it fails if
    /// the host cannot be told.
    fn bypass_eq(&mut self, bypass: bool) -> Job;

    /// The audio backend that plays to `sink` and records `mic`.
    fn audio(&mut self, mic: &Device, sink: &Device) -> Box<dyn AudioBackend>;

    /// Where the sweep that is playing now is going, for a sweep sent along `route` to
    /// `sink`. Platforms where playback cannot be rerouted answer [`Routing::Ok`].
    ///
    /// # Errors
    ///
    /// Fails if the audio system cannot be queried.
    fn check_routing(&mut self, route: Route, sink: &Device) -> Result<Routing>;

    /// Backs up the EQ host's settings into `backup_parent`, installs the EQ, loads it,
    /// and makes the host load it automatically for `sink` where it can.
    ///
    /// # Errors
    ///
    /// Fails if there is no EQ host, or the backup or the EQ cannot be written or loaded.
    fn apply_eq(&mut self, eq: &EqSet, backup_parent: &Path, sink: &Device) -> Result<Applied>;

    /// Rolls the EQ host back to how it was before the first [`Self::apply_eq`] since the
    /// app started (or since the last roll-back), which returned `applied`: the settings
    /// it had loaded, and what it loaded automatically for the output. The installed EQ
    /// stays available in the host, to load again. Returns what was done, for the user.
    ///
    /// # Errors
    ///
    /// Fails if nothing was applied, or the host cannot be put back.
    fn undo_eq(&mut self, applied: &Applied) -> Result<String>;
}

/// The real system on the platform the app was built for.
#[must_use]
pub fn native() -> Box<dyn System> {
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::Linux::default())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(Unsupported)
    }
}

/// Stands in for the system where no platform module exists yet: every query fails with
/// a message saying so, and the simulator still works.
#[derive(Debug, Default)]
pub struct Unsupported;

fn unsupported<T>() -> Result<T> {
    bail!(
        "CorrectSound does not support {} yet. `correctsound --simulate` works here; \
         AGENTS.md in the repository explains how to add it",
        std::env::consts::OS
    )
}

impl System for Unsupported {
    fn name(&self) -> &'static str {
        "unsupported platform"
    }

    fn missing(&self) -> Vec<String> {
        vec![format!(
            "support for measuring on {} (correctsound --simulate works)",
            std::env::consts::OS
        )]
    }

    fn devices(&mut self) -> Result<Devices> {
        unsupported()
    }

    fn find_input(&mut self, _name: &str) -> Result<Option<Device>> {
        unsupported()
    }

    fn input_gain(&mut self, _mic: &Device) -> Result<InputGain> {
        unsupported()
    }

    fn set_input_unity(&mut self, _mic: &Device) -> Result<()> {
        unsupported()
    }

    fn input_gain_help(&self) -> &'static str {
        "check it in the system's sound settings"
    }

    fn output_volume(&mut self, _sink: &Device) -> Result<f64> {
        unsupported()
    }

    fn set_output_volume(&mut self, _sink: &Device, _volume: f64) -> Result<()> {
        unsupported()
    }

    fn eq_bypass(&mut self) -> Result<Option<bool>> {
        Ok(None)
    }

    fn bypass_eq(&mut self, _bypass: bool) -> Job {
        Box::new(unsupported)
    }

    fn audio(&mut self, _mic: &Device, _sink: &Device) -> Box<dyn AudioBackend> {
        Box::new(Unsupported)
    }

    fn check_routing(&mut self, _route: Route, _sink: &Device) -> Result<Routing> {
        unsupported()
    }

    fn apply_eq(&mut self, _eq: &EqSet, _backup_parent: &Path, _sink: &Device) -> Result<Applied> {
        unsupported()
    }

    fn undo_eq(&mut self, _applied: &Applied) -> Result<String> {
        unsupported()
    }
}

impl AudioBackend for Unsupported {
    fn start(
        &mut self,
        _source: crate::audio::Source,
        _route: Route,
    ) -> Result<crate::audio::Stream> {
        unsupported()
    }
}
