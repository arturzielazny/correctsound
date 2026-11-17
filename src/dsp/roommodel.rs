//! A model of the room's bass: the room as a rectangular box whose walls reflect the bass,
//! its standing waves (modes), and what they do at any seat for speakers anywhere.
//!
//! Every mode of a box `length × width × height` is a pattern of loud and silent places,
//! `cos(l·π·x/length)·cos(m·π·y/width)·cos(n·π·z/height)`, ringing at
//! `343/2 · √((l/length)² + (m/width)² + (n/height)²)` Hz. A speaker drives a mode as hard
//! as the pattern is loud where the speaker is, and the seat hears it as loud as it is
//! where the seat is; the bass at the seat is the sum over all modes (Kuttruff, Room
//! Acoustics, ch. 3). How long each mode rings comes from the measurement where it was
//! measured ([`super::modes`]), and from the room's reverberation otherwise.
//!
//! Real rooms are not rigid boxes (windows, plasterboard, doors and furniture soften the
//! patterns), so the model is only trusted as far as it reproduces the measurement: on a
//! real living room it did so within about 3 dB below 60 Hz, where the lowest and
//! longest-ringing modes are, and poorly above about 70 Hz, where modes crowd together and
//! centimetres decide. [`agreement_db`] says how well it does in a given room.

use realfft::num_complex::Complex;
use serde::{Deserialize, Serialize};

use super::modes::{BassSpectrum, Mode};
use super::room::SPEED_OF_SOUND;
use super::spectrum::Response;

/// Modes up to this frequency are summed, Hz: more than twice the highest frequency
/// evaluated, so the response there is not cut short.
const MODEL_TOP_HZ: f64 = 200.0;
/// The model is compared with the measurement up to this frequency, Hz.
pub const TRUSTED_TO_HZ: f64 = 60.0;
/// The lowest frequency evaluated, Hz.
pub const FROM_HZ: f64 = 25.0;
/// The seat map and the suggestions judge the bass up to this frequency, Hz: the band the
/// lowest modes rule, a little above what the comparison checks.
pub const JUDGED_TO_HZ: f64 = 80.0;
/// Points per octave of the model's frequency grid.
const POINTS_PER_OCTAVE: f64 = 48.0;
/// A measured mode is matched to a model mode this close in frequency, as a ratio.
const MATCH_TOLERANCE: f64 = 0.04;
/// The receiver is averaged over a square this far to each side, m: the measurement
/// averages several mic positions, which fills the deepest single-point dips.
pub const SEAT_SPREAD_M: f64 = 0.2;

/// The room's inside dimensions, m: `length` front to back (from the wall behind the
/// speakers), `width` side to side, `height` floor to ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub length: f64,
    pub width: f64,
    pub height: f64,
}

/// A place in the room, m: `x` from the wall behind the speakers, `y` from the wall on the
/// left (seen from the seat), `z` above the floor.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// The room, where the speakers are (left, right) and where the listener sits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    pub room: Room,
    pub speakers: [Point; 2],
    pub seat: Point,
}

/// How far anything stays from a wall, m.
const WALL_GAP: f64 = 0.05;

impl Setup {
    /// The usual layout in `room`: speakers 1.2 m apart (less in a narrow room), 0.5 m out
    /// from the wall behind them and centred on it, the seat completing an equilateral
    /// triangle.
    #[must_use]
    pub fn typical(room: Room) -> Self {
        let spacing = (room.width * 0.4).min(1.2);
        let x = 0.5_f64.min(room.length / 4.0);
        let mid = room.width / 2.0;
        let mut setup = Self {
            room,
            speakers: [
                Point {
                    x,
                    y: mid - spacing / 2.0,
                    z: 1.0,
                },
                Point {
                    x,
                    y: mid + spacing / 2.0,
                    z: 1.0,
                },
            ],
            seat: Point {
                x: x + spacing * 3f64.sqrt() / 2.0,
                y: mid,
                z: 1.1,
            },
        };
        setup.keep_inside();
        setup
    }

    /// Moves everything inside the room, a little away from the walls.
    pub fn keep_inside(&mut self) {
        let r = self.room;
        let inside = |p: &mut Point| {
            p.x = p.x.clamp(WALL_GAP, (r.length - WALL_GAP).max(WALL_GAP));
            p.y = p.y.clamp(WALL_GAP, (r.width - WALL_GAP).max(WALL_GAP));
            p.z = p.z.clamp(WALL_GAP, (r.height - WALL_GAP).max(WALL_GAP));
        };
        for p in &mut self.speakers {
            inside(p);
        }
        inside(&mut self.seat);
    }
}

/// One standing wave of the room.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoomMode {
    /// How many half-waves fit front to back, side to side and floor to ceiling.
    pub order: [u8; 3],
    pub hz: f64,
}

/// What kind of standing wave a mode is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Between two facing walls: the strongest.
    Axial,
    /// Around four walls.
    Tangential,
    /// Around all six.
    Oblique,
}

impl Room {
    /// The length of axis `i`: 0 length, 1 width, 2 height.
    fn axis(&self, i: usize) -> f64 {
        [self.length, self.width, self.height][i]
    }

