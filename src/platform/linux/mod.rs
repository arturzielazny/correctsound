//! Linux: PipeWire for audio and devices (`pw-cat`, `pw-dump`, `wpctl`), ALSA for the
//! mic's capture control (`amixer`), and EasyEffects as the EQ host.

pub mod alsa;
pub mod easyeffects;
pub mod pipewire;
pub mod pw_cat;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};

use super::{Applied, Device, Devices, EqHost, GainSetting, InputGain, Job, Routing, System};
use crate::audio::{AudioBackend, Route};
use crate::eq::EqSet;

/// EasyEffects' virtual sink: where playback goes to pass through the EQ.
pub const EASYEFFECTS_SINK: &str = "easyeffects_sink";

/// Runs a command and returns its stdout.
///
/// # Errors
///
/// Fails if the program is missing or exits unsuccessfully.
pub fn run(prog: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(prog)
        .args(args)
        .env("LC_ALL", "C")
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

/// The real Linux system.
#[derive(Debug, Default)]
pub struct Linux {
    /// EasyEffects' bypass before the app first changed it (set by a job beside the screen).
    original_bypass: Arc<Mutex<Option<bool>>>,
    /// EasyEffects before the first apply, for a roll-back.
    before_apply: Option<easyeffects::Before>,
}

impl Linux {
    fn pipewire_volume(sink: &Device) -> Result<(f64, bool)> {
        pipewire::volume(&sink.id)
    }
}

impl System for Linux {
    fn name(&self) -> &'static str {
        "Linux (PipeWire, ALSA, EasyEffects)"
    }

    fn missing(&self) -> Vec<String> {
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

    fn devices(&mut self) -> Result<Devices> {
        let dump = pipewire::Dump::load()?;
        let mut devices = dump.devices();
        devices.eq_host = Some(EqHost {
            name: "EasyEffects".into(),
            running: dump.easyeffects_running(),
        });
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

    fn find_input(&mut self, name: &str) -> Result<Option<Device>> {
        Ok(pipewire::Dump::load()?.node_by_name(name))
    }

    fn input_gain(&mut self, mic: &Device) -> Result<InputGain> {
        let (volume, muted) = pipewire::volume(&mic.id)?;
        let mut settings = vec![GainSetting {
            label: "PipeWire input volume".into(),
            value: format!("{volume:.2}{}", if muted { " (muted)" } else { "" }),
            at_unity: (volume - 1.0).abs() < 1e-3 && !muted,
        }];
        let card = pipewire::Dump::load()
            .ok()
            .and_then(|d| d.alsa_card(&mic.name));
        let control = match &card {
            Some(card) => alsa::capture_control(card)?,
            None => None,
        };
        settings.push(match control {
            Some(a) => GainSetting {
                label: "ALSA capture control".into(),
                value: format!(
                    "{}/{}{}",
                    a.value,
                    a.max,
                    if a.switch_on { "" } else { " (switched off)" }
                ),
                at_unity: a.value == a.max && a.switch_on,
            },
            None => GainSetting {
                label: "ALSA capture control".into(),
                value: "none".into(),
                at_unity: true,
            },
        });
        Ok(InputGain { settings })
    }

    fn set_input_unity(&mut self, mic: &Device) -> Result<()> {
        let (volume, muted) = pipewire::volume(&mic.id)?;
        let id = mic.id.as_str();
        // Only what changes is registered, so the summary on exit lists only that.
        if (volume - 1.0).abs() >= 1e-3 {
            let vol = format!("{volume:.4}");
            let registered = crate::restore::register_command(
                "mic-volume",
                &format!("The mic's input volume in PipeWire is back to {volume:.2}."),
                &["wpctl", "set-volume", id, &vol],
            );
            if let Err(e) = run("wpctl", &["set-volume", id, "1.0"]) {
                if registered {
                    crate::restore::forget("mic-volume");
                }
                return Err(e);
            }
        }
        if muted {
            let registered = crate::restore::register_command(
                "mic-mute",
                "The mic is muted again, as it was.",
                &["wpctl", "set-mute", id, "1"],
            );
            if let Err(e) = run("wpctl", &["set-mute", id, "0"]) {
                if registered {
                    crate::restore::forget("mic-mute");
                }
                return Err(e);
            }
        }
        if let Some(card) = pipewire::Dump::load()?.alsa_card(&mic.name)
            && let Some(ctl) = alsa::capture_control(&card)?
        {
            alsa::set_unity(&card, &ctl)?;
        }
        Ok(())
    }

    fn input_gain_help(&self) -> &'static str {
        "check it with wpctl and alsamixer"
    }

    fn output_volume(&mut self, sink: &Device) -> Result<f64> {
        Ok(Self::pipewire_volume(sink)?.0)
    }

    fn set_output_volume(&mut self, sink: &Device, volume: f64) -> Result<()> {
        let id = sink.id.as_str();
        let current = Self::pipewire_volume(sink)?.0;
        let target = volume.clamp(0.0, 1.5);
        if (current - target).abs() < 1e-4 {
            return Ok(());
        }
        let key = format!("sink-volume-{id}");
        let registered = crate::restore::register_command(
            &key,
            &format!(
                "The volume of {} is back to {current:.2}.",
                sink.description
            ),
            &["wpctl", "set-volume", id, &format!("{current:.4}")],
        );
        if let Err(e) = run("wpctl", &["set-volume", id, &format!("{target:.4}")]) {
            if registered {
                crate::restore::forget(&key);
            }
            return Err(e);
        }
        Ok(())
    }

    fn eq_bypass(&mut self) -> Result<Option<bool>> {
        easyeffects::bypass()
    }

    fn bypass_eq(&mut self, bypass: bool) -> Job {
        let original = Arc::clone(&self.original_bypass);
        Box::new(move || {
            let mut registered = false;
            if let Some(current) = easyeffects::bypass()? {
                original
                    .lock()
                    .expect("bypass lock poisoned")
                    .get_or_insert(current);
                // Nothing to put back when it already was as asked.
                if current != bypass {
                    let (arg, what) = if current {
                        ("1", "EasyEffects is bypassed again, as it was.")
                    } else {
                        (
                            "2",
                            "EasyEffects is on again (it was bypassed while measuring, so the \
                             bare speakers were measured).",
                        )
                    };
                    registered = crate::restore::register_command(
                        "easyeffects-bypass",
                        what,
                        &["easyeffects", "-b", arg],
                    );
                }
            }
            if let Err(e) = easyeffects::set_bypass(bypass) {
                if registered {
                    crate::restore::forget("easyeffects-bypass");
                }
                return Err(e);
            }
            Ok(())
        })
    }

    fn audio(&mut self, mic: &Device, sink: &Device) -> Box<dyn AudioBackend> {
        Box::new(pw_cat::PwCat::new(&mic.name, &sink.name))
    }

    fn check_routing(&mut self, route: Route, sink: &Device) -> Result<Routing> {
        let expected = match route {
            Route::Hardware => sink.name.as_str(),
            Route::ThroughEq => EASYEFFECTS_SINK,
        };
        let links = pipewire::Dump::load()?.links_from(pw_cat::PLAYBACK_NODE);
        Ok(if links.is_empty() {
            Routing::Pending
        } else if links.iter().all(|l| l == expected) {
            Routing::Ok
        } else {
            Routing::Wrong(format!(
                "the sweep was routed to {} instead of {expected} (check EasyEffects' settings)",
                links.join(", ")
            ))
        })
    }

    fn apply_eq(&mut self, eq: &EqSet, backup_parent: &Path, sink: &Device) -> Result<Applied> {
        let target = pipewire::Dump::load()
            .ok()
            .and_then(|d| d.route_description(&sink.name))
            .map(|route| easyeffects::AutoloadTarget {
                device: sink.name.clone(),
                description: sink.description.clone(),
                route,
            });
        let original_bypass = *self.original_bypass.lock().expect("bypass lock poisoned");
        let before = match self.before_apply.take() {
            Some(before) => before,
            None => easyeffects::Before {
                preset: easyeffects::last_output_preset(),
                bypass: match original_bypass {
                    Some(b) => Some(b),
                    None => easyeffects::bypass()?,
                },
                autoload: None,
            },
        };
        let (applied, file) = match easyeffects::apply(
            eq,
            backup_parent,
            target.as_ref(),
            before.autoload.as_ref(),
        ) {
            Ok(done) => done,
            Err(e) => {
                self.before_apply = Some(before);
                return Err(e);
            }
        };
        self.before_apply = Some(easyeffects::Before {
            autoload: before.autoload.or(file),
            ..before
        });
        // EasyEffects is now active with the installed EQ; exit must not override its
        // bypass state or undo a later rollback.
        crate::restore::forget("easyeffects-bypass");
        Ok(applied)
    }

    fn undo_eq(&mut self, applied: &Applied) -> Result<String> {
        let Some(before) = self.before_apply.take() else {
            bail!("nothing has been installed to roll back");
        };
        let done = easyeffects::undo(&before, applied)
            .inspect_err(|_| self.before_apply = Some(before))?;
        crate::restore::forget("easyeffects-bypass");
        Ok(done)
    }
}
