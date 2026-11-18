//! The on-disk session directory: recordings, responses, settings and outputs.

use crate::files as fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audio::Channel;
use crate::dsp::SAMPLE_RATE;
use crate::dsp::spectrum::Response;

/// Settings recorded with a session, enough to redo the analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub created: String,
    pub mic_serial: Option<String>,
    pub calibration_file: Option<PathBuf>,
    pub sens_factor: f64,
    pub spl_offset_db: f64,
    pub sweep_dbfs: f64,
    pub sink: String,
    pub simulated: bool,
    /// Number of mic positions per channel.
    pub positions: [usize; 2],
    /// Background noise from the level step, dB SPL in its 500–2000 Hz band.
    #[serde(default)]
    pub ambient_spl: Option<f64>,
    /// The level the amplifier was set to, dB SPL on the level step's meter (sessions from
    /// before it was worked out from the room: 75).
    #[serde(default)]
    pub level_spl: Option<f64>,
    /// The level the room's background called for, dB SPL (`level_spl` may be louder).
    #[serde(default)]
    pub needed_spl: Option<f64>,
    /// The room's background noise per octave ([`crate::dsp::ambient::OCTAVES`]), dB SPL.
    #[serde(default)]
    pub room_noise_spl: Option<Vec<f64>>,
    /// The room and where the speakers and the seat were, when entered.
    #[serde(default)]
    pub room: Option<crate::dsp::roommodel::Setup>,
}

/// A session directory.
#[derive(Debug, Clone)]
pub struct Session {
    dir: PathBuf,
}

impl Session {
    /// Creates `$XDG_DATA_HOME/correctsound/sessions/<timestamp>/`.
    ///
    /// # Errors
    ///
    /// Fails if the directory cannot be created.
    pub fn create() -> Result<Self> {
        let stamp = chrono::Local::now().format("%Y-%m-%d_%H%M%S").to_string();
        let dir = crate::paths::data_dir().join("sessions").join(stamp);
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// Saved sessions with measurements, newest first.
    #[must_use]
    pub fn list() -> Vec<(PathBuf, SessionInfo)> {
        let root = crate::paths::data_dir().join("sessions");
        let mut found: Vec<(PathBuf, SessionInfo)> = fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter_map(|dir| {
                let info = Self::open(&dir).load_info().ok()?;
                (info.positions.iter().all(|&n| n > 0)).then_some((dir, info))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0));
        found
    }

    /// Opens an existing session directory.
    #[must_use]
    pub fn open(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn stem(channel: Channel, position: usize) -> String {
        format!("{}-{}", channel.name().to_lowercase(), position + 1)
    }

    /// Saves a raw capture as a 32-bit float WAV.
    ///
    /// # Errors
    ///
    /// Fails on I/O errors.
    pub fn save_capture(&self, channel: Channel, position: usize, samples: &[f32]) -> Result<()> {
        write_wav(
            &self
                .dir
                .join(format!("{}-capture.wav", Self::stem(channel, position))),
            samples,
        )
    }

    /// Saves the capture of a verification sweep (through EasyEffects), replacing any
    /// earlier one.
    ///
    /// # Errors
    ///
    /// Fails on I/O errors.
    pub fn save_verify_capture(&self, channel: Channel, samples: &[f32]) -> Result<()> {
        let name = format!("verify-{}-capture.wav", channel.name().to_lowercase());
        write_wav(&self.dir.join(name), samples)
    }

    /// Path of a saved capture.
    #[must_use]
    pub fn capture_path(&self, channel: Channel, position: usize) -> PathBuf {
        self.dir
            .join(format!("{}-capture.wav", Self::stem(channel, position)))
    }

    /// Saves a calibrated response as CSV (Hz, dB SPL).
    ///
    /// # Errors
    ///
    /// Fails on I/O errors.
    pub fn save_response(&self, name: &str, response: &Response) -> Result<()> {
        let mut text = String::from("frequency_hz,spl_db\n");
        for (f, d) in response.freqs.iter().zip(&response.db) {
            text.push_str(&format!("{f:.3},{d:.3}\n"));
        }
        self.write(&format!("{name}.csv"), &text)
    }

    /// Saves the session settings.
    ///
    /// # Errors
    ///
    /// Fails on I/O or serialisation errors.
    pub fn save_info(&self, info: &SessionInfo) -> Result<()> {
        self.write("session.toml", &toml::to_string(info)?)
    }

    /// Loads the session settings.
    ///
    /// # Errors
    ///
    /// Fails if the file is missing or invalid.
    pub fn load_info(&self) -> Result<SessionInfo> {
        let path = self.dir.join("session.toml");
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Writes a text file into the session.
    ///
    /// # Errors
    ///
    /// Fails on I/O errors.
    pub fn write(&self, name: &str, contents: &str) -> Result<()> {
        let path = self.dir.join(name);
        fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))
    }
}

/// Writes mono 32-bit float WAV at the app sample rate.
///
/// # Errors
///
/// Fails on I/O errors.
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut bytes = std::io::Cursor::new(Vec::new());
    let mut w = hound::WavWriter::new(&mut bytes, spec)?;
    for &s in samples {
        w.write_sample(s)?;
    }
    w.finalize()?;
    fs::write(path, bytes.into_inner()).with_context(|| format!("writing {}", path.display()))
}

/// Reads a mono float WAV written by [`write_wav`].
///
/// # Errors
///
/// Fails on I/O or format errors.
pub fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let bytes = fs::read(path).with_context(|| format!("opening {}", path.display()))?;
    let mut r = hound::WavReader::new(std::io::Cursor::new(bytes))
        .with_context(|| format!("reading {}", path.display()))?;
    r.samples::<f32>()
        .collect::<Result<Vec<f32>, _>>()
        .with_context(|| format!("reading {}", path.display()))
}
