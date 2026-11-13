//! Step 4: ambient noise, then pink noise per channel with a live SPL meter.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::audio::{AudioBackend, Channel, Program, Route, Source, Stream};
use crate::dsp::SAMPLE_RATE;
use crate::dsp::level::{BandFilter, LevelMeter, SplScale, median_block_dbfs, rms_dbfs};
use crate::dsp::noise::pink_band;

/// Target level for the noise, dB SPL.
pub const TARGET_SPL: f64 = 75.0;
/// Green zone half-width, dB.
pub const TOLERANCE: f64 = 1.5;
/// Playback stops above this, dB SPL.
pub const ABORT_SPL: f64 = 95.0;
/// Noise level, dBFS RMS.
pub const NOISE_DBFS: f64 = -20.0;
/// Band of the test noise, Hz; the meter only counts this band (with an octave of margin).
pub const NOISE_LOW_HZ: f64 = 500.0;
pub const NOISE_HIGH_HZ: f64 = 2000.0;
/// How long the ambient level is measured.
const AMBIENT: Duration = Duration::from_secs(2);
/// Fade-in for every noise start.
const FADE_IN_SECONDS: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum LevelPhase {
    /// Not started (or audio unavailable).
    Idle,
    /// Measuring the room's background level.
    Ambient,
    /// Noise is playing.
    Playing,
    /// Playback stopped; the reason is shown.
    Stopped(String),
}

/// What the level screen shows and owns.
pub struct LevelState {
    pub channel: Channel,
    pub phase: LevelPhase,
    pub ambient_spl: Option<f64>,
    /// Averaged SPL of the current noise.
    pub spl: Option<f64>,
    /// Last steady SPL per channel.
    pub heard: [Option<f64>; 2],
    pub sink_volume: Option<f64>,
    stream: Option<Stream>,
    meter: LevelMeter,
    ambient: Vec<f32>,
    started: Option<Instant>,
    noise: Arc<[f32]>,
    /// Limits the meter and the ambient reading to the noise's band.
    band: BandFilter,
}

impl std::fmt::Debug for LevelState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LevelState")
            .field("channel", &self.channel)
            .field("phase", &self.phase)
            .field("spl", &self.spl)
            .finish_non_exhaustive()
    }
}

impl LevelState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            channel: Channel::Left,
            phase: LevelPhase::Idle,
            ambient_spl: None,
            spl: None,
            heard: [None, None],
            sink_volume: None,
            stream: None,
            meter: LevelMeter::standard(SAMPLE_RATE),
            ambient: Vec::new(),
            started: None,
            noise: pink_band(SAMPLE_RATE, NOISE_LOW_HZ, NOISE_HIGH_HZ, NOISE_DBFS, 1).into(),
            band: BandFilter::new(SAMPLE_RATE, NOISE_LOW_HZ / 2.0, NOISE_HIGH_HZ * 2.0),
        }
    }

    /// Resets per-visit state.
    pub fn enter(&mut self, sink_volume: Option<f64>) {
        self.stop();
        self.sink_volume = sink_volume;
        self.phase = LevelPhase::Idle;
        self.spl = None;
    }

    /// Starts recording the ambient level.
    ///
    /// # Errors
    ///
    /// Fails if the capture stream cannot start.
    pub fn start_ambient(&mut self, audio: &mut dyn AudioBackend) -> Result<()> {
        self.stop();
        self.ambient.clear();
        self.band.reset();
        self.stream =
            Some(audio.start(Source::new(Program::Silence, self.channel), Route::Hardware)?);
        self.started = Some(Instant::now());
        self.phase = LevelPhase::Ambient;
        Ok(())
    }

    /// Starts (or restarts) the noise on `channel`, fading in.
    ///
    /// # Errors
    ///
    /// Fails if the streams cannot start.
    pub fn start_noise(&mut self, audio: &mut dyn AudioBackend, channel: Channel) -> Result<()> {
        self.stop();
        self.channel = channel;
        self.meter.reset();
        self.band.reset();
        self.spl = None;
        let program = Program::Loop {
            signal: Arc::clone(&self.noise),
            fade_in: FADE_IN_SECONDS * SAMPLE_RATE as usize,
        };
        self.stream = Some(audio.start(Source::new(program, channel), Route::Hardware)?);
        self.started = Some(Instant::now());
        self.phase = LevelPhase::Playing;
        Ok(())
    }

    /// Stops any audio.
    pub fn stop(&mut self) {
        self.stream = None;
        if matches!(self.phase, LevelPhase::Playing | LevelPhase::Ambient) {
            self.phase = LevelPhase::Idle;
        }
    }

    /// Seconds into the fade-in, for the "fading in" hint.
    #[must_use]
    pub fn fading_in(&self) -> bool {
        self.phase == LevelPhase::Playing
            && self
                .started
                .is_some_and(|t| t.elapsed() < Duration::from_secs(FADE_IN_SECONDS as u64 + 1))
    }

    /// Processes captured audio. Returns true when the ambient measurement just finished.
    pub fn tick(&mut self, scale: SplScale) -> bool {
        let Some(stream) = &self.stream else {
            return false;
        };
        let samples = stream.drain();
        match self.phase {
            LevelPhase::Ambient => {
                let filtered = self.band.process(&samples);
                self.ambient.extend_from_slice(&filtered);
                if self.started.is_some_and(|t| t.elapsed() >= AMBIENT) {
                    // Skip the first 0.3 s, where the stream and filter may still be settling.
                    let skip = (SAMPLE_RATE as usize * 3 / 10).min(self.ambient.len());
                    let block = SAMPLE_RATE as usize / 10;
                    let level = median_block_dbfs(&self.ambient[skip..], block);
                    self.ambient_spl = Some(scale.spl(level));
                    self.stream = None;
                    return true;
                }
            }
            LevelPhase::Playing => {
                if samples.is_empty() {
                    return false;
                }
                // The safety limit uses the unfiltered level, which can only be higher.
                let instant = scale.spl(rms_dbfs(&samples));
                if instant > ABORT_SPL {
                    self.stream = None;
                    self.phase = LevelPhase::Stopped(format!(
                        "Stopped: {instant:.0} dB SPL is above the {ABORT_SPL:.0} dB safety limit. \
                         Turn the amplifier down, then press Space."
                    ));
                    return false;
                }
                self.meter.push(&self.band.process(&samples));
                if let Some(db) = self.meter.dbfs() {
                    let spl = scale.spl(db);
                    self.spl = Some(spl);
                    if !self.fading_in() {
                        self.heard[self.channel.index()] = Some(spl);
                    }
                }
            }
            LevelPhase::Idle | LevelPhase::Stopped(_) => {}
        }
        false
    }

    /// True when the room's noise floor is within 25 dB of the target.
    #[must_use]
    pub fn room_is_noisy(&self) -> bool {
        self.ambient_spl.is_some_and(|a| a > TARGET_SPL - 25.0)
    }
}

impl Default for LevelState {
    fn default() -> Self {
        Self::new()
    }
}
