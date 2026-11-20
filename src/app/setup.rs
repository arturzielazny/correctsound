//! Step 1, setup: the mic, its calibration file and input gain, the EQ host, and the output
//! the speakers are on, on one screen. What was chosen last time is remembered (the output,
//! the mic's serial number and orientation) and the calibration file is saved after its first
//! download, so a later run needs one Enter.

use std::sync::mpsc;

use anyhow::{Result, anyhow, bail};
use web_time::Instant;

use super::keys::KeyCode;
use super::{App, CalStatus, Step};
use crate::cal::{self, Orientation, Serial};
use crate::platform::{Device, InputGain};

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
                if self.step == Step::Setup && !self.reviewing() {
                    self.read_mic_gain();
                }
            }
            // Reviewing needs devices only for applying; where there are none (a platform
            // without support), the review itself still works.
            Err(_) if self.reviewing() => {}
            Err(e) => self.set_error(&e),
        }
    }

    /// True when the calibration file comes from miniDSP by the mic's serial number: not
    /// when simulating, nor when one was given on the command line.
    #[must_use]
    pub fn calibration_by_serial(&self) -> bool {
        !self.simulated() && self.options.cal_file.is_none()
    }

    pub(super) fn key_setup(&mut self, code: KeyCode) {
        let n = self.devices.as_ref().map_or(0, |d| d.sinks.len());
        let editing = self.calibration_by_serial() && self.cal_status != CalStatus::Downloading;
        match code {
            KeyCode::Up if n > 0 => self.sink_index = (self.sink_index + n - 1) % n,
            KeyCode::Down if n > 0 => self.sink_index = (self.sink_index + 1) % n,
            KeyCode::Char('r') => self.poll_devices(),
            KeyCode::Char(c)
                if editing && (c.is_ascii_digit() || c == '-') && self.serial_input.len() < 9 =>
            {
                self.serial_input.push(c);
                self.calibration_changed();
            }
            KeyCode::Backspace if editing => {
                self.serial_input.pop();
                self.calibration_changed();
            }
            KeyCode::Tab | KeyCode::Left | KeyCode::Right if editing => {
                self.orientation = match self.orientation {
                    Orientation::OnAxis => Orientation::Vertical,
                    Orientation::Vertical => Orientation::OnAxis,
                };
                self.calibration_changed();
            }
            KeyCode::Enter => self.continue_setup(),
            _ => {}
        }
    }

    /// The serial number or the orientation changed: the file loaded no longer fits, and a
    /// saved one for the new choice is loaded at once.
    fn calibration_changed(&mut self) {
        self.cal = None;
        self.cal_path = None;
        self.cal_status = CalStatus::Idle;
        self.continue_after_download = false;
        self.load_saved_calibration();
    }

    /// Loads the calibration without asking anyone: the file given on the command line, or
    /// the one saved for the serial number and orientation, if it was downloaded before.
    pub(super) fn load_saved_calibration(&mut self) {
        if let Some(path) = self.options.cal_file.clone() {
            match cal::load(&path) {
                Ok(c) => {
                    self.cal = Some(c);
                    self.cal_path = Some(path);
                }
                Err(e) => self.set_error(&e),
            }
            return;
        }
        if !self.calibration_by_serial() {
            return;
        }
        if let Ok(serial) = self.serial_input.parse::<Serial>()
            && let Some((c, path)) = cal::saved(&serial, self.orientation, &cal::cache_dir())
        {
            self.cal = Some(c);
            self.cal_path = Some(path);
        }
    }

    /// Enter on the setup screen: everything that can be done for the user is done (the
    /// calibration downloaded, the mic gain set to unity), and the level step opens; or
    /// what is missing is said.
    fn continue_setup(&mut self) {
        if let Err(e) = self.try_continue_setup() {
            self.continue_after_download = false;
            self.set_error(&e);
        }
    }

    fn try_continue_setup(&mut self) -> Result<()> {
        if !self.missing_tools.is_empty() {
            bail!("missing tools: {}", self.missing_tools.join(", "));
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
            bail!("{} has no left/right channels", sink.description);
        }
        if self.cal.is_none() {
            if let Some(path) = self.options.cal_file.clone() {
                self.cal = Some(cal::load(&path)?);
                self.cal_path = Some(path);
            } else {
                // Downloaded first; this runs again when it arrives.
                if self.cal_status != CalStatus::Downloading {
                    self.start_download()?;
                }
                self.continue_after_download = true;
                return Ok(());
            }
        }
        // The setup screen said Enter does this, and that it goes back on exit.
        self.set_mic_gain_to_unity(&mic)?;
        // EasyEffects takes about a third of a second for this: it runs beside the screen,
        // and the level step starts listening once it is done.
        if self.eq_host_running() && self.eq_bypass_job.is_none() {
            self.eq_bypass_job = Some(crate::background::spawn(self.system.bypass_eq(true)));
        }
        self.audio = Some(self.system.audio(&mic, &sink));
        if !self.simulated() {
            self.config.sink = Some(sink.name.clone());
            if let Ok(serial) = self.serial_input.parse::<Serial>() {
                self.config.serial = Some(serial);
                self.config.orientation = self.orientation;
            }
            self.config.save();
        }
        self.go(Step::Level);
        Ok(())
    }

    /// The EQ host has been bypassed (or failed to be): the level step starts listening, or
    /// on failure setup says why.
    pub(super) fn poll_eq_bypass(&mut self) {
        let Some(job) = &self.eq_bypass_job else {
            return;
        };
        let done = match job.try_recv() {
            Ok(done) => done,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow!("it stopped unexpectedly")),
        };
        self.eq_bypass_job = None;
        match done {
            Ok(()) if self.step == Step::Level => self.start_listening(),
            Ok(()) => {}
            Err(e) => {
                let host = self.eq_host_name().to_owned();
                self.go(Step::Setup);
                self.message = Some(format!("Could not bypass {host}: {e:#}"));
            }
        }
    }

    /// True while the EQ host is being bypassed, before the level step can listen.
    #[must_use]
    pub fn bypassing_eq(&self) -> bool {
        self.eq_bypass_job.is_some()
    }

    pub(super) fn read_mic_gain(&mut self) {
        let Some(mic) = self.mic().cloned() else {
            return;
        };
        match self.system.input_gain(&mic) {
            Ok(g) => self.mic_gain = Some(g),
            Err(e) => self.set_error(&e),
        }
    }

    /// Sets the mic's input gain to the unity the calibration assumes, if it is not there
    /// (it is restored on exit).
    fn set_mic_gain_to_unity(&mut self, mic: &Device) -> Result<()> {
        let gain = self.system.input_gain(mic)?;
        if gain.is_unity() {
            self.mic_gain = Some(gain);
            return Ok(());
        }
        self.system.set_input_unity(mic)?;
        let now = self.system.input_gain(mic)?;
        let unity = now.is_unity();
        self.mic_gain = Some(now);
        if !unity {
            bail!(
                "the mic gain did not change; {}",
                self.system.input_gain_help()
            );
        }
        Ok(())
    }

    fn start_download(&mut self) -> Result<()> {
        if self.serial_input.is_empty() {
            bail!("type the serial number printed on the mic first, like 700-1234");
        }
        let serial: Serial = self.serial_input.parse()?;
        let orientation = self.orientation;
        self.cal_rx = Some(crate::background::spawn(move || {
            cal::fetch(&serial, orientation, &cal::cache_dir())
        }));
        self.cal_status = CalStatus::Downloading;
        Ok(())
    }

    pub(super) fn poll_download(&mut self) {
        let Some(rx) = &self.cal_rx else { return };
        match rx.try_recv() {
            Ok(Ok((c, path))) => {
                self.cal = Some(c);
                self.cal_path = Some(path);
                self.cal_status = CalStatus::Downloaded;
                self.cal_rx = None;
                if std::mem::take(&mut self.continue_after_download) {
                    self.continue_setup();
                }
            }
            Ok(Err(e)) => {
                self.cal_status = CalStatus::Failed(format!("{e:#}"));
                self.cal_rx = None;
                self.continue_after_download = false;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.cal_status = CalStatus::Failed("the download stopped unexpectedly".into());
                self.cal_rx = None;
                self.continue_after_download = false;
            }
        }
    }

    /// True when the mic's input gain is known to be off unity (Enter sets it).
    #[must_use]
    pub fn mic_gain_off_unity(&self) -> bool {
        self.mic_gain
            .as_ref()
            .is_some_and(|g| !InputGain::is_unity(g))
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
