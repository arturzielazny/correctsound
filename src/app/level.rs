//! Step 4: listening to the room's background noise, which sets how loud the measurement
//! needs to be ([`crate::dsp::ambient`]), then pink noise per channel with a live SPL meter
//! for setting the amplifier to that level. The step's state, its keys and its background work.

use std::sync::Arc;

use anyhow::Result;
use crossterm::event::KeyCode;

use super::{App, Step};
use crate::audio::{AudioBackend, Channel, Program, Route, Source, Stream};
use crate::cal::Calibration;
use crate::dsp::SAMPLE_RATE;
use crate::dsp::ambient::{AmbientMeter, OCTAVES, OctaveLevels, Suggestion};
use crate::dsp::level::{BandFilter, LevelMeter, SplScale, median_block_dbfs, rms_dbfs};
use crate::dsp::noise::pink_band;

/// Level for the noise before the room has been listened to (sessions saved before the
/// level was worked out from the room), dB SPL.
pub const DEFAULT_TARGET_SPL: f64 = 75.0;
/// Green zone half-width, dB.
pub const TOLERANCE: f64 = 1.5;
/// Playback stops above this, dB SPL.
pub const ABORT_SPL: f64 = 95.0;
/// Noise level, dBFS RMS.
pub const NOISE_DBFS: f64 = -20.0;
/// Band of the test noise, Hz; the meter only counts this band (with an octave of margin).
pub const NOISE_LOW_HZ: f64 = 500.0;
pub const NOISE_HIGH_HZ: f64 = 2000.0;
/// Recording skipped when listening starts, while the stream and filters settle.
const LISTEN_SETTLE: usize = SAMPLE_RATE as usize / 5;
/// Background kept for the level in the meter's band: the last 3 s.
const AMBIENT_KEEP: usize = SAMPLE_RATE as usize * 3;
/// The suggestion moves down only by this much at once, dB, so it does not flicker; it
/// moves up at once, the safe direction.
const SUGGESTION_HYSTERESIS_DB: f64 = 2.0;
/// Fade-in for every noise start, ms: short, so the meter shows the level quickly, while
/// the safety limit ([`ABORT_SPL`]) still catches an amplifier left far too loud.
const FADE_IN_MS: u64 = 500;
/// Recording after the fade-in starts before the meter starts afresh, ms: the fade plus
/// the output's latency, so the quiet start does not drag the average down.
const METER_RESTART_MS: u64 = FADE_IN_MS + 150;
/// The noise is taken not to reach the mic when the meter reads less than this above the
/// room's background, dB.
const UNHEARD_DB: f64 = 10.0;
/// After that, the meter's averaging needs this long before a reading counts, ms.
const METER_SETTLE_MS: u64 = 300;
/// Above this the sweeps get uncomfortably loud over a whole measurement, and small
/// speakers distort in the bass, dB SPL. Up to it, louder than the room needs is fine: the
/// sweeps only gain margin over the background.
pub const COMFORT_MAX_SPL: f64 = 85.0;

/// How a meter reading compares with the level the room calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    /// Below the target: the quietest octaves end up too close to the background.
    TooQuiet,
    /// At the target.
    Right,
    /// Above the target but not loud: more than needed, and fine.
    LouderThanNeeded,
    /// Above [`COMFORT_MAX_SPL`].
    TooLoud,
}

