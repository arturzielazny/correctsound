//! A real room for the simulator ([`super::sim`]): the sound from each speaker to the mic
//! worked out from the room's size and where the speakers and the mic are, so moving any of
//! them changes the bass the way it would at home.
//!
//! Below the crossover the room is the modal sum of [`crate::dsp::roommodel`] (the same
//! physics the room screen fits), played as one band-pass filter per standing wave plus the
//! closed room's pressure rise; above it, the direct sound and the first reflection off each
//! of the six walls. The model's response at frequency `f`,
//! `−1/f + Σ dₙ·f / (fₙ² − f² + j·f·fₙ/Q)`, is (up to a constant) the Laplace form
//! `1/s + Σ dₙ·s / (s² + s·ωₙ/Q + ωₙ²)`, and each term there is a band-pass filter scaled by
//! `Q/ωₙ`, so the filters reproduce the model exactly, ringing included.

use std::f64::consts::{LN_10, PI, TAU};

use realfft::num_complex::Complex;

use crate::dsp::SAMPLE_RATE;
use crate::dsp::biquad::{Biquad, BiquadState};
use crate::dsp::room::SPEED_OF_SOUND;
use crate::dsp::roommodel::{Model, Point, Room, Setup};

const RATE: f64 = SAMPLE_RATE as f64;
/// Where the standing waves hand over to the direct sound and its reflections, Hz.
const CROSSOVER_HZ: f64 = 150.0;
/// Below this the closed room's pressure rise levels off, Hz (a real room leaks).
const LEAK_HZ: f64 = 5.0;
/// The band in which the standing waves are levelled against the direct sound, Hz.
const MATCH_BAND: (f64, f64) = (110.0, 190.0);
/// Longest reflection delay kept, samples (a wall 20 m away).
const HISTORY: usize = 1 << 13;
/// Where the mic is for each measured position, m from the seat (towards the back of the
/// room, towards the right wall): each added position moves it 25 cm, as the app asks.
pub const MIC_OFFSETS: [(f64, f64); 5] = [
    (0.0, 0.0),
    (0.0, 0.25),
    (0.25, 0.0),
    (0.0, -0.25),
    (-0.25, 0.0),
];

/// A room with speakers and a seat in it, and how it rings.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    pub setup: Setup,
    /// How long the standing waves ring, s, unless listed in `ringing`.
    pub t60_s: f64,
    /// Standing waves that ring for a different time: (Hz, T60 s), each given to the room's
    /// standing wave nearest that frequency.
    pub ringing: Vec<(f64, f64)>,
    /// Share of the sound pressure a wall reflects, 0 to 1.
    pub reflectance: f64,
}

impl Scene {
    /// A living room like the one CorrectSound was first tried in: 5.64 m front to back,
    /// 5.24 m wide, 2.7 m high, the speakers 1.13 m apart and 0.75 m out from the wall
    /// behind them, the right one 0.7 m from the right-hand wall, the seat completing an
    /// equilateral triangle. Its lowest standing waves ring long, as that room's did.
    #[must_use]
    pub fn living_room() -> Self {
        let room = Room {
            length: 5.64,
            width: 5.24,
            height: 2.7,
        };
        let (x, right) = (0.75, room.width - 0.7);
        let left = right - 1.13;
        Self {
            setup: Setup {
                room,
                speakers: [
                    Point { x, y: left, z: 1.0 },
                    Point {
                        x,
                        y: right,
                        z: 1.0,
                    },
                ],
                seat: Point {
                    x: x + 1.13 * 3f64.sqrt() / 2.0,
                    y: (left + right) / 2.0,
                    z: 1.1,
                },
            },
            t60_s: 0.5,
            ringing: vec![(32.7, 1.6), (44.6, 1.5), (30.4, 1.0)],
            reflectance: 0.6,
        }
    }

    /// The standing waves and how long each rings.
    fn model(&self) -> Model {
        let mut model = Model::new(self.setup.room, &[], self.t60_s);
        for &(hz, t60) in &self.ringing {
            let nearest = model
                .modes
                .iter()
                .enumerate()
                .min_by(|a, b| (a.1.hz - hz).abs().total_cmp(&(b.1.hz - hz).abs()))
                .map(|(i, _)| i);
            if let Some(i) = nearest {
                model.t60_s[i] = t60;
            }
        }
        model
    }

    /// Where speaker `channel` (0 left, 1 right) is: `wall_m` from the wall behind it when
    /// the placement screen has moved the speakers, else where the setup has it.
    #[must_use]
    pub fn speaker(&self, channel: usize, wall_m: Option<f64>) -> Point {
        let mut s = self.setup.speakers[channel];
        if let Some(d) = wall_m {
            s.x = d.clamp(0.05, self.setup.room.length - 0.05);
        }
        s
    }

    /// Where the mic is at measured position `position` (counting from 0).
    #[must_use]
    pub fn mic(&self, position: usize) -> Point {
        let (dx, dy) = MIC_OFFSETS[position % MIC_OFFSETS.len()];
        let room = self.setup.room;
        Point {
            x: (self.setup.seat.x + dx).clamp(0.05, room.length - 0.05),
            y: (self.setup.seat.y + dy).clamp(0.05, room.width - 0.05),
            z: self.setup.seat.z,
        }
    }

