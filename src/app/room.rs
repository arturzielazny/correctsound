//! The room screen's state and keys: the room and where the speakers and the seat are (guessed
//! from the measurement, or entered with a tape measure), and what the room model says
//! about them ([`crate::dsp::roommodel`]): which standing wave each measured resonance is,
//! how well the model reproduces the measurement, where traps would work, and whether
//! another seat or another place for the whole setup would sound more even.

use crossterm::event::KeyCode;

use super::{App, Overlay, measure};
use crate::analysis::Responses;
use crate::dsp::modes::Mode;
use crate::dsp::roommodel::{
    self, Evidence, FROM_HZ, Guess, JUDGED_TO_HZ, Model, Point, RoomMode, SeatMap, Setup, grid,
    unevenness_db,
};
use crate::dsp::spectrum::Response;

/// Which room on the list is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selected {
    /// One of the user's own rooms, entered with a tape measure (saved).
    Yours(usize),
    /// One of the rooms and layouts that explain the measurement, best first.
    Guess(usize),
}

/// What the plan shows over the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanView {
    /// The selected mode's loud and silent places.
    Mode,
    /// How even the bass would be at every seat.
    Seats,
}

/// One editable number. They follow how people measure a room: the setup from the wall
/// behind it and from the nearer side wall, the speakers' spacing, and the seat from the
/// speakers. Changing one keeps the rest of the setup together: a wider room keeps the
/// setup in its corner, moving the speakers moves the seat with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Length,
    Width,
    Height,
    SpeakersFromFront,
    SideWall,
    Apart,
    SpeakerHeight,
    SeatFromSpeakers,
    SeatOffCentre,
    EarHeight,
}

impl Field {
    pub const ALL: [Self; 10] = [
        Self::Length,
        Self::Width,
        Self::Height,
        Self::SpeakersFromFront,
        Self::SideWall,
        Self::Apart,
        Self::SpeakerHeight,
        Self::SeatFromSpeakers,
        Self::SeatOffCentre,
        Self::EarHeight,
    ];

    /// What it is in `s`, for the screen (the side wall is the one nearer the speakers).
    #[must_use]
    pub fn label(self, s: &Setup) -> &'static str {
        match self {
            Self::Length => "Room, front to back",
            Self::Width => "Room, side to side",
            Self::Height => "Room, floor to ceiling",
            Self::SpeakersFromFront => "Speakers' fronts to the wall behind",
            Self::SideWall if on_right(s) => "Right speaker to the right wall",
            Self::SideWall => "Left speaker to the left wall",
            Self::Apart => "Speakers apart, centre to centre",
            Self::SpeakerHeight => "Woofers above the floor",
            Self::SeatFromSpeakers => "Your ears to the speakers' line",
            Self::SeatOffCentre => "Your ears right of centre (left: -)",
            Self::EarHeight => "Your ears above the floor",
        }
    }

    /// Its value in `s`, m.
    #[must_use]
    pub fn get(self, s: &Setup) -> f64 {
        match self {
            Self::Length => s.room.length,
            Self::Width => s.room.width,
            Self::Height => s.room.height,
            Self::SpeakersFromFront => s.speakers[0].x,
            Self::SideWall if on_right(s) => s.room.width - s.speakers[1].y,
            Self::SideWall => s.speakers[0].y,
            Self::Apart => s.speakers[1].y - s.speakers[0].y,
            Self::SpeakerHeight => s.speakers[0].z,
            Self::SeatFromSpeakers => s.seat.x - s.speakers[0].x,
            Self::SeatOffCentre => s.seat.y - middle(s),
            Self::EarHeight => s.seat.z,
        }
    }

    /// Sets its value in `s`, keeping the rest of the setup together.
    pub fn set(self, s: &mut Setup, v: f64) {
        let v = if self == Self::SeatOffCentre {
            v
        } else {
            v.max(0.0)
        };
        match self {
            Self::Length => s.room.length = v,
            Self::Width => {
                // A setup in the right half keeps its distance from the right wall.
                if on_right(s) {
                    shift_sideways(s, v - s.room.width);
                }
                s.room.width = v;
            }
            Self::Height => s.room.height = v,
            Self::SpeakersFromFront => {
                let by = v - s.speakers[0].x;
                for p in &mut s.speakers {
                    p.x = v;
                }
                s.seat.x += by;
            }
            Self::SideWall => {
                let by = if on_right(s) {
                    s.room.width - v - s.speakers[1].y
                } else {
                    v - s.speakers[0].y
                };
                shift_sideways(s, by);
            }
            Self::Apart => {
                // The speaker nearer its wall stays; the seat stays centred between them.
                let off_centre = s.seat.y - middle(s);
                if on_right(s) {
                    s.speakers[0].y = s.speakers[1].y - v;
                } else {
                    s.speakers[1].y = s.speakers[0].y + v;
                }
                s.seat.y = middle(s) + off_centre;
            }
            Self::SpeakerHeight => {
                for p in &mut s.speakers {
                    p.z = v;
                }
            }
            Self::SeatFromSpeakers => s.seat.x = s.speakers[0].x + v,
            Self::SeatOffCentre => s.seat.y = middle(s) + v,
            Self::EarHeight => s.seat.z = v,
        }
        if s.room.is_valid() {
            s.keep_inside();
        }
    }
}