    /// True when the dimensions are plausible for a room (1.5–30 m, 1.8–10 m high).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        (1.5..=30.0).contains(&self.length)
            && (1.5..=30.0).contains(&self.width)
            && (1.8..=10.0).contains(&self.height)
    }

    /// The frequency of the mode `order`, Hz.
    #[must_use]
    pub fn mode_hz(&self, order: [u8; 3]) -> f64 {
        let s: f64 = (0..3)
            .map(|i| (f64::from(order[i]) / self.axis(i)).powi(2))
            .sum();
        SPEED_OF_SOUND / 2.0 * s.sqrt()
    }

    /// Every mode up to `top_hz`, lowest first.
    #[must_use]
    pub fn modes(&self, top_hz: f64) -> Vec<RoomMode> {
        let most = |len: f64| (2.0 * len * top_hz / SPEED_OF_SOUND) as u8 + 1;
        let mut out = Vec::new();
        for l in 0..=most(self.length) {
            for m in 0..=most(self.width) {
                for n in 0..=most(self.height) {
                    let order = [l, m, n];
                    let hz = self.mode_hz(order);
                    if order != [0, 0, 0] && hz <= top_hz {
                        out.push(RoomMode { order, hz });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.hz.total_cmp(&b.hz));
        out
    }
}

impl RoomMode {
    /// How loud the mode is at `p`, −1 to 1 (the sign is its phase).
    #[must_use]
    pub fn shape(&self, room: &Room, p: Point) -> f64 {
        let at = [p.x, p.y, p.z];
        (0..3)
            .map(|i| (f64::from(self.order[i]) * std::f64::consts::PI * at[i] / room.axis(i)).cos())
            .product()
    }

    #[must_use]
    pub fn kind(&self) -> Kind {
        match self.order.iter().filter(|&&n| n > 0).count() {
            1 => Kind::Axial,
            2 => Kind::Tangential,
            _ => Kind::Oblique,
        }
    }

    /// "(0,1,0)".
    #[must_use]
    pub fn label(&self) -> String {
        format!("({},{},{})", self.order[0], self.order[1], self.order[2])
    }

    /// Where it stands, in words: "side to side (5.24 m)", "front to back and side to
    /// side", "around the whole room".
    #[must_use]
    pub fn describe(&self, room: &Room) -> String {
        let names = ["front to back", "side to side", "floor to ceiling"];
        let axes: Vec<usize> = (0..3).filter(|&i| self.order[i] > 0).collect();
        match axes.as_slice() {
            [i] => {
                let n = self.order[*i];
                let harmonic = match n {
                    1 => String::new(),
                    2 => ", 2nd".to_owned(),
                    3 => ", 3rd".to_owned(),
                    n => format!(", {n}th"),
                };
                format!("{} ({:.2} m{harmonic})", names[*i], room.axis(*i))
            }
            [a, b] => format!("{} and {}", names[*a], names[*b]),
            _ => "around the whole room".to_owned(),
        }
    }

    /// Weight of the mode in the modal sum: modes with more non-zero orders have a smaller
    /// normalisation integral (`1/2` per non-zero order), so a larger weight.
    #[must_use]
    pub fn weight(&self) -> f64 {
        f64::from(1u32 << self.order.iter().filter(|&&n| n > 0).count())
    }
}

/// The model: the room's modes with how long each rings.
#[derive(Debug, Clone)]
pub struct Model {
    pub room: Room,
    pub modes: Vec<RoomMode>,
    /// T60 of each mode, s.
    pub t60_s: Vec<f64>,
    /// True for the modes whose T60 was measured.
    pub measured: Vec<bool>,
}

impl Model {
    /// The model of `room`, with the measured modes' decay times and `typical_t60_s` for
    /// the others. Each measured mode goes to the model mode nearest in frequency, within
    /// 4 %.
    #[must_use]
    pub fn new(room: Room, measured_modes: &[Mode], typical_t60_s: f64) -> Self {
        let modes = room.modes(MODEL_TOP_HZ);
        let mut t60_s = vec![typical_t60_s; modes.len()];
        let mut measured = vec![false; modes.len()];
        for m in measured_modes {
            if let Some(i) = nearest(&modes, m.hz) {
                t60_s[i] = m.t60_s;
                measured[i] = true;
            }
        }
        Self {
            room,
            modes,
            t60_s,
            measured,
        }
    }

    /// The model mode a measured resonance at `hz` most likely is: of those within 4 %, the
    /// one the speakers drive hardest at the seat (the nearest when `setup` is not given).
    #[must_use]
    pub fn identify(&self, hz: f64, setup: Option<&Setup>) -> Option<RoomMode> {
        let candidates = self
            .modes
            .iter()
            .filter(|m| (m.hz / hz - 1.0).abs() < MATCH_TOLERANCE);
        match setup {
            Some(s) => candidates
                .max_by(|a, b| coupling(a, s).total_cmp(&coupling(b, s)))
                .copied(),
            None => candidates
                .min_by(|a, b| (a.hz - hz).abs().total_cmp(&(b.hz - hz).abs()))
                .copied(),
        }
    }

    /// The response at `receiver` of `sources` playing the same signal (bass is mono in most
    /// music), per frequency, for a source of constant volume velocity.
    #[must_use]
    pub fn response(&self, sources: &[Point], receiver: Point, freqs: &[f64]) -> Vec<Complex<f64>> {
        Evaluator::new(self, freqs).response(sources, receiver)
    }

    /// The level at `receiver`, dB (relative), averaged over a square `spread` m to each
    /// side of it at the same height.
    #[must_use]
    pub fn level_db(
        &self,
        sources: &[Point],
        receiver: Point,
        freqs: &[f64],
        spread: f64,
    ) -> Vec<f64> {
        Evaluator::new(self, freqs).level_db(sources, receiver, spread)
    }

    /// How uneven the bass below [`JUDGED_TO_HZ`] would be everywhere at ear height `z`
    /// with the speakers where they are: a grid of `step` m.
    #[must_use]
    pub fn seat_map(&self, setup: &Setup, step: f64) -> SeatMap {
        let r = self.room;
        let nx = ((r.length / step).floor() as usize).max(1);
        let ny = ((r.width / step).floor() as usize).max(1);
        let freqs = grid(FROM_HZ, JUDGED_TO_HZ);
        // Everything that does not depend on the seat, once.
        let drive: Vec<f64> = self
            .modes
            .iter()
            .map(|m| m.weight() * setup.speakers.iter().map(|&s| m.shape(&r, s)).sum::<f64>())
            .collect();
        let terms: Vec<Vec<Complex<f64>>> = freqs
            .iter()
            .map(|&f| {
                self.modes
                    .iter()
                    .zip(&self.t60_s)
                    .map(|(m, &t60)| f / resonance(m.hz, t60, f))
                    .collect()
            })
            .collect();
        let cos_table = |order: usize, len: f64, at: f64| {
            (0..=order)
                .map(|n| (n as f64 * std::f64::consts::PI * at / len).cos())
                .collect::<Vec<f64>>()
        };
        let max_order = |i: usize| {
            self.modes
                .iter()
                .map(|m| m.order[i] as usize)
                .max()
                .unwrap_or(0)
        };
        let cz = cos_table(max_order(2), r.height, setup.seat.z);
        // Each row of seats on its own thread, joined back in order.
        let row = |iy: usize| {
            let mut values = Vec::with_capacity(nx);
            let y = (iy as f64 + 0.5) * step;
            let cy = cos_table(max_order(1), r.width, y);
            for ix in 0..nx {
                let x = (ix as f64 + 0.5) * step;
                let cx = cos_table(max_order(0), r.length, x);
                let at_seat: Vec<f64> = self
                    .modes
                    .iter()
                    .zip(&drive)
                    .map(|(m, d)| {
                        d * cx[m.order[0] as usize]
                            * cy[m.order[1] as usize]
                            * cz[m.order[2] as usize]
                    })
                    .collect();
                let db: Vec<f64> = freqs
                    .iter()
                    .zip(&terms)
                    .map(|(&f, t)| {
                        let mut h = Complex::new(-(setup.speakers.len() as f64) / f, 0.0);
                        for (a, term) in at_seat.iter().zip(t) {
                            h += *a * term;
                        }
                        super::power_to_db(h.norm_sqr())
                    })
                    .collect();
                values.push(unevenness_db(&freqs, &db));
            }
            values
        };
        let values: Vec<f64> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..ny).map(|iy| scope.spawn(move || row(iy))).collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("seat map threads do not panic"))
                .collect()
        });
        SeatMap {
            nx,
            ny,
            step,
            values,
        }
    }
}

