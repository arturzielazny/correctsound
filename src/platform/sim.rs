//! The simulated platform, paired with the simulated room in [`crate::audio::sim`]: the
//! tests and `--simulate` run the whole wizard on it, on any OS, without playing a sound.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::{Applied, Device, Devices, EqHost, GainSetting, InputGain, Routing, System};
use crate::audio::sim::{SimBackend, SimConfig, SimState};
use crate::audio::{AudioBackend, Route};
use crate::eq::EqSet;

/// Fake devices whose state lives in [`SimState`].
#[derive(Debug)]
pub struct SimSystem {
    config: SimConfig,
    state: Arc<Mutex<SimState>>,
    /// The simulated input volume; starts off unity so the gain screen has work to do.
    input_volume: f64,
    bypass: bool,
}

impl SimSystem {
    #[must_use]
    pub fn new(config: SimConfig, state: Arc<Mutex<SimState>>) -> Self {
        Self {
            config,
            state,
            input_volume: 0.42,
            bypass: false,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SimState> {
        self.state.lock().expect("sim state lock poisoned")
    }
}

/// The output the simulated speakers are on.
pub const SINK: &str = "sim_output.analog-stereo";
/// How much quieter the mic hears an output other than the speakers, dB (a laptop's own
/// speakers across the room).
const OTHER_OUTPUT_DB: f64 = -45.0;

impl System for SimSystem {
    fn name(&self) -> &'static str {
        "simulator"
    }

    fn missing(&self) -> Vec<String> {
        Vec::new()
    }

    fn devices(&mut self) -> Result<Devices> {
        let st = self.lock();
        let mut sinks: Vec<Device> = st
            .other_outputs
            .iter()
            .enumerate()
            .map(|(i, (name, description))| Device {
                id: format!("{}", 10 + i),
                name: name.clone(),
                description: description.clone(),
                stereo: true,
            })
            .collect();
        sinks.push(Device {
            id: "2".into(),
            name: SINK.into(),
            description: "Simulated speakers".into(),
            stereo: true,
        });
        Ok(Devices {
            mic: st.mic_plugged.then(|| Device {
                id: "1".into(),
                name: "sim_input.UMIK-1".into(),
                description: "Simulated UMIK-1".into(),
                stereo: false,
            }),
            preferred_sink: Some(st.default_output.clone().unwrap_or_else(|| SINK.into())),
            sinks,
            other_streams: vec![],
            eq_host: Some(EqHost {
                name: "EasyEffects".into(),
                running: true,
            }),
        })
    }

    fn find_input(&mut self, name: &str) -> Result<Option<Device>> {
        Ok(self.devices()?.mic.filter(|m| m.name == name))
    }

    fn input_gain(&mut self, _mic: &Device) -> Result<InputGain> {
        Ok(InputGain {
            settings: vec![
                GainSetting {
                    label: "Input volume".into(),
                    value: format!("{:.2}", self.input_volume),
                    at_unity: (self.input_volume - 1.0).abs() < 1e-3,
                },
                GainSetting {
                    label: "Capture control".into(),
                    value: "127/127".into(),
                    at_unity: true,
                },
            ],
        })
    }

    fn set_input_unity(&mut self, _mic: &Device) -> Result<()> {
        self.input_volume = 1.0;
        Ok(())
    }

    fn input_gain_help(&self) -> &'static str {
        "the simulator should have set it; this is a bug"
    }

    fn output_volume(&mut self, _sink: &Device) -> Result<f64> {
        Ok(self.lock().sink_volume)
    }

    fn set_output_volume(&mut self, _sink: &Device, volume: f64) -> Result<()> {
        self.lock().sink_volume = volume.clamp(0.0, 1.5);
        Ok(())
    }

    fn eq_bypass(&mut self) -> Result<Option<bool>> {
        Ok(Some(self.bypass))
    }

    fn set_eq_bypass(&mut self, bypass: bool) -> Result<()> {
        self.bypass = bypass;
        Ok(())
    }

    fn audio(&mut self, _mic: &Device, sink: &Device) -> Box<dyn AudioBackend> {
        let mut config = self.config.clone();
        if sink.name != SINK {
            for room in &mut config.rooms {
                room.gain_db += OTHER_OUTPUT_DB;
            }
        }
        Box::new(SimBackend::new(config, Arc::clone(&self.state)))
    }

    fn check_routing(&mut self, _route: Route, _sink: &Device) -> Result<Routing> {
        Ok(Routing::Ok)
    }

    fn apply_eq(&mut self, eq: &EqSet, backup_parent: &Path, sink: &Device) -> Result<Applied> {
        self.lock().eq = Some(eq.clone());
        self.bypass = false;
        let path = backup_parent.join("CorrectSound.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&crate::eq::export::easyeffects_preset(eq))?,
        )?;
        Ok(Applied {
            preset_name: "CorrectSound (simulated)".into(),
            preset_path: path,
            backup_dir: backup_parent.join("easyeffects-backup (simulated, nothing changed)"),
            autoload: Some(sink.description.clone()),
        })
    }
}