/// Halfway between the speakers, m from the left wall.
fn middle(s: &Setup) -> f64 {
    (s.speakers[0].y + s.speakers[1].y) / 2.0
}

/// True when the speakers stand in the right half of the room.
fn on_right(s: &Setup) -> bool {
    middle(s) > s.room.width / 2.0
}

/// Moves the speakers and the seat `by` m to the right.
fn shift_sideways(s: &mut Setup, by: f64) {
    for p in s.speakers.iter_mut().chain(std::iter::once(&mut s.seat)) {
        p.y += by;
    }
}

/// `s` mirrored left to right: the same setup against the other side wall.
#[must_use]
pub fn mirrored(s: &Setup) -> Setup {
    let w = s.room.width;
    let flip = |p: Point| Point { y: w - p.y, ..p };
    Setup {
        room: s.room,
        speakers: [flip(s.speakers[1]), flip(s.speakers[0])],
        seat: flip(s.seat),
    }
}

/// A measured resonance, and which of the room's standing waves it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Named {
    pub measured: Mode,
    /// `None` when no standing wave of the room is near: furniture, a reflection.
    pub mode: Option<RoomMode>,
}

/// A change of position the model expects to make the bass more even.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// What to do, in words: "sit 20 cm further back and 30 cm to the left".
    pub what: String,
    /// The bass below [`JUDGED_TO_HZ`] now and after, as unevenness, dB.
    pub now_db: f64,
    pub then_db: f64,
}

/// Everything the model says about one setup.
#[derive(Debug, Clone)]
pub struct Derived {
    pub setup: Setup,
    pub model: Model,
    /// How far the model is from each speaker's measurement below 60 Hz, dB.
    pub agreement_db: [Option<f64>; 2],
    /// The measured resonances, named.
    pub named: Vec<Named>,
    /// The modes listed, lowest first: the measured ones, then the strongest others below
    /// [`JUDGED_TO_HZ`].
    pub listed: Vec<RoomMode>,
    pub seat_map: SeatMap,
    /// The model's bass at the seat for each speaker, dB, on its frequency grid.
    pub predicted: [Response; 2],
    pub seat: Option<Suggestion>,
    pub shift: Option<Suggestion>,
    /// How far the predicted left/right balance at the lowest modes is from the measured
    /// one ([`Derived::balance`]), dB.
    pub balance_error_db: Option<f64>,
    pub balance: Vec<(f64, f64)>,
}

/// Agreement that counts as describing the room well, dB.
pub const GOOD_AGREEMENT_DB: f64 = 3.5;
/// Beyond this the model is not trusted with suggestions, dB.
pub const POOR_AGREEMENT_DB: f64 = 5.0;
/// A suggestion must improve the bass by at least this much to be offered, dB.
const WORTH_DB: f64 = 1.0;
/// How far the seat suggestion looks, m.
const SEAT_RADIUS_M: f64 = 0.6;
/// Seat map resolution, m.
const MAP_STEP_M: f64 = 0.1;

/// The room screen.
#[derive(Debug, Clone)]
pub struct RoomState {
    /// The user's own rooms, as entered (saved in the config).
    pub yours: Vec<Setup>,
    /// How well each of [`Self::yours`] explains the measurement ([`roommodel::agreement_db`],
    /// both speakers), once worked out.
    pub yours_fit: Vec<Option<f64>>,
    /// Rooms and layouts that explain the measurement, best first.
    pub guesses: Vec<Guess>,
    /// How many measurements the guesses were worked out from.
    guessed_from: usize,
    pub selected: Selected,
    pub field: usize,
    /// A number being typed into the selected field.
    pub typing: String,
    /// The mode shown on the plan, an index into [`Derived::listed`].
    pub mode: usize,
    pub view: PlanView,
    derived: Option<Derived>,
    /// Set when one of the user's rooms changed (or was added or deleted), to be saved.
    changed: bool,
    /// The room of the user's that the next `x` deletes.
    delete_armed: Option<usize>,
}

