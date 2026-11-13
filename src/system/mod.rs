//! System discovery and control: PipeWire nodes, volumes, ALSA mixer and EasyEffects.

pub mod alsa;
pub mod easyeffects;
pub mod pw;
pub mod sim;

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::eq::EqSet;

/// A PipeWire node of interest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: u32,
    pub name: String,
    pub description: String,
    /// `audio.position`, e.g. `["FL", "FR"]`.
    pub positions: Vec<String>,
    /// ALSA card number, for hardware nodes.
    pub alsa_card: Option<String>,
}

impl Node {
    #[must_use]
    pub fn is_stereo_capable(&self) -> bool {
        let has = |p: &str| self.positions.iter().any(|x| x == p);
        has("FL") && has("FR")
    }
}

/// What preflight found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Devices {
    pub mic: Option<Node>,
    /// Hardware sinks only (EasyEffects and other virtual sinks are excluded).
    pub sinks: Vec<Node>,
    /// Name of the sink EasyEffects outputs to, or else the default sink.
    pub preferred_sink: Option<String>,
    /// Other applications with output streams (they may be playing audio).
    pub other_streams: Vec<String>,
    pub easyeffects_running: bool,
}

/// Capture gain state of the mic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MicGain {
    /// PipeWire source volume (wpctl scale).
    pub pw_volume: f64,
    pub pw_muted: bool,
    /// ALSA capture control, when present.
    pub alsa: Option<alsa::CaptureControl>,
}

impl MicGain {
    /// True when the mic path is at unity: PipeWire 1.0 unmuted, ALSA at 0 dB and on.
    #[must_use]
    pub fn is_unity(&self) -> bool {
        (self.pw_volume - 1.0).abs() < 1e-3
            && !self.pw_muted
            && self.alsa.is_none_or(|a| a.value == a.max && a.switch_on)
    }
}

/// Outcome of installing the EasyEffects preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub preset_name: String,
    pub preset_path: PathBuf,
    pub backup_dir: PathBuf,
    /// The output route EasyEffects now autoloads the preset on, if it could be set.
    pub autoload: Option<String>,
}

/// Everything the wizard needs from the system, real or simulated.
pub trait System: Send {
    /// Enumerates the mic, sinks, other streams and EasyEffects.
    ///
    /// # Errors
    ///
    /// Fails if the PipeWire tools cannot be run.
    fn devices(&mut self) -> Result<Devices>;

    /// Reads the mic's capture gain.
    ///
    /// # Errors
    ///
    /// Fails if the volume tools fail.
    fn mic_gain(&mut self, mic: &Node) -> Result<MicGain>;

    /// Sets the mic path to unity, registering undo steps first.
    ///
    /// # Errors
    ///
    /// Fails if the volume tools fail.
    fn set_mic_unity(&mut self, mic: &Node) -> Result<()>;

    /// Reads a sink's volume (wpctl scale).
    ///
    /// # Errors
    ///
    /// Fails if wpctl fails.
    fn sink_volume(&mut self, sink: &Node) -> Result<f64>;

    /// Sets a sink's volume, registering an undo step first.
    ///
    /// # Errors
    ///
    /// Fails if wpctl fails.
    fn set_sink_volume(&mut self, sink: &Node, volume: f64) -> Result<()>;

    /// EasyEffects global bypass; `None` when EasyEffects is not running.
    ///
    /// # Errors
    ///
    /// Fails if the EasyEffects CLI fails.
    fn easyeffects_bypass(&mut self) -> Result<Option<bool>>;

    /// Sets EasyEffects global bypass, registering the undo step first.
    ///
    /// # Errors
    ///
    /// Fails if the EasyEffects CLI fails.
    fn set_easyeffects_bypass(&mut self, bypass: bool) -> Result<()>;

    /// Names of the nodes our playback stream (`node_name`) is linked to.
    ///
    /// # Errors
    ///
    /// Fails if pw-dump fails.
    fn playback_links(&mut self, node_name: &str) -> Result<Vec<String>>;

    /// Backs up the EasyEffects settings into `backup_parent`, installs the preset, loads
    /// it, and makes EasyEffects autoload it for `sink`.
    ///
    /// # Errors
    ///
    /// Fails if the backup or the preset cannot be written, or loading fails.
    fn apply_preset(
        &mut self,
        eq: &EqSet,
        backup_parent: &std::path::Path,
        sink: &Node,
    ) -> Result<Applied>;
}