/// The model at a fixed set of frequencies, with each mode's resonance there worked out
/// once: evaluating many layouts then only sums.
struct Evaluator<'a> {
    model: &'a Model,
    freqs: Vec<f64>,
    /// Per frequency, per mode: `f / (fₙ² − f² + j·f·fₙ/Q)`.
    terms: Vec<Vec<Complex<f64>>>,
}

impl<'a> Evaluator<'a> {
    fn new(model: &'a Model, freqs: &[f64]) -> Self {
        let terms = freqs
            .iter()
            .map(|&f| {
                model
                    .modes
                    .iter()
                    .zip(&model.t60_s)
                    .map(|(m, &t60)| f / resonance(m.hz, t60, f))
                    .collect()
            })
            .collect();
        Self {
            model,
            freqs: freqs.to_vec(),
            terms,
        }
    }

    fn response(&self, sources: &[Point], receiver: Point) -> Vec<Complex<f64>> {
        let room = &self.model.room;
        let drive: Vec<f64> = self
            .model
            .modes
            .iter()
            .map(|m| {
                m.weight()
                    * m.shape(room, receiver)
                    * sources.iter().map(|&s| m.shape(room, s)).sum::<f64>()
            })
            .collect();
        self.freqs
            .iter()
            .zip(&self.terms)
            .map(|(&f, terms)| {
                // The room as a closed box (the mode of order 0: weight 1, the same
                // everywhere, `f / (0 − f²)`), then the modes.
                let mut h = Complex::new(-(sources.len() as f64) / f, 0.0);
                for (d, t) in drive.iter().zip(terms) {
                    h += *d * t;
                }
                h
            })
            .collect()
    }

    fn level_db(&self, sources: &[Point], receiver: Point, spread: f64) -> Vec<f64> {
        let room = self.model.room;
        let offsets: &[f64] = if spread > 0.0 {
            &[-1.0, 0.0, 1.0]
        } else {
            &[0.0]
        };
        let mut power = vec![0.0; self.freqs.len()];
        let mut n = 0.0;
        for dx in offsets {
            for dy in offsets {
                let r = Point {
                    x: (receiver.x + dx * spread).clamp(WALL_GAP, room.length - WALL_GAP),
                    y: (receiver.y + dy * spread).clamp(WALL_GAP, room.width - WALL_GAP),
                    z: receiver.z,
                };
                for (p, h) in power.iter_mut().zip(self.response(sources, r)) {
                    *p += h.norm_sqr();
                }
                n += 1.0;
            }
        }
        power.iter().map(|p| super::power_to_db(p / n)).collect()
    }
}

/// The denominator of one mode's resonance at `f`: `fₙ² − f² + j·f·fₙ/Q`, with `Q` from its
/// T60 (`Q = π·fₙ·T60 / 6.91`).
fn resonance(fn_hz: f64, t60: f64, f: f64) -> Complex<f64> {
    let q = std::f64::consts::PI * fn_hz * t60 / (3.0 * std::f64::consts::LN_10);
    Complex::new(fn_hz * fn_hz - f * f, f * fn_hz / q)
}