impl Default for RoomState {
    fn default() -> Self {
        Self {
            yours: Vec::new(),
            yours_fit: Vec::new(),
            guesses: Vec::new(),
            guessed_from: 0,
            selected: Selected::Guess(0),
            field: 0,
            typing: String::new(),
            mode: 0,
            view: PlanView::Mode,
            derived: None,
            changed: false,
            delete_armed: None,
        }
    }
}

/// What the room screen needs from the measurement.
pub struct Measured<'a> {
    pub responses: [&'a Responses; 2],
    /// Every measurement's modes, each speaker's positions in turn.
    pub per_measurement: Vec<&'a [Mode]>,
    /// Each speaker's modes, merged over its positions.
    pub merged: Vec<Mode>,
    /// The left speaker's level against the right one's at the lowest modes, `(Hz, dB)`.
    pub balance: Vec<(f64, f64)>,
    /// The room's typical reverberation time, s.
    pub typical_t60_s: f64,
}

impl Measured<'_> {
    fn evidence(&self) -> Evidence<'_> {
        Evidence {
            responses: [&self.responses[0].display, &self.responses[1].display],
            balance: &self.balance,
        }
    }
}

impl RoomState {
    /// Starts with the user's saved rooms (`session` first: the room this measurement was
    /// made in, when it was saved with it).
    pub fn load(&mut self, mut yours: Vec<Setup>, session: Option<Setup>) {
        if let Some(s) = session {
            yours.retain(|y| *y != s);
            yours.insert(0, s);
        }
        self.yours_fit = vec![None; yours.len()];
        self.yours = yours;
        self.selected = if self.yours.is_empty() {
            Selected::Guess(0)
        } else {
            Selected::Yours(0)
        };
    }

    /// Works out the guesses once per set of measurements, and how well the user's rooms
    /// explain the measurement.
    pub fn guess(&mut self, m: &Measured<'_>) {
        if self.guessed_from != m.per_measurement.len() || self.guesses.is_empty() {
            let heard = roommodel::heard_resonances(&m.per_measurement);
            self.guesses = roommodel::guess(&heard, m.evidence(), &m.merged, m.typical_t60_s);
            self.guessed_from = m.per_measurement.len();
        }
        if self.yours_fit.len() != self.yours.len() {
            self.yours_fit = vec![None; self.yours.len()];
        }
        for (fit, setup) in self.yours_fit.iter_mut().zip(&self.yours) {
            if fit.is_none() && setup.room.is_valid() {
                let model = Model::new(setup.room, &m.merged, m.typical_t60_s);
                *fit = Some(
                    (0..2)
                        .map(|c| {
                            roommodel::agreement_db(
                                &m.responses[c].display,
                                &model,
                                setup.speakers[c],
                                setup.seat,
                            )
                        })
                        .sum::<f64>()
                        / 2.0,
                );
            }
        }
    }

    /// The room shown.
    #[must_use]
    pub fn setup(&self) -> Option<Setup> {
        match self.selected {
            Selected::Yours(i) => self.yours.get(i).copied(),
            Selected::Guess(i) => self.guesses.get(i).map(|g| g.setup),
        }
    }

    /// True when the room shown is a guess, not one of the user's.
    #[must_use]
    pub fn showing_guess(&self) -> bool {
        matches!(self.selected, Selected::Guess(_))
    }

    /// Everything on the list, in order: the user's rooms, then the guesses.
    #[must_use]
    pub fn entries(&self) -> Vec<Selected> {
        (0..self.yours.len())
            .map(Selected::Yours)
            .chain((0..self.guesses.len()).map(Selected::Guess))
            .collect()
    }

    /// Shows the next (`step` 1) or previous (−1) room on the list.
    pub fn step(&mut self, step: isize) {
        let all = self.entries();
        if all.is_empty() {
            return;
        }
        let now = all.iter().position(|e| *e == self.selected).unwrap_or(0) as isize;
        let n = all.len() as isize;
        self.selected = all[(now + step).rem_euclid(n) as usize];
        self.typing.clear();
        self.delete_armed = None;
    }

    /// The room to change: the user's room shown, or, when a guess is shown, a new room of
    /// the user's starting from it. Returns the room and whether it is new.
    fn editable(&mut self) -> Option<(&mut Setup, bool)> {
        let mut new = false;
        if let Selected::Guess(i) = self.selected {
            let g = self.guesses.get(i)?.setup;
            self.yours.push(g);
            self.yours_fit.push(None);
            self.selected = Selected::Yours(self.yours.len() - 1);
            new = true;
        }
        let Selected::Yours(i) = self.selected else {
            return None;
        };
        self.changed = true;
        self.delete_armed = None;
        if let Some(f) = self.yours_fit.get_mut(i) {
            *f = None;
        }
        self.yours.get_mut(i).map(|s| (s, new))
    }