    /// The path from speaker `channel` to the mic at `position`, the speakers `wall_m` from
    /// the wall behind them when moved.
    #[must_use]
    pub fn path(&self, channel: usize, wall_m: Option<f64>, position: usize) -> ScenePath {
        let model = self.model();
        let room = self.setup.room;
        let (source, mic) = (self.speaker(channel, wall_m), self.mic(position));
        // Each standing wave: how hard this speaker drives it where the mic is, and its Q.
        let waves: Vec<(f64, f64, f64)> = model
            .modes
            .iter()
            .zip(&model.t60_s)
            .map(|(m, &t60)| {
                let drive = m.weight() * m.shape(&room, source) * m.shape(&room, mic);
                (m.hz, PI * m.hz * t60 / (3.0 * LN_10), drive)
            })
            .collect();
        let leak = TAU * LEAK_HZ;
        // The bass in its Laplace form, for levelling it against the direct sound.
        let bass_at = |f: f64| {
            let s = Complex::new(0.0, TAU * f);
            let mut h = 1.0 / (s + leak);
            for &(hz, q, d) in &waves {
                let w = TAU * hz;
                h += d * s / (s * s + s * w / q + w * w);
            }
            h.norm()
        };
        // The direct sound and the six first reflections, by mirroring the speaker in each
        // wall.
        let distance = |a: Point, b: Point| {
            ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
        };
        let direct = distance(source, mic).max(0.1);
        let images = [
            Point {
                x: -source.x,
                ..source
            },
            Point {
                x: 2.0 * room.length - source.x,
                ..source
            },
            Point {
                y: -source.y,
                ..source
            },
            Point {
                y: 2.0 * room.width - source.y,
                ..source
            },
            Point {
                z: -source.z,
                ..source
            },
            Point {
                z: 2.0 * room.height - source.z,
                ..source
            },
        ];
        let taps: Vec<(usize, f64)> = images
            .iter()
            .map(|&image| {
                let r = distance(image, mic);
                let delay = ((r - direct) / SPEED_OF_SOUND * RATE).round() as usize;
                (delay.min(HISTORY - 1), self.reflectance * direct / r)
            })
            .collect();
        let treble_level = (1.0 + taps.iter().map(|t| t.1 * t.1).sum::<f64>()).sqrt();
        let band: Vec<f64> = (0..=16)
            .map(|k| MATCH_BAND.0 * (MATCH_BAND.1 / MATCH_BAND.0).powf(f64::from(k) / 16.0))
            .collect();
        let bass_level =
            (band.iter().map(|&f| bass_at(f).powi(2)).sum::<f64>() / band.len() as f64).sqrt();
        let scale = treble_level / bass_level.max(1e-30);
        let lr = std::f64::consts::FRAC_1_SQRT_2;
        ScenePath {
            low: [
                Biquad::low_pass(RATE, CROSSOVER_HZ, lr),
                Biquad::low_pass(RATE, CROSSOVER_HZ, lr),
            ],
            low_state: [BiquadState::default(); 2],
            high: [
                Biquad::high_pass(RATE, CROSSOVER_HZ, lr),
                Biquad::high_pass(RATE, CROSSOVER_HZ, lr),
            ],
            high_state: [BiquadState::default(); 2],
            waves: waves
                .iter()
                .map(|&(hz, q, d)| (Biquad::band_pass(RATE, hz, q), d * q / (TAU * hz)))
                .collect(),
            wave_state: vec![BiquadState::default(); waves.len()],
            leak: (1.0 - (-leak / RATE).exp(), 1.0 / leak),
            leak_state: 0.0,
            scale,
            taps,
            history: vec![0.0; HISTORY],
            pos: 0,
        }
    }
}

/// One speaker's sound on its way to the mic, sample by sample.
#[derive(Debug, Clone)]
pub struct ScenePath {
    /// The crossover: two second-order sections each way (Linkwitz–Riley, 24 dB/octave), so
    /// the two halves add up flat.
    low: [Biquad; 2],
    low_state: [BiquadState; 2],
    high: [Biquad; 2],
    high_state: [BiquadState; 2],
    /// One band-pass filter per standing wave, and its gain.
    waves: Vec<(Biquad, f64)>,
    wave_state: Vec<BiquadState>,
    /// The closed room's pressure rise: a one-pole low-pass's coefficient and gain.
    leak: (f64, f64),
    leak_state: f64,
    /// Brings the standing waves to the level of the direct sound at the crossover.
    scale: f64,
    /// The six first reflections: delay after the direct sound (samples) and gain.
    taps: Vec<(usize, f64)>,
    history: Vec<f64>,
    pos: usize,
}

