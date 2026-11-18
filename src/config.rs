//! Remembered user choices (`$XDG_CONFIG_HOME/correctsound/config.toml`).

use crate::files as fs;

use serde::{Deserialize, Serialize};

use crate::cal::{Orientation, Serial};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub serial: Option<Serial>,
    #[serde(default)]
    pub orientation: Orientation,
    pub sink: Option<String>,
    /// The user's rooms (dimensions, where the speakers and the seat are), as entered.
    #[serde(default)]
    pub rooms: Vec<crate::dsp::roommodel::Setup>,
    /// A single room, as saved before there could be several: read once into `rooms`.
    #[serde(default, skip_serializing)]
    pub room: Option<crate::dsp::roommodel::Setup>,
}

impl Config {
    /// Loads the config, or the default if it is missing or unreadable.
    #[must_use]
    pub fn load() -> Self {
        fs::read_to_string(crate::paths::config_dir().join("config.toml"))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Saves the config; failures are ignored because it is only a convenience.
    pub fn save(&self) {
        let dir = crate::paths::config_dir();
        if let Ok(text) = toml::to_string(self) {
            let _ = fs::create_dir_all(&dir);
            let _ = fs::write(dir.join("config.toml"), text);
        }
    }
}