/// The index of the model mode nearest `hz`, within [`MATCH_TOLERANCE`].
fn nearest(modes: &[RoomMode], hz: f64) -> Option<usize> {
    modes
        .iter()
        .enumerate()
        .filter(|(_, m)| (m.hz / hz - 1.0).abs() < MATCH_TOLERANCE)
        .min_by(|a, b| (a.1.hz - hz).abs().total_cmp(&(b.1.hz - hz).abs()))
        .map(|(i, _)| i)
}

/// How hard the speakers drive `mode` at the seat: 0 to 1 per speaker, summed.
#[must_use]
pub fn coupling(mode: &RoomMode, setup: &Setup) -> f64 {
    setup
        .speakers
        .iter()
        .map(|&s| (mode.shape(&setup.room, s) * mode.shape(&setup.room, setup.seat)).abs())
        .sum()
}

/// How evenly the bass would be heard everywhere on the floor, as [`unevenness_db`] per
/// cell of a grid, row by row from the left wall (`y`), each from the wall behind the
/// speakers (`x`).
#[derive(Debug, Clone, PartialEq)]
pub struct SeatMap {
    pub nx: usize,
    pub ny: usize,
    /// Cell size, m.
    pub step: f64,
    pub values: Vec<f64>,
}

impl SeatMap {
    /// The value in the cell holding (`x`, `y`).
    #[must_use]
    pub fn at(&self, x: f64, y: f64) -> f64 {
        let ix = ((x / self.step) as usize).min(self.nx - 1);
        let iy = ((y / self.step) as usize).min(self.ny - 1);
        self.values[iy * self.nx + ix]
    }

    /// The most even cell within `radius` m of (`x`, `y`): its centre and value.
    #[must_use]
    pub fn best_near(&self, x: f64, y: f64, radius: f64) -> Option<(f64, f64, f64)> {
        (0..self.ny)
            .flat_map(|iy| (0..self.nx).map(move |ix| (ix, iy)))
            .map(|(ix, iy)| {
                let (cx, cy) = ((ix as f64 + 0.5) * self.step, (iy as f64 + 0.5) * self.step);
                (cx, cy, self.values[iy * self.nx + ix])
            })
            .filter(|(cx, cy, _)| (cx - x).hypot(cy - y) <= radius)
            .min_by(|a, b| a.2.total_cmp(&b.2))
    }
}

/// A log-spaced frequency grid from `from` to `to` Hz, [`POINTS_PER_OCTAVE`] per octave.
#[must_use]
pub fn grid(from: f64, to: f64) -> Vec<f64> {
    let n = ((to / from).log2() * POINTS_PER_OCTAVE).ceil() as usize;
    (0..=n)
        .map(|i| from * 2f64.powf(i as f64 / POINTS_PER_OCTAVE))
        .collect()
}

/// Moving average over `half` points to each side.
fn smooth(v: &[f64], half: usize) -> Vec<f64> {
    (0..v.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(v.len()));
            v[a..b].iter().sum::<f64>() / (b - a) as f64
        })
        .collect()
}

/// The peaks and dips of a response on a [`grid`], dB: smoothed to 1/12 octave, minus its
/// one-octave trend (which removes the speaker's own roll-off and the overall level).
fn ups_and_downs(db: &[f64]) -> Vec<f64> {
    let s = smooth(db, (POINTS_PER_OCTAVE / 24.0) as usize);
    let trend = smooth(&s, (POINTS_PER_OCTAVE / 2.0) as usize);
    s.iter().zip(&trend).map(|(a, b)| a - b).collect()
}

/// The model's peaks and dips (`model_db` on the [`grid`] `freqs`) laid over the measured
/// response's one-octave trend: the model as it would look if it also had the speaker's
/// roll-off, for drawing next to the measurement. What [`agreement_db`] compares.
#[must_use]
pub fn aligned(measured: &Response, freqs: &[f64], model_db: &[f64]) -> Vec<f64> {
    let m: Vec<f64> = freqs.iter().map(|&f| measured.at(f)).collect();
    let trend = smooth(
        &smooth(&m, (POINTS_PER_OCTAVE / 24.0) as usize),
        (POINTS_PER_OCTAVE / 2.0) as usize,
    );
    ups_and_downs(model_db)
        .iter()
        .zip(&trend)
        .map(|(d, t)| d + t)
        .collect()
}

/// How uneven a response on a [`grid`] is: the RMS of its peaks and dips, dB.
#[must_use]
pub fn unevenness_db(freqs: &[f64], db: &[f64]) -> f64 {
    debug_assert_eq!(freqs.len(), db.len());
    let d = ups_and_downs(db);
    (d.iter().map(|v| v * v).sum::<f64>() / d.len().max(1) as f64).sqrt()
}

/// How far the model's peaks and dips at the seat are from the measurement's, from
/// [`FROM_HZ`] to [`TRUSTED_TO_HZ`], dB RMS: about 3 in a room it describes well, 6–8 when
/// the room or the positions are wrong.
#[must_use]
pub fn agreement_db(measured: &Response, model: &Model, source: Point, seat: Point) -> f64 {
    let eval = Evaluator::new(model, &agreement_grid());
    let target = ups_and_downs(&measured_on_grid(measured, &eval.freqs));
    agreement_with(&eval, &target, source, seat)
}

