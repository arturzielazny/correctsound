//! A simulated room, mic and sink, for tests and `--simulate`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;

use super::scene::{Scene, ScenePath};
use super::{AudioBackend, Channel, Route, Source, Stream};
use crate::cal::Calibration;
use crate::dsp::biquad::{Biquad, BiquadState};
use crate::dsp::{Rng, SAMPLE_RATE, db_to_amplitude};
use crate::eq::EqSet;

const RATE: f64 = SAMPLE_RATE as f64;

/// One channel's acoustic path from sink to mic.
#[derive(Debug, Clone)]
pub struct SimRoom {
    /// Room modes and speaker irregularities: (Hz, dB, Q).
    pub resonances: Vec<(f64, f64, f64)>,
    /// Speaker low-frequency roll-off: (Hz, Q).
    pub high_pass: (f64, f64),
    /// Discrete reflections: (ms, linear gain).
    pub reflections: Vec<(f64, f64)>,
    /// Broadband gain from sink full scale to mic dBFS at sink volume 1.0.
    pub gain_db: f64,
    /// Acoustic + latency delay in samples.
    pub delay: usize,
}

impl SimRoom {
    fn filters(&self) -> Vec<Biquad> {
        let mut f: Vec<Biquad> = self
            .resonances
            .iter()
            .map(|&(hz, db, q)| Biquad::peaking(RATE, hz, db, q))
            .collect();
        f.push(Biquad::high_pass(RATE, self.high_pass.0, self.high_pass.1));
        f
    }

    /// Analytic response (no reflections) in dB relative to `gain_db`.
    #[must_use]
    pub fn shape_db(&self, freq: f64) -> f64 {
        self.filters()
            .iter()
            .map(|b| b.magnitude_db(RATE, freq))
            .sum()
    }
}

/// The whole simulated setup.
#[derive(Debug, Clone)]
pub struct SimConfig {
    pub rooms: [SimRoom; 2],
    /// The simulated mic's own response (a high shelf), removed again by [`SimConfig::calibration`].
    pub mic_shelf: (f64, f64),
    pub sens_factor: f64,
    /// The mic's hiss, white, dBFS RMS.
    pub noise_dbfs: f64,
    /// The room's rumble (traffic, ventilation): noise falling 12 dB per octave above
    /// 60 Hz, set by the level of the white noise it is made from, dBFS RMS. About 38 dB SPL
    /// in the 63 Hz octave: a real room with some traffic or ventilation.
    pub rumble_dbfs: f64,
    /// 1.0 = real time; larger values run measurements faster.
    pub speed: f64,
    /// A real room behind the sound ([`Scene`]): when set, the bass and the reflections come
    /// from its size and where the speakers and the mic are, and `rooms` describe only the
    /// speakers themselves.
    pub scene: Option<Scene>,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            rooms: [
                SimRoom {
                    resonances: vec![
                        // The room's length mode, lightly damped like a real room's: it rings
                        // for about 0.8 s (a peaking filter's poles give T60 = 2.2·Q·A / f,
                        // A = 10^(gain/40)).
                        (33.0, 10.0, 7.0),
                        (48.0, 6.0, 5.0),
                        (97.0, 8.0, 12.0),
                        (122.0, 10.0, 6.0),
                        (181.0, 5.0, 6.0),
                        (226.0, -7.0, 6.0),
                        (650.0, 3.0, 4.0),
                        (2500.0, 3.5, 2.5),
                        (12_000.0, 4.0, 1.0),
                    ],
                    high_pass: (42.0, 0.8),
                    reflections: vec![(3.1, 0.25), (7.4, 0.15), (13.0, 0.1)],
                    gain_db: -24.0,
                    delay: 2400 + 140,
                },
                SimRoom {
                    resonances: vec![
                        (33.0, 9.0, 7.0),
                        (45.0, 7.0, 8.0),
                        (110.0, 6.0, 5.0),
                        (450.0, 5.0, 5.0),
                        (775.0, -4.0, 4.0),
                        (1026.0, 3.0, 2.0),
                        (5000.0, -2.5, 1.5),
                    ],
                    high_pass: (40.0, 0.8),
                    reflections: vec![(2.7, 0.22), (8.9, 0.12)],
                    gain_db: -22.5,
                    delay: 2400 + 170,
                },
            ],
            mic_shelf: (6000.0, 2.0),
            sens_factor: -0.3,
            noise_dbfs: -95.0,
            rumble_dbfs: -52.0,
            speed: 1.0,
            scene: None,
        }
    }
}