    /// The selected field.
    #[must_use]
    pub fn selected_field(&self) -> Field {
        Field::ALL[self.field.min(Field::ALL.len() - 1)]
    }

    /// Changes the selected field by `delta` m; true when that made a new room of the
    /// user's from a guess.
    pub fn nudge(&mut self, delta: f64) -> bool {
        let field = self.selected_field();
        self.editable().is_some_and(|(s, new)| {
            let v = field.get(s);
            field.set(s, ((v + delta) * 100.0).round() / 100.0);
            new
        })
    }

    /// Mirrors the setup to the other side of the room; true when that made a new room of
    /// the user's from a guess.
    pub fn mirror(&mut self) -> bool {
        self.editable().is_some_and(|(s, new)| {
            *s = mirrored(s);
            new
        })
    }

    /// Applies the number typed; `Ok(true)` when that made a new room of the user's from a
    /// guess.
    ///
    /// # Errors
    ///
    /// The text typed, when it is not a number.
    pub fn apply_typed(&mut self) -> Result<bool, String> {
        let text = std::mem::take(&mut self.typing).replace(',', ".");
        if text.trim().is_empty() {
            return Ok(false);
        }
        let v: f64 = text.trim().parse().map_err(|_| text.clone())?;
        let field = self.selected_field();
        Ok(self.editable().is_some_and(|(s, new)| {
            field.set(s, v);
            new
        }))
    }

    /// Deletes the user's room shown on the second call (the first only asks); returns what
    /// happened, for the user.
    pub fn delete(&mut self) -> String {
        let Selected::Yours(i) = self.selected else {
            return "Guesses cannot be deleted: they come from the measurement.".to_owned();
        };
        if self.delete_armed != Some(i) {
            self.delete_armed = Some(i);
            return format!("Press x again to delete your room {}.", i + 1);
        }
        self.yours.remove(i);
        self.yours_fit.remove(i);
        self.delete_armed = None;
        self.changed = true;
        self.selected = if self.yours.is_empty() {
            Selected::Guess(0)
        } else {
            Selected::Yours(i.min(self.yours.len() - 1))
        };
        format!("Deleted your room {}.", i + 1)
    }

    /// True once after the user's rooms changed: they should be saved.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// The model's view of the current setup, worked out again only when it changed.
    pub fn derived(&mut self, m: &Measured<'_>) -> Option<&Derived> {
        let setup = self.setup()?;
        if !setup.room.is_valid() {
            self.derived = None;
            return None;
        }
        if self.derived.as_ref().is_none_or(|d| d.setup != setup) {
            let was: Vec<[u8; 3]> = self
                .derived
                .as_ref()
                .map(|d| d.listed.iter().map(|m| m.order).collect())
                .unwrap_or_default();
            let new = derive(setup, m);
            let now: Vec<[u8; 3]> = new.listed.iter().map(|md| md.order).collect();
            // A new list of waves starts on the one that rings longest.
            if was != now {
                self.mode = new
                    .named
                    .iter()
                    .filter_map(|n| n.mode.map(|md| (md.order, n.measured.t60_s)))
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .and_then(|(order, _)| now.iter().position(|o| *o == order))
                    .unwrap_or(0);
            }
            self.mode = self.mode.min(now.len().saturating_sub(1));
            self.derived = Some(new);
        }
        self.derived.as_ref()
    }

    /// The last [`Self::derived`], without working anything out (for drawing).
    #[must_use]
    pub fn current(&self) -> Option<&Derived> {
        self.derived
            .as_ref()
            .filter(|d| Some(d.setup) == self.setup())
    }
}