impl ScenePath {
    /// The sound at the mic for the speaker's output `x`.
    pub fn tick(&mut self, x: f64) -> f64 {
        let mut low = x;
        for (f, s) in self.low.iter().zip(&mut self.low_state) {
            low = f.tick(s, low);
        }
        self.leak_state += self.leak.0 * (low - self.leak_state);
        let mut bass = self.leak.1 * self.leak_state;
        for ((f, g), s) in self.waves.iter().zip(&mut self.wave_state) {
            bass += g * f.tick(s, low);
        }
        let mut high = x;
        for (f, s) in self.high.iter().zip(&mut self.high_state) {
            high = f.tick(s, high);
        }
        self.history[self.pos] = high;
        let mut treble = high;
        for &(delay, gain) in &self.taps {
            treble += gain * self.history[(self.pos + HISTORY - delay) % HISTORY];
        }
        self.pos = (self.pos + 1) % HISTORY;
        self.scale * bass + treble
    }

    /// Takes `moved`'s positions (new coefficients and delays) and keeps the sound already
    /// in flight, as a room does when a speaker is moved while it plays.
    pub fn move_to(&mut self, moved: Self) {
        self.waves = moved.waves;
        self.leak = moved.leak;
        self.scale = moved.scale;
        self.taps = moved.taps;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::spectrum::{Smoothing, smooth_power};

    /// The path's response, from its impulse response: (Hz, dB) at 1/24 octave.
    fn response(path: &mut ScenePath) -> crate::dsp::spectrum::Response {
        let n = 1 << 18;
        let ir: Vec<f64> = (0..n)
            .map(|i| path.tick(f64::from(u8::from(i == 0))))
            .collect();
        let mut planner = realfft::RealFftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(n);
        let mut input = ir;
        let mut out = fft.make_output_vec();
        fft.process(&mut input, &mut out).expect("sizes match");
        let power: Vec<f64> = out.iter().map(Complex::norm_sqr).collect();
        smooth_power(&power, RATE / n as f64, Smoothing::Fixed(1.0 / 24.0))
    }

    fn db_at(r: &crate::dsp::spectrum::Response, hz: f64) -> f64 {
        let i = r.freqs.partition_point(|&f| f < hz);
        r.db[i]
    }

    #[test]
    fn the_bass_follows_the_room_model() {
        let scene = Scene::living_room();
        let mut path = scene.path(1, None, 0);
        let got = response(&mut path);
        // The model's own shape over the bass (same seat, one point, no averaging).
        let model = scene.model();
        let freqs: Vec<f64> = (0..40)
            .map(|k| 25.0 * 4f64.powf(f64::from(k) / 40.0))
            .collect();
        let want: Vec<f64> = model
            .response(&[scene.speaker(1, None)], scene.mic(0), &freqs)
            .iter()
            .map(|h| 20.0 * h.norm().log10())
            .collect();
        let have: Vec<f64> = freqs.iter().map(|&f| db_at(&got, f)).collect();
        // Compared as shapes (the level is set against the direct sound), below the
        // crossover where the model alone decides.
        let (dw, dh): (Vec<f64>, Vec<f64>) = freqs
            .iter()
            .zip(want.iter().zip(&have))
            .filter(|(f, _)| **f < 90.0)
            .map(|(_, (w, h))| (*w, *h))
            .unzip();
        let offset = dh.iter().zip(&dw).map(|(h, w)| h - w).sum::<f64>() / dw.len() as f64;
        let rms = (dh
            .iter()
            .zip(&dw)
            .map(|(h, w)| (h - w - offset).powi(2))
            .sum::<f64>()
            / dw.len() as f64)
            .sqrt();
        assert!(
            rms < 1.2,
            "the simulator's bass differs from the model by {rms:.2} dB"
        );
    }

    #[test]
    fn moving_the_mic_or_a_speaker_changes_the_bass() {
        let scene = Scene::living_room();
        let at = |channel, wall_m, position| response(&mut scene.path(channel, wall_m, position));
        let shape = |r: &crate::dsp::spectrum::Response| -> Vec<f64> {
            (0..30)
                .map(|k| db_at(r, 25.0 * 4f64.powf(f64::from(k) / 30.0)))
                .collect()
        };
        let difference = |a: &[f64], b: &[f64]| {
            let d: Vec<f64> = a.iter().zip(b).map(|(x, y)| x - y).collect();
            let mean = d.iter().sum::<f64>() / d.len() as f64;
            (d.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / d.len() as f64).sqrt()
        };
        let here = shape(&at(1, None, 0));
        let mic_moved = shape(&at(1, None, 1));
        let speaker_moved = shape(&at(1, Some(1.2), 0));
        assert!(difference(&here, &mic_moved) > 1.0, "25 cm of mic movement");
        assert!(
            difference(&here, &speaker_moved) > 3.0,
            "speaker 45 cm further out"
        );
    }

    #[test]
    fn the_long_ringing_waves_ring_long() {
        let model = Scene::living_room().model();
        let at = |hz: f64| {
            let i = model
                .modes
                .iter()
                .position(|m| (m.hz - hz).abs() < 0.5)
                .expect("a wave there");
            model.t60_s[i]
        };
        assert_eq!(at(32.7), 1.6);
        assert_eq!(at(44.6), 1.5);
        assert_eq!(at(63.5), 0.5);
    }
}
