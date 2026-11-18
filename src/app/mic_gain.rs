//! Step 3: setting the mic's input gain to the unity its calibration assumes.

use super::keys::KeyCode;
use super::{App, Step};
use crate::platform::InputGain;

impl App {
    pub(super) fn read_mic_gain(&mut self) {
        let Some(mic) = self.mic().cloned() else {
            return;
        };
        match self.system.input_gain(&mic) {
            Ok(g) => self.mic_gain = Some(g),
            Err(e) => self.set_error(&e),
        }
    }

    pub(super) fn key_mic_gain(&mut self, code: KeyCode) {
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
}