impl Reading {
    /// Judges `spl` against `target`, both dB SPL.
    #[must_use]
    pub fn of(spl: f64, target: f64) -> Self {
        if spl < target - TOLERANCE {
            Self::TooQuiet
        } else if spl <= target + TOLERANCE {
            Self::Right
        } else if spl <= COMFORT_MAX_SPL.max(target + TOLERANCE) {
            Self::LouderThanNeeded
        } else {
            Self::TooLoud
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LevelPhase {
    /// Not started (or audio unavailable).
    Idle,
    /// Listening to the room's background noise, continuously.
    Listening,
    /// Noise is playing.
    Playing,
    /// Playback stopped; the reason is shown.
    Stopped(String),
}

/// What the level screen shows and owns.
pub struct LevelState {
    pub channel: Channel,
    pub phase: LevelPhase,
    /// The level to set, dB SPL: the room's suggestion, fixed when the noise starts.
    pub target_spl: f64,
    /// The room's background per octave ([`OCTAVES`]), dB SPL, corrected for the mic.
    pub room_noise: Option<OctaveLevels>,
    /// The measurement level the room's background calls for, while listening.
    pub suggestion: Option<Suggestion>,
    /// Background noise in the meter's band, dB SPL.
    pub ambient_spl: Option<f64>,
    /// Averaged SPL of the current noise.
    pub spl: Option<f64>,
    /// Last steady SPL per channel.
    pub heard: [Option<f64>; 2],
    /// The level set in a session being reviewed, dB SPL (nothing is heard then).
    pub reviewed_set_spl: Option<f64>,
    pub sink_volume: Option<f64>,
    stream: Option<Stream>,
    meter: LevelMeter,
    listener: AmbientMeter,
    /// Samples still to skip before listening counts.
    settle: usize,
    /// The last few seconds of background, filtered to the meter's band.
    ambient: Vec<f32>,
    /// Samples recorded since the noise started: the meter's timing is counted in audio,
    /// not clock time, so the simulator behaves like a room.
    received: usize,
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
            target_spl: DEFAULT_TARGET_SPL,
            room_noise: None,
            suggestion: None,
            ambient_spl: None,
            spl: None,
            heard: [None, None],
            reviewed_set_spl: None,
            sink_volume: None,
            stream: None,
            meter: LevelMeter::quick(SAMPLE_RATE),
            listener: AmbientMeter::new(),
            settle: 0,
            ambient: Vec::new(),
            received: 0,
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

    /// Starts listening to the room, until [`Self::start_noise`].
    ///
    /// # Errors
    ///
    /// Fails if the capture stream cannot start.
    pub fn start_listening(&mut self, audio: &mut dyn AudioBackend) -> Result<()> {
        self.stop();
        self.ambient.clear();
        self.band.reset();
        self.listener.reset();
        self.room_noise = None;
        self.suggestion = None;
        self.settle = LISTEN_SETTLE;
        self.stream =
            Some(audio.start(Source::new(Program::Silence, self.channel), Route::Hardware)?);
        self.phase = LevelPhase::Listening;
        Ok(())
    }

    /// Seconds of the room heard so far while listening (up to 3).
    #[must_use]
    pub fn listened_secs(&self) -> f64 {
        self.listener.heard_secs()
    }

    /// True when the noise may start: the room has been heard long enough for a
    /// suggestion, or listening is over.
    #[must_use]
    pub fn can_play(&self) -> bool {
        self.phase != LevelPhase::Listening || self.suggestion.is_some()
    }

    /// Starts (or restarts) the noise on `channel`, fading in. Coming from listening, the
    /// suggested level becomes the target.
    ///
    /// # Errors
    ///
    /// Fails if the streams cannot start.
    pub fn start_noise(
        &mut self,
        audio: &mut dyn AudioBackend,
        channel: Channel,
        scale: SplScale,
    ) -> Result<()> {
        if self.phase == LevelPhase::Listening {
            if let Some(s) = self.suggestion {
                self.target_spl = s.level_spl;
            }
            if !self.ambient.is_empty() {
                let block = SAMPLE_RATE as usize / 10;
                self.ambient_spl = Some(scale.spl(median_block_dbfs(&self.ambient, block)));
            }
        }
        self.stop();
        self.channel = channel;
        self.meter.reset();
        self.band.reset();
        self.spl = None;
        self.received = 0;
        let program = Program::Loop {
            signal: Arc::clone(&self.noise),
            fade_in: ms_to_samples(FADE_IN_MS),
        };
        self.stream = Some(audio.start(Source::new(program, channel), Route::Hardware)?);
        self.phase = LevelPhase::Playing;
        Ok(())
    }

    /// Stops any audio.
    pub fn stop(&mut self) {
        self.stream = None;
        if matches!(self.phase, LevelPhase::Playing | LevelPhase::Listening) {
            self.phase = LevelPhase::Idle;
        }
    }

    /// True while the noise fades in and the meter settles, for the "fading in" hint.
    #[must_use]
    pub fn fading_in(&self) -> bool {
        self.phase == LevelPhase::Playing
            && self.received < ms_to_samples(METER_RESTART_MS + METER_SETTLE_MS)
    }

    /// The level the amplifier was left at: the louder of the steady readings, dB SPL.
    #[must_use]
    pub fn set_spl(&self) -> Option<f64> {
        self.heard
            .iter()
            .flatten()
            .copied()
            .reduce(f64::max)
            .or(self.reviewed_set_spl)
    }

    /// Processes captured audio. `cal` corrects the room's noise levels for the mic.
    pub fn tick(&mut self, scale: SplScale, cal: Option<&Calibration>) {
        let Some(stream) = &self.stream else {
            return;
        };
        let samples = stream.drain();
        match self.phase {
            LevelPhase::Listening => {
                // The first moments, while the stream and filter settle, do not count.
                let skip = self.settle.min(samples.len());
                self.settle -= skip;
                let samples = &samples[skip..];
                let filtered = self.band.process(samples);
                self.ambient.extend_from_slice(&filtered);
                if self.ambient.len() > AMBIENT_KEEP {
                    self.ambient.drain(..self.ambient.len() - AMBIENT_KEEP);
                }
                self.listener.push(samples);
                if let Some(dbfs) = self.listener.steady_dbfs() {
                    let raw = dbfs.map(|d| scale.spl(d));
                    let mut shown = raw;
                    for (level, &f) in shown.iter_mut().zip(&OCTAVES) {
                        *level -= cal.map_or(0.0, |c| c.db_at(f));
                    }
                    self.room_noise = Some(shown);
                    // The margin is worked out on the raw recording, like the check after
                    // each sweep, so the suggestion uses the uncorrected levels.
                    let new = Suggestion::for_noise(&raw);
                    let keep = self.suggestion.is_some_and(|old| {
                        new.level_spl <= old.level_spl
                            && new.level_spl > old.level_spl - SUGGESTION_HYSTERESIS_DB
                    });
                    if !keep {
                        self.suggestion = Some(new);
                    }
                }
            }
            LevelPhase::Playing => {
                if samples.is_empty() {
                    return;
                }
                // The safety limit uses the unfiltered level, which can only be higher.
                let instant = scale.spl(rms_dbfs(&samples));
                if instant > ABORT_SPL {
                    self.stream = None;
                    self.phase = LevelPhase::Stopped(format!(
                        "Stopped: {instant:.0} dB SPL is above the {ABORT_SPL:.0} dB safety limit. \
                         Turn the amplifier down, then press Space."
                    ));
                    return;
                }
                // Once the fade is over, the meter starts again from the full level.
                let restart = ms_to_samples(METER_RESTART_MS);
                let before = self.received;
                self.received += samples.len();
                if before < restart && self.received >= restart {
                    self.meter.reset();
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
    }

    /// True when the room's noise in the meter's band is within 25 dB of the target, so
    /// the meter reads it along with the noise.
    #[must_use]
    pub fn room_is_noisy(&self) -> bool {
        self.ambient_spl.is_some_and(|a| a > self.target_spl - 25.0)
    }

    /// True when the noise plays but the meter reads hardly more than the room's own
    /// background: the sound is not reaching the mic, most likely because it goes to an
    /// output the speakers are not on.
    #[must_use]
    pub fn noise_unheard(&self) -> bool {
        self.phase == LevelPhase::Playing
            && !self.fading_in()
            && self
                .spl
                .zip(self.ambient_spl)
                .is_some_and(|(s, a)| s < a + UNHEARD_DB)
    }
}

/// `ms` of audio in samples.
fn ms_to_samples(ms: u64) -> usize {
    (ms * u64::from(SAMPLE_RATE) / 1000) as usize
}

impl Default for LevelState {
    fn default() -> Self {
        Self::new()
    }
}

// ----- the step's keys and background work -----

impl App {
    pub(super) fn key_level(&mut self, code: KeyCode) {
        let listening = self.level.phase == LevelPhase::Listening;
        match code {
            // While listening, Enter or Space starts the noise at the suggested level.
            KeyCode::Enter | KeyCode::Char(' ') if listening => {
                if self.level.can_play() {
                    let ch = self.level.channel;
                    self.start_noise(ch);
                } else {
                    self.message = Some("Still listening to the room; a moment more.".into());
                }
            }
            KeyCode::Tab | KeyCode::Left | KeyCode::Right if !listening => {
                let ch = self.level.channel.other();
                self.start_noise(ch);
            }
            KeyCode::Char(' ') => {
                let ch = self.level.channel;
                self.start_noise(ch);
            }
            KeyCode::Char('l') if !listening => {
                if let Some(audio) = self.audio.as_mut()
                    && let Err(e) = self.level.start_listening(audio.as_mut())
                {
                    self.set_error(&e);
                }
            }
            KeyCode::Up | KeyCode::Down => {
                let step = if code == KeyCode::Up { 0.02 } else { -0.02 };
                let Some(sink) = self.sink().cloned() else {
                    return;
                };
                let current = self.system.output_volume(&sink).unwrap_or(1.0);
                let new = (current + step).clamp(0.0, 1.0);
                match self.system.set_output_volume(&sink, new) {
                    Ok(()) => self.level.sink_volume = Some(new),
                    Err(e) => self.set_error(&e),
                }
            }
            KeyCode::Enter => {
                if self.level.heard.iter().all(Option::is_none) {
                    self.message = Some("Let the noise play and set the level first.".into());
                    return;
                }
                self.measure.frozen_volume = self.current_sink_volume();
                self.go(Step::Measure);
            }
            _ => {}
        }
    }

    pub(super) fn start_noise(&mut self, channel: Channel) {
        let scale = self.spl_scale();
        let Some(audio) = self.audio.as_mut() else {
            return;
        };
        if let Err(e) = self.level.start_noise(audio.as_mut(), channel, scale) {
            self.set_error(&e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn louder_than_needed_is_fine_until_it_gets_loud() {
        // A quiet room needs 60 dB; 75 is more than needed and fine.
        assert_eq!(Reading::of(55.0, 60.0), Reading::TooQuiet);
        assert_eq!(Reading::of(60.8, 60.0), Reading::Right);
        assert_eq!(Reading::of(75.0, 60.0), Reading::LouderThanNeeded);
        assert_eq!(Reading::of(88.0, 60.0), Reading::TooLoud);
        // A target near the comfortable maximum still has its tolerance above it.
        assert_eq!(Reading::of(85.5, 84.5), Reading::Right);
    }
}