/// The frequencies [`agreement_db`] evaluates: half an octave wider than it compares, so
/// the trend is not cut short at the ends.
fn agreement_grid() -> Vec<f64> {
    grid(FROM_HZ / 1.5, TRUSTED_TO_HZ * 1.5)
}

fn measured_on_grid(measured: &Response, freqs: &[f64]) -> Vec<f64> {
    freqs.iter().map(|&f| measured.at(f)).collect()
}

/// [`agreement_db`] against the measurement's peaks and dips already worked out.
fn agreement_with(eval: &Evaluator<'_>, target: &[f64], source: Point, seat: Point) -> f64 {
    let model = ups_and_downs(&eval.level_db(&[source], seat, SEAT_SPREAD_M));
    let inside: Vec<f64> = eval
        .freqs
        .iter()
        .zip(target.iter().zip(&model))
        .filter(|(f, _)| (FROM_HZ..=TRUSTED_TO_HZ).contains(*f))
        .map(|(_, (a, b))| (a - b).powi(2))
        .collect();
    (inside.iter().sum::<f64>() / inside.len().max(1) as f64).sqrt()
}

/// The resonances heard over several measurements (mic positions, both speakers), as
/// `(Hz, weight)` for [`guess_rooms`]: modes within a fortieth of an octave are one, and
/// its weight is the share of the measurements that heard it.
#[must_use]
pub fn heard_resonances(measurements: &[&[Mode]]) -> Vec<(f64, f64)> {
    let mut groups: Vec<(f64, usize)> = Vec::new();
    for m in measurements.iter().flat_map(|ms| ms.iter()) {
        match groups
            .iter_mut()
            .find(|g| (g.0 / m.hz).log2().abs() < 1.0 / 40.0)
        {
            Some(g) => {
                g.0 = (g.0 * g.1 as f64 + m.hz) / (g.1 + 1) as f64;
                g.1 += 1;
            }
            None => groups.push((m.hz, 1)),
        }
    }
    let n = measurements.len().max(1) as f64;
    let mut out: Vec<(f64, f64)> = groups
        .into_iter()
        .map(|(hz, count)| (hz, (count as f64 / n).min(1.0)))
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// Up to `most` different rooms whose modes explain the measured resonances (`(Hz,
/// weight)`), most likely first. Only a short list of candidates: the same few resonances
/// fit several boxes (a room's combined front-to-back-and-side-to-side mode looks like a
/// third pair of walls), so [`guess`] decides between them by the shape of the bass.
#[must_use]
pub fn guess_rooms(resonances: &[(f64, f64)], most: usize) -> Vec<Room> {
    if resonances.len() < 2 {
        return Vec::new();
    }
    let top = resonances.iter().map(|r| r.0).fold(0.0, f64::max) * 1.05;
    let total: f64 = resonances.iter().map(|r| r.1).sum();
    let score = |room: &Room| {
        let modes = room.modes(top);
        let explained: f64 = resonances
            .iter()
            .map(|&(hz, w)| {
                let err = modes
                    .iter()
                    .map(|m| (m.hz / hz).ln().abs())
                    .fold(f64::INFINITY, f64::min);
                w * (1.0 - err / 0.015).max(0.0)
            })
            .sum();
        // A room with modes so dense that anything matches explains nothing.
        explained - 0.08 * modes.len() as f64 * total / resonances.len() as f64
    };
    let mut lengths: Vec<f64> = resonances
        .iter()
        .flat_map(|&(hz, _)| (1..=3).map(move |n| f64::from(n) * SPEED_OF_SOUND / (2.0 * hz)))
        .filter(|l| (2.0..=9.0).contains(l))
        .map(|l| (l * 100.0).round() / 100.0)
        .collect();
    // A dimension heard only through a wave around four walls: when `f₂` is the wave
    // around the walls `f₁` runs between and another pair, `√(f₂² − f₁²)` is that other
    // pair's own wave. It is easily missed on its own when it sits next to a stronger one
    // (30.4 Hz beside 33 Hz in a 5.64 × 5.24 m room), but the wave around all four walls
    // gives it away.
    for (i, &(f1, _)) in resonances.iter().enumerate() {
        for &(f2, _) in &resonances[i + 1..] {
            let (lo, hi) = if f1 < f2 { (f1, f2) } else { (f2, f1) };
            let hidden = (hi * hi - lo * lo).sqrt();
            let l = SPEED_OF_SOUND / (2.0 * hidden);
            if hi < 2.0 * lo && (2.0..=9.0).contains(&l) {
                lengths.push((l * 100.0).round() / 100.0);
            }
        }
    }
    lengths.sort_by(f64::total_cmp);
    lengths.dedup();
    let mut heights: Vec<f64> = vec![2.4, 2.5, 2.6, 2.7, 2.8, 3.0];
    heights.extend(lengths.iter().filter(|h| (2.2..=3.5).contains(*h)));
    let mut scored: Vec<(f64, Room)> = Vec::new();
    for (i, &a) in lengths.iter().enumerate() {
        for &b in &lengths[..=i] {
            for &h in &heights {
                if h > b {
                    continue;
                }
                let room = Room {
                    length: a,
                    width: b,
                    height: h,
                };
                scored.push((score(&room), room));
            }
        }
    }
    scored.sort_by(|x, y| y.0.total_cmp(&x.0));
    // The best few different rooms, each refined by up to 3 % on the two floor dimensions.
    let similar = |a: &Room, b: &Room| {
        (a.length / b.length - 1.0).abs() < 0.05 && (a.width / b.width - 1.0).abs() < 0.05
    };
    let mut distinct: Vec<(f64, Room)> = Vec::new();
    for &(s, room) in &scored {
        if !distinct.iter().any(|(_, r)| similar(r, &room)) {
            distinct.push((s, room));
        }
        if distinct.len() == most {
            break;
        }
    }
    let mut refined: Vec<(f64, Room)> = distinct
        .iter()
        .map(|&(s, room)| {
            let mut best = (s, room);
            for dl in -3..=3 {
                for dw in -3..=3 {
                    let r = Room {
                        length: room.length * (1.0 + f64::from(dl) * 0.01),
                        width: room.width * (1.0 + f64::from(dw) * 0.01),
                        height: room.height,
                    };
                    let v = score(&r);
                    if v > best.0 {
                        best = (v, r);
                    }
                }
            }
            best
        })
        .collect();
    refined.sort_by(|x, y| y.0.total_cmp(&x.0));
    refined
        .into_iter()
        .map(|(_, r)| Room {
            length: (r.length * 100.0).round() / 100.0,
            width: (r.width * 100.0).round() / 100.0,
            height: r.height,
        })
        .collect()
}

/// What a measurement says about where things are.
#[derive(Debug, Clone, Copy)]
pub struct Evidence<'a> {
    /// Each speaker's measured response at the seat (`[left, right]`).
    pub responses: [&'a Response; 2],
    /// The left speaker's level against the right one's at the room's lowest modes,
    /// `(Hz, dB)`, from [`left_right_balance`]: it does not depend on where the mic was, and
    /// it tells which side of the room the speakers are on, which the bass shape alone
    /// hardly can.
    pub balance: &'a [(f64, f64)],
}

/// How much a dB of balance error counts against a dB of [`agreement_db`] in a guess.
const BALANCE_WEIGHT: f64 = 0.5;

/// A room and layout that explain the measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guess {
    pub setup: Setup,
    /// [`agreement_db`], the mean of both speakers.
    pub agreement_db: f64,
    /// [`balance_error_db`], when there was a balance to compare.
    pub balance_error_db: Option<f64>,
}

