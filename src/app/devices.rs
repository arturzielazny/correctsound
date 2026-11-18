//! Step 1: finding the mic and the outputs, and choosing the output the speakers are on.

use web_time::Instant;

use anyhow::{Result, anyhow};

use super::keys::KeyCode;
use super::{App, Step};
use crate::cal;
use crate::platform::Device;

impl App {
    pub(super) fn poll_devices(&mut self) {
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
                let selected = self.sink().map(|s| s.name.clone());
                self.sink_index = sink_to_select(
                    &d.sinks,
                    [
                        self.options.sink.as_deref(),
                        selected.as_deref(),
                        self.config.sink.as_deref(),
                        d.preferred_sink.as_deref(),
                    ],
                );
                self.devices = Some(d);
            }
            // Reviewing needs devices only for applying; where there are none (a platform
            // without support), the review itself still works.
            Err(_) if self.reviewing() => {}
            Err(e) => self.set_error(&e),
        }
    }

    pub(super) fn key_preflight(&mut self, code: KeyCode) {
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

    pub(super) fn confirm_devices(&mut self) -> Result<()> {
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

    /// A warning when the selected output is not the one the last measurement used.
    #[must_use]
    pub fn output_warning(&self) -> Option<String> {
        output_warning(
            &self.devices.as_ref()?.sinks,
            self.sink()?,
            self.last_measured_sink.as_deref()?,
        )
    }
}

/// The output to select after listing the devices again: the first of `wanted` that is
/// present, in order of precedence (the command line, the output already selected, the one
/// saved from last time, the platform's preference); else the first output.
///
/// Keeping the selected output by name matters while the app waits for the mic: devices
/// are listed every second then, and the choice must not drift to the system default.
fn sink_to_select(sinks: &[Device], wanted: [Option<&str>; 4]) -> usize {
    wanted
        .into_iter()
        .flatten()
        .find_map(|w| sinks.iter().position(|s| s.name == w))
        .unwrap_or(0)
}

/// What to say when `selected` is not `last`, the output the last measurement was made on:
/// a different output plays the sweeps somewhere else (the laptop's own speakers, say).
fn output_warning(sinks: &[Device], selected: &Device, last: &str) -> Option<String> {
    if selected.name == last {
        return None;
    }
    Some(match sinks.iter().find(|s| s.name == last) {
        Some(s) => format!(
            "Your last measurement used {}, not the output selected below. Check that the \
             speakers are connected to the one selected.",
            s.description
        ),
        None => format!(
            "Your last measurement used an output that is not here now ({last}). If the \
             speakers are on it, switch it on or plug it in; this list updates with r."
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_when_the_output_is_not_the_last_one() {
        let laptop = output("laptop");
        let mut hdmi = output("hdmi");
        hdmi.description = "HDMI 4".into();
        let sinks = [laptop.clone(), hdmi.clone()];
        assert_eq!(output_warning(&sinks, &hdmi, "hdmi"), None);
        let other = output_warning(&sinks, &laptop, "hdmi").expect("a warning");
        assert!(other.contains("HDMI 4"), "{other}");
        let gone = output_warning(&sinks, &laptop, "usb-dac").expect("a warning");
        assert!(gone.contains("not here now (usb-dac)"), "{gone}");
    }

    fn output(name: &str) -> Device {
        Device {
            id: name.into(),
            name: name.into(),
            description: name.into(),
            stereo: true,
        }
    }

    #[test]
    fn the_chosen_output_survives_listing_the_devices_again() {
        let laptop = output("laptop");
        let speakers = output("speakers");
        // First listing: nothing selected yet, so the output saved last time wins over the
        // system default (the laptop).
        let sinks = [laptop.clone(), speakers.clone()];
        let first = sink_to_select(&sinks, [None, None, Some("speakers"), Some("laptop")]);
        assert_eq!(sinks[first].name, "speakers");
        // The mic is plugged in and the outputs come back in another order: still the
        // speakers, not the default and not the same index.
        let sinks = [speakers.clone(), output("hdmi"), laptop.clone()];
        let again = sink_to_select(
            &sinks,
            [None, Some("speakers"), Some("speakers"), Some("laptop")],
        );
        assert_eq!(sinks[again].name, "speakers");
        // A choice made with the arrows beats the saved one.
        let picked = sink_to_select(&sinks, [None, Some("hdmi"), Some("speakers"), None]);
        assert_eq!(sinks[picked].name, "hdmi");
        // The command line beats everything; an output that is gone falls through.
        let sinks = [laptop, speakers];
        let named = sink_to_select(&sinks, [Some("speakers"), Some("laptop"), None, None]);
        assert_eq!(sinks[named].name, "speakers");
        let gone = sink_to_select(&sinks, [None, Some("hdmi"), None, Some("laptop")]);
        assert_eq!(sinks[gone].name, "laptop");
        assert_eq!(sink_to_select(&[], [Some("x"), None, None, None]), 0);
    }
}