impl SimConfig {
    /// The speakers of [`SimConfig::default`] in a real room ([`Scene::living_room`]): moving
    /// the mic or the speakers changes the bass as it would at home, and the room screen can
    /// be checked against a room whose size and layout are known.
    #[must_use]
    pub fn living_room() -> Self {
        let mut config = Self::default();
        for room in &mut config.rooms {
            // The speakers' own character only: the room adds its resonances and reflections.
            room.resonances.retain(|&(hz, _, _)| hz > 300.0);
            room.reflections.clear();
        }
        config.scene = Some(Scene::living_room());
        config
    }

    /// Calibration file describing the simulated mic.
    #[must_use]
    pub fn calibration(&self) -> Calibration {
        let shelf = Biquad::high_shelf(RATE, self.mic_shelf.0, self.mic_shelf.1);
        let points: Vec<(f64, f64)> = (0..=616)
            .map(|i| {
                let f = 10.0 * 2000f64.powf(f64::from(i) / 616.0);
                (f, shelf.magnitude_db(RATE, f))
            })
            .collect();
        Calibration::from_points(self.sens_factor, &points)
    }
}

/// Mutable simulated system state shared with [`crate::platform::sim`].
#[derive(Debug, Clone)]
pub struct SimState {
    /// Output volume, 1.0 being unity (on Linux, wpctl's cubic scale).
    pub sink_volume: f64,
    /// EQ loaded into the simulated EQ host, applied to [`Route::ThroughEq`] playback.
    pub eq: Option<EqSet>,
    /// Distance from the speakers to the wall behind them, m, once the placement screen
    /// has moved them; its bass reflection cancels around a quarter wavelength.
    pub wall_m: Option<f64>,
    /// Which mic position is being measured (each added position moves the mic, see
    /// [`crate::audio::scene::MIC_OFFSETS`]); only a [`Scene`] hears the difference.
    pub mic_position: usize,
    /// False until the simulated mic is plugged in.
    pub mic_plugged: bool,
    /// Outputs besides the speakers, `(name, description)`: the mic hardly hears them, as it
    /// would not hear a laptop's own speakers across the room.
    pub other_outputs: Vec<(String, String)>,
    /// The system's default output, when not the speakers.
    pub default_output: Option<String>,
    /// Streams started so far: each gets its own noise, the same on every run.
    pub streams: u64,
}

impl Default for SimState {
    fn default() -> Self {
        Self {
            sink_volume: 0.8,
            eq: None,
            wall_m: None,
            mic_position: 0,
            mic_plugged: true,
            other_outputs: Vec::new(),
            default_output: None,
            streams: 0,
        }
    }
}

/// Distance from the speaker to the mic in the simulated room, m (for the wall reflection's
/// level).
const LISTENING_M: f64 = 1.0;
/// Above this the speakers beam forward and the wall behind them hardly reflects, Hz.
const WALL_LOW_PASS_HZ: f64 = 250.0;

/// The wall reflection for a speaker `d` metres from it: (delay in samples, gain).
fn wall_reflection(d: f64) -> (usize, f64) {
    let extra = 2.0 * d;
    (
        (extra / crate::dsp::room::SPEED_OF_SOUND * RATE).round() as usize,
        LISTENING_M / (LISTENING_M + extra),
    )
}

/// Renders audio through the simulated room.
#[derive(Debug, Clone)]
pub struct SimBackend {
    config: Arc<SimConfig>,
    state: Arc<Mutex<SimState>>,
}

impl SimBackend {
    #[must_use]
    pub fn new(config: SimConfig, state: Arc<Mutex<SimState>>) -> Self {
        Self {
            config: Arc::new(config),
            state,
        }
    }