/// Works out everything the model says about `setup`.
fn derive(setup: Setup, m: &Measured<'_>) -> Derived {
    let model = Model::new(setup.room, &m.merged, m.typical_t60_s);
    let agreement_db = [0, 1].map(|c| {
        Some(roommodel::agreement_db(
            &m.responses[c].display,
            &model,
            setup.speakers[c],
            setup.seat,
        ))
    });
    // Each measured resonance once (both speakers merged), named.
    let mut measured = m.merged.clone();
    measured.sort_by(|a, b| a.hz.total_cmp(&b.hz));
    measured.dedup_by(|a, b| (a.hz / b.hz).log2().abs() < 1.0 / 40.0);
    let named: Vec<Named> = measured
        .iter()
        .map(|&mm| Named {
            measured: mm,
            mode: model.identify(mm.hz, Some(&setup)),
        })
        .collect();
    // The measured modes, then the strongest others the speakers drive at the seat.
    let mut listed: Vec<RoomMode> = named.iter().filter_map(|n| n.mode).collect();
    let mut others: Vec<RoomMode> = model
        .modes
        .iter()
        .filter(|md| md.hz <= JUDGED_TO_HZ && !listed.iter().any(|l| l.order == md.order))
        .copied()
        .collect();
    others
        .sort_by(|a, b| roommodel::coupling(b, &setup).total_cmp(&roommodel::coupling(a, &setup)));
    listed.extend(
        others
            .into_iter()
            .take(8usize.saturating_sub(listed.len()).max(2)),
    );
    listed.sort_by(|a, b| a.hz.total_cmp(&b.hz));
    listed.dedup_by(|a, b| a.order == b.order);

    let seat_map = model.seat_map(&setup, MAP_STEP_M);
    let freqs = grid(FROM_HZ / 1.5, 160.0);
    let predicted = [0, 1].map(|c| Response {
        freqs: freqs.clone(),
        db: model.level_db(
            &[setup.speakers[c]],
            setup.seat,
            &freqs,
            roommodel::SEAT_SPREAD_M,
        ),
    });
    let seat = seat_suggestion(&setup, &seat_map);
    let shift = shift_suggestion(&setup, &model);
    let balance_error_db = roommodel::balance_error_db(&model, &setup, &m.balance);
    Derived {
        balance_error_db,
        balance: m.balance.clone(),
        setup,
        model,
        agreement_db,
        named,
        listed,
        seat_map,
        predicted,
        seat,
        shift,
    }
}

/// "20 cm further back and 30 cm to the left", for a move of `dx` (away from the wall
/// behind the speakers) and `dy` (towards the right wall), m.
fn moved(dx: f64, dy: f64, forward_word: &str, back_word: &str) -> String {
    let mut parts = Vec::new();
    let cm = |v: f64| (v.abs() * 100.0 / 5.0).round() * 5.0;
    if cm(dx) >= 5.0 {
        parts.push(format!(
            "{:.0} cm {}",
            cm(dx),
            if dx > 0.0 { back_word } else { forward_word }
        ));
    }
    if cm(dy) >= 5.0 {
        parts.push(format!(
            "{:.0} cm to the {}",
            cm(dy),
            if dy > 0.0 { "right" } else { "left" }
        ));
    }
    parts.join(" and ")
}

/// The most even seat within [`SEAT_RADIUS_M`], when it is worth moving to.
fn seat_suggestion(setup: &Setup, map: &SeatMap) -> Option<Suggestion> {
    let now = map.at(setup.seat.x, setup.seat.y);
    let (x, y, then) = map.best_near(setup.seat.x, setup.seat.y, SEAT_RADIUS_M)?;
    let what = moved(
        x - setup.seat.x,
        y - setup.seat.y,
        "closer to the speakers",
        "further back",
    );
    (now - then >= WORTH_DB && !what.is_empty()).then(|| Suggestion {
        what: format!("sit {what}"),
        now_db: now,
        then_db: then,
    })
}

/// The best place for the whole setup (speakers and seat together, as they are): moved
/// along the wall behind the speakers, and out from it.
fn shift_suggestion(setup: &Setup, model: &Model) -> Option<Suggestion> {
    let freqs = grid(FROM_HZ, JUDGED_TO_HZ);
    let room = setup.room;
    let judge = |s: &Setup| {
        let db = model.level_db(&s.speakers, s.seat, &freqs, 0.0);
        unevenness_db(&freqs, &db)
    };
    let now = judge(setup);
    let ys: Vec<f64> = setup
        .speakers
        .iter()
        .map(|p| p.y)
        .chain([setup.seat.y])
        .collect();
    let xs: Vec<f64> = setup
        .speakers
        .iter()
        .map(|p| p.x)
        .chain([setup.seat.x])
        .collect();
    let (min_y, max_y) = (
        ys.iter().copied().fold(f64::INFINITY, f64::min),
        ys.iter().copied().fold(0.0, f64::max),
    );
    let (min_x, max_x) = (
        xs.iter().copied().fold(f64::INFINITY, f64::min),
        xs.iter().copied().fold(0.0, f64::max),
    );
    let mut best: Option<(f64, f64, f64)> = None;
    let mut dy = 0.15 - min_y;
    while max_y + dy <= room.width - 0.15 {
        let mut dx = (0.15 - min_x).max(-1.0);
        while max_x + dx <= (room.length - 0.3).min(max_x + 1.0) {
            let shift = |p: Point| Point {
                x: p.x + dx,
                y: p.y + dy,
                z: p.z,
            };
            let s = Setup {
                room,
                speakers: setup.speakers.map(shift),
                seat: shift(setup.seat),
            };
            let v = judge(&s);
            if best.is_none_or(|b| v < b.2) {
                best = Some((dx, dy, v));
            }
            dx += 0.1;
        }
        dy += 0.1;
    }
    let (dx, dy, then) = best?;
    let what = moved(dx, dy, "closer to the wall behind them", "further out");
    (now - then >= WORTH_DB * 1.5 && !what.is_empty()).then(|| Suggestion {
        what: format!("move the speakers and your seat together {what}"),
        now_db: now,
        then_db: then,
    })
}