impl Guess {
    /// What the guesses are ranked by: lower is better.
    #[must_use]
    pub fn score(&self) -> f64 {
        self.agreement_db + BALANCE_WEIGHT * self.balance_error_db.unwrap_or(0.0)
    }
}

/// The left speaker's level against the right one's at each of `modes_hz` (below
/// [`BALANCE_TO_HZ`]), `(Hz, dB)`, from measurements made from the same mic positions
/// (`(left, right)` pairs). At a frequency where one standing wave dominates, the ratio is
/// where the two speakers stand in that wave, whatever the mic position: so it is the median
/// over the positions, kept only where they agree (within ±3 dB).
#[must_use]
pub fn left_right_balance(
    pairs: &[(&BassSpectrum, &BassSpectrum)],
    modes_hz: &[f64],
) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for &hz in modes_hz.iter().filter(|&&f| f <= BALANCE_TO_HZ) {
        let mut ratios: Vec<f64> = pairs
            .iter()
            .filter_map(|(l, r)| {
                // The mode's peak in the two together, within ±0.6 Hz.
                let peak = (-12..=12)
                    .map(|k| hz + f64::from(k) * 0.05)
                    .filter_map(|f| Some((f, l.at(f)?, r.at(f)?)))
                    .max_by(|a, b| {
                        (a.1.norm_sqr() + a.2.norm_sqr())
                            .total_cmp(&(b.1.norm_sqr() + b.2.norm_sqr()))
                    })?;
                Some(super::amplitude_to_db(
                    peak.1.norm() / peak.2.norm().max(1e-30),
                ))
            })
            .collect();
        if ratios.is_empty() {
            continue;
        }
        ratios.sort_by(f64::total_cmp);
        let median = ratios[ratios.len() / 2];
        if ratios.iter().all(|r| (r - median).abs() <= 3.0) || ratios.len() == 1 {
            out.push((hz, median));
        }
    }
    out
}

/// Modes above this are left out of the balance: they crowd too much for one to dominate.
pub const BALANCE_TO_HZ: f64 = 55.0;

/// How far `setup`'s predicted left/right balance at the seat is from the measured one,
/// dB RMS over the frequencies measured; `None` without any.
#[must_use]
pub fn balance_error_db(model: &Model, setup: &Setup, balance: &[(f64, f64)]) -> Option<f64> {
    if balance.is_empty() {
        return None;
    }
    let freqs: Vec<f64> = balance.iter().map(|b| b.0).collect();
    Some(balance_error_with(
        &Evaluator::new(model, &freqs),
        setup,
        balance,
    ))
}

fn balance_error_with(eval: &Evaluator<'_>, setup: &Setup, balance: &[(f64, f64)]) -> f64 {
    let l = eval.level_db(&[setup.speakers[0]], setup.seat, SEAT_SPREAD_M);
    let r = eval.level_db(&[setup.speakers[1]], setup.seat, SEAT_SPREAD_M);
    let sum: f64 = balance
        .iter()
        .zip(l.iter().zip(&r))
        .map(|(b, (l, r))| (l - r - b.1).powi(2))
        .sum();
    (sum / balance.len() as f64).sqrt()
}