    /// Renders a whole `Once` source offline (no threads, no pacing), for tests.
    #[must_use]
    pub fn render_offline(&self, mut source: Source, route: Route) -> Vec<f32> {
        let total = source.total_frames().unwrap_or(SAMPLE_RATE as usize);
        let mut chain = Chain::new(&self.config, &self.state, source.channel, route, 99);
        let mut mono = vec![0f32; total];
        source.fill_mono(&mut mono);
        chain.process(&mut mono);
        mono
    }
}

/// Per-stream processing state.
struct Chain {
    eq: Vec<Biquad>,
    eq_state: Vec<BiquadState>,
    eq_gain: f64,
    room: Vec<Biquad>,
    room_state: Vec<BiquadState>,
    reflections: Vec<(usize, f64)>,
    /// The real room's path, when there is a [`Scene`]: then the fixed reflections and the
    /// wall below are not used.
    scene: Option<ScenePath>,
    /// Where the scene's path was worked out for: the wall distance and the mic position.
    scene_at: (Option<f64>, usize),
    /// Room output before reflections, for the reflection taps.
    dry: Vec<f64>,
    /// Room output with reflections, for the propagation delay.
    wet: Vec<f64>,
    /// The wall behind the speaker: low-passed room output, delay and gain.
    wall: Option<(Biquad, BiquadState, usize, f64)>,
    bass: Vec<f64>,
    pos: usize,
    delay: usize,
    mic: Biquad,
    mic_state: BiquadState,
    gain: f64,
    noise: f64,
    /// Rumble: input level, the two one-pole low-passes' coefficient and states.
    rumble: (f64, f64, f64, f64),
    rng: Rng,
}

const HISTORY: usize = 1 << 16;
/// Samples made at a time.
const BLOCK: usize = 1024;

impl Chain {
    fn new(
        config: &SimConfig,
        state: &Mutex<SimState>,
        channel: Channel,
        route: Route,
        seed: u64,
    ) -> Self {
        let room = &config.rooms[channel.index()];
        let st = state.lock().expect("sim state lock poisoned").clone();
        let (eq, eq_gain) = match (&st.eq, route) {
            (Some(set), Route::ThroughEq) => (
                set.channels[channel.index()]
                    .iter()
                    .map(|f| Biquad::peaking(RATE, f.freq, f.gain, f.q))
                    .collect(),
                db_to_amplitude(set.preamp_db + set.trim_db[channel.index()]),
            ),
            _ => (Vec::new(), 1.0),
        };
        let filters = room.filters();
        let scene = config
            .scene
            .as_ref()
            .map(|s| s.path(channel.index(), st.wall_m, st.mic_position));
        Self {
            scene,
            scene_at: (st.wall_m, st.mic_position),
            eq_state: vec![BiquadState::default(); eq.len()],
            eq,
            eq_gain,
            room_state: vec![BiquadState::default(); filters.len()],
            room: filters,
            reflections: room
                .reflections
                .iter()
                .map(|&(ms, g)| ((ms * RATE / 1000.0) as usize, g))
                .collect(),
            dry: vec![0.0; HISTORY],
            wet: vec![0.0; HISTORY],
            wall: st.wall_m.map(|d| {
                let (delay, gain) = wall_reflection(d);
                (
                    Biquad::low_pass(RATE, WALL_LOW_PASS_HZ, std::f64::consts::FRAC_1_SQRT_2),
                    BiquadState::default(),
                    delay,
                    gain,
                )
            }),
            bass: vec![0.0; HISTORY],
            pos: 0,
            delay: room.delay,
            mic: Biquad::high_shelf(RATE, config.mic_shelf.0, config.mic_shelf.1),
            mic_state: BiquadState::default(),
            gain: db_to_amplitude(room.gain_db) * st.sink_volume.powi(3),
            noise: db_to_amplitude(config.noise_dbfs) / 2f64.sqrt(),
            rumble: (
                db_to_amplitude(config.rumble_dbfs),
                1.0 - (-std::f64::consts::TAU * 60.0 / RATE).exp(),
                0.0,
                0.0,
            ),
            rng: Rng::new(seed),
        }
    }

