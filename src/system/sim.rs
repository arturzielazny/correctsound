//! Simulated system to pair with [`crate::audio::sim`].

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::alsa::CaptureControl;
use super::{Applied, Devices, MicGain, Node, System};
use crate::audio::sim::SimState;
use crate::eq::EqSet;

/// Fake devices whose state lives in [`SimState`].
#[derive(Debug)]
pub struct SimSystem {
    state: Arc<Mutex<SimState>>,
    mic_gain: MicGain,
    bypass: bool,
}

impl SimSystem {
    #[must_use]
    pub fn new(state: Arc<Mutex<SimState>>) -> Self {
        Self {
            state,
            // Start off-unity so the gain screen has something to fix.
            mic_gain: MicGain {
                pw_volume: 0.42,
                pw_muted: false,
                alsa: Some(CaptureControl {
                    value: 127,
                    max: 127,
                    switch_on: true,
                }),
            },
            bypass: false,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SimState> {
        self.state.lock().expect("sim state lock poisoned")
    }
}

const SINK: &str = "sim_output.analog-stereo";

impl System for SimSystem {
    fn devices(&mut self) -> Result<Devices> {
        Ok(Devices {
            mic: Some(Node {
                id: 1,
                name: "sim_input.UMIK-1".into(),
                description: "Simulated UMIK-1".into(),
                positions: vec!["MONO".into()],
                alsa_card: None,
            }),
            sinks: vec![Node {
                id: 2,
                name: SINK.into(),
                description: "Simulated speakers".into(),
                positions: vec!["FL".into(), "FR".into()],
                alsa_card: None,
            }],
            preferred_sink: Some(SINK.into()),
            other_streams: vec![],
            easyeffects_running: true,
        })
    }

    fn mic_gain(&mut self, _mic: &Node) -> Result<MicGain> {
        Ok(self.mic_gain)
    }

    fn set_mic_unity(&mut self, _mic: &Node) -> Result<()> {
        self.mic_gain.pw_volume = 1.0;
        self.mic_gain.pw_muted = false;
        Ok(())
    }

    fn sink_volume(&mut self, _sink: &Node) -> Result<f64> {
        Ok(self.lock().sink_volume)
    }

    fn set_sink_volume(&mut self, _sink: &Node, volume: f64) -> Result<()> {
        self.lock().sink_volume = volume.clamp(0.0, 1.5);
        Ok(())
    }

    fn easyeffects_bypass(&mut self) -> Result<Option<bool>> {
        Ok(Some(self.bypass))
    }

    fn set_easyeffects_bypass(&mut self, bypass: bool) -> Result<()> {
        self.bypass = bypass;
        Ok(())
    }

    fn playback_links(&mut self, _node_name: &str) -> Result<Vec<String>> {
        Ok(vec![SINK.into()])
    }

    fn apply_preset(&mut self, eq: &EqSet, backup_parent: &Path, sink: &Node) -> Result<Applied> {
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