/// Rooms and layouts that explain the measurement, the best first: the candidate rooms
/// from the resonances heard ([`guess_rooms`]), each with the layout that best explains the
/// evidence ([`guess_setup`]). `modes` gives the measured decay times, `typical_t60_s` the
/// rest.
#[must_use]
pub fn guess(
    heard: &[(f64, f64)],
    evidence: Evidence<'_>,
    modes: &[Mode],
    typical_t60_s: f64,
) -> Vec<Guess> {
    // Each candidate room's layout search is independent: all at once.
    let rooms = guess_rooms(heard, CANDIDATE_ROOMS);
    let mut found: Vec<Guess> = std::thread::scope(|scope| {
        let handles: Vec<_> = rooms
            .into_iter()
            .map(|room| {
                scope.spawn(move || {
                    guess_setup(room, evidence, &Model::new(room, modes, typical_t60_s))
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().expect("guess threads do not panic"))
            .collect()
    });
    found.sort_by(|a, b| a.score().total_cmp(&b.score()));
    found
}

/// Candidate rooms [`guess`] tries.
const CANDIDATE_ROOMS: usize = 8;

/// The layout in `room` that best explains the evidence: the speakers side by side at the
/// same distance from the wall behind them, the seat completing an equilateral triangle.
/// The bass shape decides most of it; the left/right balance decides the side (the mirror
/// image swaps which speaker is nearer a side wall, and so which is louder at a standing
/// wave between the side walls).
#[must_use]
pub fn guess_setup(room: Room, evidence: Evidence<'_>, model: &Model) -> Option<Guess> {
    let eval = Evaluator::new(model, &agreement_grid());
    let at_modes = Evaluator::new(
        model,
        &evidence.balance.iter().map(|b| b.0).collect::<Vec<f64>>(),
    );
    let targets: Vec<Vec<f64>> = evidence
        .responses
        .iter()
        .map(|m| ups_and_downs(&measured_on_grid(m, &eval.freqs)))
        .collect();
    let evaluate = |setup: &Setup| Guess {
        setup: *setup,
        agreement_db: (0..2)
            .map(|c| agreement_with(&eval, &targets[c], setup.speakers[c], setup.seat))
            .sum::<f64>()
            / 2.0,
        balance_error_db: (!evidence.balance.is_empty())
            .then(|| balance_error_with(&at_modes, setup, evidence.balance)),
    };
    let consider = |x: f64, mid: f64, spacing: f64, best: &mut Option<Guess>| {
        // (The first of equally good layouts wins, as scanning them in order would.)
        let seat_x = x + spacing * 3f64.sqrt() / 2.0;
        if mid - spacing / 2.0 < 0.15
            || mid + spacing / 2.0 > room.width - 0.15
            || seat_x > room.length - 0.3
        {
            return;
        }
        let setup = Setup {
            room,
            speakers: [
                Point {
                    x,
                    y: mid - spacing / 2.0,
                    z: 1.0,
                },
                Point {
                    x,
                    y: mid + spacing / 2.0,
                    z: 1.0,
                },
            ],
            seat: Point {
                x: seat_x,
                y: mid,
                z: 1.1,
            },
        };
        let g = evaluate(&setup);
        if best.as_ref().is_none_or(|b| g.score() < b.score()) {
            *best = Some(g);
        }
    };
    // Coarse, then finer around the best.
    let steps = |from: f64, to: f64, step: f64| {
        let n = ((to - from) / step).floor().max(0.0) as usize;
        (0..=n).map(move |i| from + i as f64 * step)
    };
    // Every distance from the wall on its own thread; the best of each, in order, so the
    // result is the one a scan in that order finds.
    let in_parallel = |rows: Vec<f64>, per_row: &(dyn Fn(f64, &mut Option<Guess>) + Sync)| {
        std::thread::scope(|scope| {
            let handles: Vec<_> = rows
                .into_iter()
                .map(|x| {
                    scope.spawn(move || {
                        let mut best = None;
                        per_row(x, &mut best);
                        best
                    })
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().expect("guess threads do not panic"))
                .fold(None, |best: Option<Guess>, g| {
                    if best.as_ref().is_none_or(|b| g.score() < b.score()) {
                        Some(g)
                    } else {
                        best
                    }
                })
        })
    };
    let best = in_parallel(
        steps(0.2, (room.length / 2.0).min(2.0), 0.2).collect(),
        &|x, best| {
            for mid in steps(0.6, room.width - 0.6, 0.2) {
                for spacing in [1.0, 1.4, 1.8] {
                    consider(x, mid, spacing, best);
                }
            }
        },
    );
    let coarse = best?.setup;
    let (x0, mid0) = (coarse.speakers[0].x, coarse.seat.y);
    let spacing0 = coarse.speakers[1].y - coarse.speakers[0].y;
    let fine = in_parallel(steps(x0 - 0.15, x0 + 0.15, 0.05).collect(), &|x, best| {
        for mid in steps(mid0 - 0.15, mid0 + 0.15, 0.05) {
            for spacing in steps(spacing0 - 0.2, spacing0 + 0.2, 0.1) {
                if x > 0.1 && spacing > 0.6 {
                    consider(x, mid, spacing, best);
                }
            }
        }
    });
    // A finer layout replaces the coarse one only when it is better.
    match (best, fine) {
        (Some(b), Some(f)) if f.score() < b.score() => Some(f),
        (b, _) => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: Room = Room {
        length: 5.64,
        width: 5.24,
        height: 2.7,
    };

    fn corner_setup() -> Setup {
        Setup {
            room: ROOM,
            speakers: [
                Point {
                    x: 0.95,
                    y: 3.41,
                    z: 1.05,
                },
                Point {
                    x: 0.95,
                    y: 4.54,
                    z: 1.05,
                },
            ],
            seat: Point {
                x: 1.93,
                y: 3.975,
                z: 1.15,
            },
        }
    }

    #[test]
    fn modes_of_a_real_room() {
        let modes = ROOM.modes(50.0);
        let f: Vec<(String, f64)> = modes.iter().map(|m| (m.label(), m.hz)).collect();
        assert_eq!(f[0].0, "(1,0,0)");
        assert!((f[0].1 - 30.41).abs() < 0.02, "{f:?}");
        assert_eq!(f[1].0, "(0,1,0)");
        assert!((f[1].1 - 32.73).abs() < 0.02, "{f:?}");
        assert_eq!(f[2].0, "(1,1,0)");
        assert!((f[2].1 - 44.68).abs() < 0.02, "{f:?}");
        assert_eq!(modes[2].kind(), Kind::Tangential);
        assert_eq!(modes[1].describe(&ROOM), "side to side (5.24 m)");
        assert_eq!(modes[2].describe(&ROOM), "front to back and side to side");
    }

    #[test]
    fn a_seat_on_a_modes_silent_line_does_not_hear_it() {
        let model = Model::new(ROOM, &[], 0.6);
        let m = model.modes[0];
        let corner = Point {
            x: 0.1,
            y: 0.1,
            z: 0.1,
        };
        let middle = Point {
            x: ROOM.length / 2.0,
            y: 1.0,
            z: 1.2,
        };
        // At 30.4 Hz, the (1,0,0) mode's frequency, the middle of the length hears far less
        // than a corner (the 32.7 Hz mode next to it still reaches it).
        let at = |r: Point| model.response(&[corner], r, &[m.hz])[0].norm();
        assert!(
            at(corner) > 5.0 * at(middle),
            "{} vs {}",
            at(corner),
            at(middle)
        );
    }

    #[test]
    fn measured_decay_times_go_to_their_modes() {
        let measured = [Mode {
            hz: 32.7,
            t60_s: 1.7,
            t60_from_width_s: 1.7,
            prominence_db: 12.0,
        }];
        let model = Model::new(ROOM, &measured, 0.6);
        let i = model
            .modes
            .iter()
            .position(|m| m.order == [0, 1, 0])
            .expect("mode");
        assert_eq!(model.t60_s[i], 1.7);
        assert!(model.measured[i]);
        assert_eq!(model.t60_s[0], 0.6);
        // Identified with the speakers and the seat known: 45 Hz is the (1,1,0) mode.
        let named = model.identify(45.0, Some(&corner_setup())).expect("a mode");
        assert_eq!(named.order, [1, 1, 0]);
    }

    /// The model's own response for a known layout, as a measurement, is explained best by
    /// that layout or its mirror image.
    #[test]
    fn finds_the_layout_that_explains_a_measurement() {
        let setup = corner_setup();
        let model = Model::new(ROOM, &[], 0.8);
        let freqs = grid(12.0, 200.0);
        let measured: Vec<Response> = (0..2)
            .map(|c| Response {
                freqs: freqs.clone(),
                db: model.level_db(&[setup.speakers[c]], setup.seat, &freqs, SEAT_SPREAD_M),
            })
            .collect();
        let truth = agreement_db(&measured[1], &model, setup.speakers[1], setup.seat);
        // Not quite 0: the measurement is read between the points of another grid.
        assert!(truth < 0.1, "{truth}");
        // The left/right balance at the lowest modes, as a measurement would give it.
        let balance: Vec<(f64, f64)> = [32.7, 44.7]
            .iter()
            .map(|&hz| {
                let l = model.level_db(&[setup.speakers[0]], setup.seat, &[hz], SEAT_SPREAD_M)[0];
                let r = model.level_db(&[setup.speakers[1]], setup.seat, &[hz], SEAT_SPREAD_M)[0];
                (hz, l - r)
            })
            .collect();
        let evidence = Evidence {
            responses: [&measured[0], &measured[1]],
            balance: &balance,
        };
        let g = guess_setup(ROOM, evidence, &model).expect("a layout");
        let (found, fit) = (g.setup, g.agreement_db);
        assert!(g.balance_error_db.expect("compared") < 1.0, "{g:?}");
        // With the balance, the side is right too (not the mirror image).
        let close = |a: f64, b: f64| (a - b).abs() < 0.25;
        assert!(
            close(found.speakers[0].x, setup.speakers[0].x)
                && close(found.seat.y, setup.seat.y)
                && fit < 1.5,
            "{found:?} fit {fit}"
        );
    }

    #[test]
    fn guesses_the_room_from_its_resonances() {
        // What a corner setup in the real room showed: the lowest modes, with the weak
        // 30.4 Hz one.
        let heard = [
            (30.4, 0.4),
            (32.75, 1.0),
            (45.0, 1.0),
            (70.0, 0.6),
            (83.4, 0.4),
            (97.1, 0.5),
            (120.8, 0.6),
        ];
        let rooms = guess_rooms(&heard, 8);
        assert!(
            rooms
                .iter()
                .any(|r| (r.length - 5.64).abs() < 0.08 && (r.width - 5.24).abs() < 0.08),
            "{rooms:?}"
        );
    }

    #[test]
    fn the_seat_map_finds_a_more_even_seat() {
        let setup = corner_setup();
        let model = Model::new(ROOM, &[], 0.8);
        let map = model.seat_map(&setup, 0.2);
        assert_eq!(map.values.len(), map.nx * map.ny);
        let here = map.at(setup.seat.x, setup.seat.y);
        let (_, _, best) = map
            .best_near(setup.seat.x, setup.seat.y, 0.6)
            .expect("cells");
        assert!(best <= here);
        assert!(map.values.iter().all(|v| v.is_finite()));
    }
}