    /// Moves the wall behind the speaker (or removes it), keeping the sound already in
    /// flight: a real room changes while the music plays.
    fn set_wall(&mut self, wall_m: Option<f64>, scene: Option<(&Scene, usize, usize)>) {
        if let (Some(path), Some((s, channel, position))) = (&mut self.scene, scene) {
            if self.scene_at != (wall_m, position) {
                path.move_to(s.path(channel, wall_m, position));
                self.scene_at = (wall_m, position);
            }
            return;
        }
        match (wall_m, &mut self.wall) {
            (Some(d), Some((_, _, delay, gain))) => (*delay, *gain) = wall_reflection(d),
            (Some(d), None) => {
                let (delay, gain) = wall_reflection(d);
                self.wall = Some((
                    Biquad::low_pass(RATE, WALL_LOW_PASS_HZ, std::f64::consts::FRAC_1_SQRT_2),
                    BiquadState::default(),
                    delay,
                    gain,
                ));
            }
            (None, _) => self.wall = None,
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        for v in buf.iter_mut() {
            let mut x = f64::from(*v) * self.eq_gain;
            for (b, s) in self.eq.iter().zip(&mut self.eq_state) {
                x = b.tick(s, x);
            }
            for (b, s) in self.room.iter().zip(&mut self.room_state) {
                x = b.tick(s, x);
            }
            let at = |back: usize| (self.pos + HISTORY - back) % HISTORY;
            let y = if let Some(path) = &mut self.scene {
                path.tick(x)
            } else {
                self.dry[at(0)] = x;
                let mut y = x;
                for &(d, g) in &self.reflections {
                    y += g * self.dry[at(d)];
                }
                if let Some((lp, state, delay, gain)) = &mut self.wall {
                    self.bass[at(0)] = lp.tick(state, x);
                    y += *gain * self.bass[at(*delay)];
                }
                y
            };
            self.wet[at(0)] = y;
            let delayed = self.wet[at(self.delay)];
            self.pos += 1;
            let z = self.mic.tick(&mut self.mic_state, delayed) * self.gain;
            let (level, a, y1, y2) = &mut self.rumble;
            *y1 += *a * (*level * self.rng.next_gaussian() - *y1);
            *y2 += *a * (*y1 - *y2);
            *v = (z + *y2 + self.noise * self.rng.next_gaussian()) as f32;
        }
    }
}

impl AudioBackend for SimBackend {
    fn start(&mut self, mut source: Source, route: Route) -> Result<Stream> {
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let config = Arc::clone(&self.config);
        let state = Arc::clone(&self.state);
        let finished2 = Arc::clone(&finished);
        // Each stream's noise comes from its number: the same on every run.
        let seed = {
            let mut st = self.state.lock().expect("sim state lock poisoned");
            st.streams += 1;
            st.streams ^ 0x5eed
        };
        // Set up with the stream's first block, not here: the room's paths take a while.
        let mut chain: Option<Chain> = None;
        let every = Duration::from_secs_f64(BLOCK as f64 / RATE / config.speed);
        let rx = crate::background::paced(every, Arc::clone(&stop), move || {
            let mut buf = vec![0f32; BLOCK];
            source.fill_mono(&mut buf);
            // Volume and wall changes apply live, as in a real room.
            let (vol, wall_m, position) = {
                let st = state.lock().expect("sim state lock poisoned");
                (st.sink_volume, st.wall_m, st.mic_position)
            };
            let room_gain = db_to_amplitude(config.rooms[source.channel.index()].gain_db);
            let chain = chain
                .get_or_insert_with(|| Chain::new(&config, &state, source.channel, route, seed));
            chain.gain = room_gain * vol.powi(3);
            let scene = config
                .scene
                .as_ref()
                .map(|s| (s, source.channel.index(), position));
            chain.set_wall(wall_m, scene);
            chain.process(&mut buf);
            if source.is_done() {
                finished2.store(true, Ordering::Release);
            }
            buf
        });
        Ok(Stream::new(
            rx,
            finished,
            Box::new(move || stop.store(true, Ordering::Release)),
        ))
    }
}