/// Runs a command and returns its stdout.
///
/// # Errors
///
/// Fails if the program is missing or exits unsuccessfully.
pub fn run(prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {prog} (is it installed?)"))?;
    if !out.status.success() {
        bail!(
            "{prog} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Checks the external tools exist; returns a message naming each missing package.
#[must_use]
pub fn missing_tools() -> Vec<String> {
    let tools = [
        ("pw-dump", "pipewire"),
        ("pw-cat", "pipewire"),
        ("wpctl", "wireplumber"),
        ("amixer", "alsa-utils"),
    ];
    tools
        .iter()
        .filter(|(t, _)| {
            Command::new("sh")
                .args(["-c", &format!("command -v {t}")])
                .stdout(Stdio::null())
                .status()
                .map_or(true, |s| !s.success())
        })
        .map(|(t, p)| format!("{t} (package: {p})"))
        .collect()
}

/// The real Linux system.
#[derive(Debug, Default)]
pub struct Linux;

impl System for Linux {
    fn devices(&mut self) -> Result<Devices> {
        let dump = pw::Dump::load()?;
        let mut devices = dump.devices();
        devices.preferred_sink = dump
            .easyeffects_output()
            .or_else(easyeffects::configured_output_device)
            .filter(|n| devices.sinks.iter().any(|s| &s.name == n))
            .or_else(|| {
                dump.default_sink()
                    .filter(|n| devices.sinks.iter().any(|s| &s.name == n))
            })
            .or_else(|| devices.sinks.first().map(|s| s.name.clone()));
        Ok(devices)
    }

    fn mic_gain(&mut self, mic: &Node) -> Result<MicGain> {
        let (pw_volume, pw_muted) = pw::volume(mic.id)?;
        let alsa = match &mic.alsa_card {
            Some(card) => alsa::capture_control(card)?,
            None => None,
        };
        Ok(MicGain {
            pw_volume,
            pw_muted,
            alsa,
        })
    }

    fn set_mic_unity(&mut self, mic: &Node) -> Result<()> {
        let gain = self.mic_gain(mic)?;
        let id = mic.id.to_string();
        let vol = format!("{:.4}", gain.pw_volume);
        crate::restore::register("mic-volume", &["wpctl", "set-volume", &id, &vol]);
        let mute = if gain.pw_muted { "1" } else { "0" };
        crate::restore::register("mic-mute", &["wpctl", "set-mute", &id, mute]);
        run("wpctl", &["set-volume", &id, "1.0"])?;
        run("wpctl", &["set-mute", &id, "0"])?;
        if let (Some(card), Some(ctl)) = (&mic.alsa_card, gain.alsa) {
            alsa::set_unity(card, &ctl)?;
        }
        Ok(())
    }

    fn sink_volume(&mut self, sink: &Node) -> Result<f64> {
        Ok(pw::volume(sink.id)?.0)
    }

    fn set_sink_volume(&mut self, sink: &Node, volume: f64) -> Result<()> {
        let id = sink.id.to_string();
        let current = pw::volume(sink.id)?.0;
        crate::restore::register(
            &format!("sink-volume-{id}"),
            &["wpctl", "set-volume", &id, &format!("{current:.4}")],
        );
        run(
            "wpctl",
            &["set-volume", &id, &format!("{:.4}", volume.clamp(0.0, 1.5))],
        )?;
        Ok(())
    }

    fn easyeffects_bypass(&mut self) -> Result<Option<bool>> {
        easyeffects::bypass()
    }

    fn set_easyeffects_bypass(&mut self, bypass: bool) -> Result<()> {
        if let Some(current) = easyeffects::bypass()? {
            let arg = if current { "1" } else { "2" };
            crate::restore::register("easyeffects-bypass", &["easyeffects", "-b", arg]);
        }
        easyeffects::set_bypass(bypass)
    }

    fn playback_links(&mut self, node_name: &str) -> Result<Vec<String>> {
        Ok(pw::Dump::load()?.links_from(node_name))
    }

    fn apply_preset(
        &mut self,
        eq: &EqSet,
        backup_parent: &std::path::Path,
        sink: &Node,
    ) -> Result<Applied> {
        let target = pw::Dump::load()
            .ok()
            .and_then(|d| d.route_description(&sink.name))
            .map(|route| easyeffects::AutoloadTarget {
                device: sink.name.clone(),
                description: sink.description.clone(),
                route,
            });
        easyeffects::apply(eq, backup_parent, target.as_ref())
    }
}