/// The longest-ringing measured modes that ring clearly longer than the room, worst
/// first: what bass traps are for.
#[must_use]
pub fn ringing(named: &[Named], typical_t60_s: f64) -> Vec<&Named> {
    let mut out: Vec<&Named> = named
        .iter()
        .filter(|n| n.measured.t60_s > 1.5 * typical_t60_s)
        .collect();
    out.sort_by(|a, b| b.measured.t60_s.total_cmp(&a.measured.t60_s));
    out
}

/// Where traps work for `mode`, in words.
#[must_use]
pub fn trap_advice(mode: &RoomMode) -> &'static str {
    match mode.order {
        [_, 0, 0] => "on the front and back walls, best in their corners",
        [0, _, 0] => "on the left and right walls, best in their corners",
        [0, 0, _] => "along the edges of the floor and the ceiling",
        [_, _, 0] => "in the four vertical corners",
        _ => "where the walls meet the floor or the ceiling",
    }
}

// ----- the step's keys and background work -----

impl App {
    /// What the room screen needs from the measurement, when both speakers are measured.
    pub(super) fn room_measured(
        measure: &measure::MeasureState,
        typical_t60_s: f64,
    ) -> Option<Measured<'_>> {
        let [Some(l), Some(r)] = &measure.responses else {
            return None;
        };
        let per_measurement: Vec<&[crate::dsp::modes::Mode]> = measure
            .positions
            .iter()
            .flat_map(|runs| runs.iter().map(|a| a.modes.as_slice()))
            .collect();
        let merged: Vec<crate::dsp::modes::Mode> = measure
            .positions
            .iter()
            .flat_map(|runs| {
                let all: Vec<&[crate::dsp::modes::Mode]> =
                    runs.iter().map(|a| a.modes.as_slice()).collect();
                crate::dsp::modes::merge(&all)
            })
            .collect();
        // The left/right balance at each low mode, from the mic positions both speakers
        // were measured from.
        let pairs: Vec<(
            &crate::dsp::modes::BassSpectrum,
            &crate::dsp::modes::BassSpectrum,
        )> = measure.positions[0]
            .iter()
            .zip(&measure.positions[1])
            .filter_map(|(a, b)| Some((a.bass.as_ref()?, b.bass.as_ref()?)))
            .collect();
        let mut modes_hz: Vec<f64> = merged.iter().map(|m| m.hz).collect();
        modes_hz.sort_by(f64::total_cmp);
        modes_hz.dedup_by(|a, b| (*a / *b).log2().abs() < 1.0 / 40.0);
        let balance = crate::dsp::roommodel::left_right_balance(&pairs, &modes_hz);
        Some(Measured {
            responses: [l, r],
            per_measurement,
            merged,
            balance,
            typical_t60_s,
        })
    }

    /// Brings the room screen up to date with the measurement and the setup.
    pub(super) fn refresh_room(&mut self) {
        let typical = self.room_reverb_s();
        if let Some(m) = Self::room_measured(&self.measure, typical) {
            self.room.guess(&m);
            let _ = self.room.derived(&m);
        }
    }

    /// The room's reverberation in the bass, s: what modes that were not measured ring for.
    #[must_use]
    pub fn room_reverb_s(&self) -> f64 {
        let mut all: Vec<f64> = self
            .measure
            .positions
            .iter()
            .flatten()
            .flat_map(|a| a.decay.iter().filter(|d| d.0 <= 250.0).map(|d| d.1))
            .collect();
        all.sort_by(f64::total_cmp);
        all.get(all.len() / 2).copied().unwrap_or(0.5)
    }

    /// Keys on the room screen: fields, numbers, the mode on the plan, the view.
    pub(super) fn key_room(&mut self, code: KeyCode) {
        let r = &mut self.room;
        match code {
            KeyCode::Esc if !r.typing.is_empty() => r.typing.clear(),
            KeyCode::Esc | KeyCode::Char('q' | 'r') => {
                self.overlay = None;
                return;
            }
            KeyCode::Up => {
                r.typing.clear();
                r.field = r.field.saturating_sub(1);
            }
            KeyCode::Down => {
                r.typing.clear();
                r.field = (r.field + 1).min(Field::ALL.len() - 1);
            }
            KeyCode::Left | KeyCode::Right if r.typing.is_empty() => {
                let delta = if code == KeyCode::Left { -0.05 } else { 0.05 };
                if r.nudge(delta) {
                    self.message = Some(format!(
                        "Changing a guess makes it your room {}: saved, and kept for next time.",
                        r.yours.len()
                    ));
                }
            }
            KeyCode::Char(c @ ('0'..='9' | '.' | ',')) => r.typing.push(c),
            // A minus starts a number: the seat left of centre.
            KeyCode::Char('-') if r.typing.is_empty() => r.typing.push('-'),
            KeyCode::Char('m') if r.typing.is_empty() => {
                let new = r.mirror();
                self.message = Some(if new {
                    format!(
                        "Mirrored to the other side wall: your room {}, saved.",
                        r.yours.len()
                    )
                } else {
                    "Mirrored to the other side wall.".to_owned()
                });
            }
            KeyCode::Backspace => {
                r.typing.pop();
            }
            KeyCode::Enter => match r.apply_typed() {
                Ok(true) => {
                    self.message = Some(format!(
                        "Changing a guess makes it your room {}: saved, and kept for next time.",
                        r.yours.len()
                    ));
                }
                Ok(false) => {}
                Err(text) => {
                    self.message = Some(format!(
                        "\"{text}\" is not a distance: type metres, like 5.64."
                    ));
                }
            },
            KeyCode::Char('x') => self.message = Some(r.delete()),
            // In the seat map, [ and ] go back to the standing waves.
            KeyCode::Char('[' | ']') if r.view == PlanView::Seats => {
                r.view = PlanView::Mode;
            }
            KeyCode::Char('[') => r.mode = r.mode.saturating_sub(1),
            KeyCode::Char(']') => r.mode += 1,
            KeyCode::Char('v') => {
                r.view = match r.view {
                    PlanView::Mode => PlanView::Seats,
                    PlanView::Seats => PlanView::Mode,
                };
            }
            KeyCode::Char('g' | 'G') => r.step(if code == KeyCode::Char('g') { 1 } else { -1 }),
            KeyCode::Tab => self.view_channel = 1 - self.view_channel,
            KeyCode::Char('i') => self.open_overlay(Overlay::Info { technical: false }),
            _ => {}
        }
        if self.room.take_changed() {
            self.save_rooms();
        }
        self.refresh_room();
        let listed = self.room.current().map_or(0, |d| d.listed.len());
        self.room.mode = self.room.mode.min(listed.saturating_sub(1));
    }

    /// Saves the user's rooms in the config, and the one shown with the session (so a
    /// review finds the room the measurement was made in).
    pub(super) fn save_rooms(&mut self) {
        if self.simulated() {
            return;
        }
        self.config.rooms.clone_from(&self.room.yours);
        self.config.save();
        let shown = self.room.setup().filter(|_| !self.room.showing_guess());
        if let Some(session) = &self.session
            && let Ok(mut info) = session.load_info()
            && info.room != shown
        {
            info.room = shown;
            if let Err(e) = session.save_info(&info) {
                self.set_error(&e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::roommodel::Room;

    fn setup() -> Setup {
        Setup::typical(Room {
            length: 5.64,
            width: 5.24,
            height: 2.7,
        })
    }

    #[test]
    fn fields_read_and_write_what_they_say() {
        // The user's room: in the right corner, the right speaker 0.7 m from its wall,
        // 1.13 m apart, 0.75 m from the wall behind, an equilateral triangle.
        // A centred setup counts as the left half: 0.7 m goes to the left wall, and m
        // mirrors it into the right corner.
        let mut s = setup();
        Field::SideWall.set(&mut s, 0.7);
        assert!(!on_right(&s));
        s = mirrored(&s);
        Field::Apart.set(&mut s, 1.13);
        Field::SpeakersFromFront.set(&mut s, 0.75);
        Field::SeatFromSpeakers.set(&mut s, 0.98);
        Field::SeatOffCentre.set(&mut s, 0.0);
        assert!(on_right(&s), "{s:?}");
        assert_eq!(Field::SideWall.label(&s), "Right speaker to the right wall");
        assert!((s.speakers[1].y - 4.54).abs() < 1e-9);
        assert!((s.speakers[0].y - 3.41).abs() < 1e-9);
        assert!((s.seat.y - 3.975).abs() < 1e-9);
        assert!((s.seat.x - 1.73).abs() < 1e-9);
        for (f, v) in [
            (Field::SideWall, 0.7),
            (Field::Apart, 1.13),
            (Field::SeatFromSpeakers, 0.98),
            (Field::SeatOffCentre, 0.0),
        ] {
            assert!((f.get(&s) - v).abs() < 1e-9, "{f:?}: {}", f.get(&s));
        }
        // A guess 3 m wide corrected to 5.24 m: the setup stays in its corner, together.
        let mut narrow = s;
        narrow.room.width = 3.0;
        shift_sideways(&mut narrow, 3.0 - 5.24);
        Field::Width.set(&mut narrow, 5.24);
        for (a, b) in narrow
            .speakers
            .iter()
            .chain([&narrow.seat])
            .zip(s.speakers.iter().chain([&s.seat]))
        {
            assert!((a.y - b.y).abs() < 1e-9, "{narrow:?}");
        }
        // Moving the speakers forward takes the seat along.
        Field::SpeakersFromFront.set(&mut s, 0.85);
        assert!((s.seat.x - 1.83).abs() < 1e-9);
        // Nothing ends up outside the room.
        Field::SeatFromSpeakers.set(&mut s, 9.0);
        assert!(s.seat.x < s.room.length);
    }

    #[test]
    fn mirroring_puts_the_setup_against_the_other_wall() {
        let mut s = setup();
        Field::SideWall.set(&mut s, 0.7);
        let m = mirrored(&s);
        assert!(on_right(&m));
        assert_eq!(Field::SideWall.label(&m), "Right speaker to the right wall");
        assert!((Field::SideWall.get(&m) - 0.7).abs() < 1e-9);
        assert!((Field::Apart.get(&m) - Field::Apart.get(&s)).abs() < 1e-9);
        assert!(
            m.speakers[0].y < m.speakers[1].y,
            "the left speaker stays the left one"
        );
        let back = mirrored(&m);
        for (a, b) in back
            .speakers
            .iter()
            .chain([&back.seat])
            .zip(s.speakers.iter().chain([&s.seat]))
        {
            assert!((a.y - b.y).abs() < 1e-9 && a.x == b.x, "{back:?}");
        }
    }

    fn guess_of(setup: Setup) -> Guess {
        Guess {
            setup,
            agreement_db: 2.0,
            balance_error_db: None,
        }
    }

    #[test]
    fn editing_a_guess_makes_a_room_of_your_own() {
        let mut r = RoomState {
            guesses: vec![guess_of(setup())],
            ..RoomState::default()
        };
        assert!(r.showing_guess());
        r.field = 2;
        r.typing = "2,65".into();
        assert_eq!(r.apply_typed(), Ok(true));
        assert_eq!(r.selected, Selected::Yours(0));
        assert_eq!(r.setup().expect("set").room.height, 2.65);
        // The guess itself is untouched.
        assert_eq!(r.guesses[0].setup.room.height, 2.7);
        assert!(r.take_changed());
        assert!(!r.take_changed());
        // Editing your room again changes it, without making another.
        assert!(!r.nudge(0.05));
        assert_eq!(r.yours.len(), 1);
        r.typing = "abc".into();
        assert!(r.apply_typed().is_err());
    }

    #[test]
    fn the_list_holds_your_rooms_then_the_guesses() {
        let mut r = RoomState {
            guesses: vec![guess_of(setup()), guess_of(setup())],
            ..RoomState::default()
        };
        let mine = Setup::typical(Room {
            length: 4.0,
            width: 3.5,
            height: 2.5,
        });
        r.load(vec![mine], None);
        assert_eq!(r.selected, Selected::Yours(0));
        assert_eq!(
            r.entries(),
            vec![Selected::Yours(0), Selected::Guess(0), Selected::Guess(1)]
        );
        r.step(1);
        assert_eq!(r.selected, Selected::Guess(0));
        r.step(-1);
        r.step(-1);
        assert_eq!(r.selected, Selected::Guess(1));
        // Deleting takes two presses, and only your own rooms.
        assert!(r.delete().contains("cannot"));
        r.selected = Selected::Yours(0);
        assert!(r.delete().contains("again"));
        assert!(r.yours.len() == 1);
        assert!(r.delete().contains("Deleted"));
        assert!(r.yours.is_empty());
        assert_eq!(r.selected, Selected::Guess(0));
    }

    #[test]
    fn moves_are_described_from_the_seat() {
        assert_eq!(
            moved(0.2, -0.31, "closer to the speakers", "further back"),
            "20 cm further back and 30 cm to the left"
        );
        assert_eq!(moved(0.01, 0.0, "a", "b"), "");
    }
}
