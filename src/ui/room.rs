//! The room screen: the room seen from above with the speakers, the seat and a standing
//! wave's loud and silent places (or how even the bass is at every seat), the numbers to
//! correct with a tape measure, the standing waves, and what they mean.

use std::hash::{Hash, Hasher};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::graphics::Graphics;
use super::plot::{self, Curve, Series};
use super::raster::{Canvas, Pen};
use super::theme::{Rgb, Theme};
use crate::app::App;
use crate::app::room::{
    Derived, Field, GOOD_AGREEMENT_DB, POOR_AGREEMENT_DB, PlanView, Selected, ringing, trap_advice,
};
use crate::dsp::roommodel::{JUDGED_TO_HZ, Point, Room, RoomMode, Setup, TRUSTED_TO_HZ, aligned};

/// The room model is drawn up to this frequency, Hz.
const MODEL_SHOWN_TO_HZ: f64 = 120.0;

/// Width of the column of numbers and words on the right.
const SIDE_WIDTH: u16 = 52;

/// Draws the room screen in `area`.
pub fn draw(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
    let r = &app.room;
    let Some(d) = r.shown() else {
        let text = if r.setup().is_some() {
            "Those dimensions are not a room (1.5–30 m, at least 1.8 m high): correct them."
        } else {
            "The room appears once both speakers are measured."
        };
        frame.render_widget(Paragraph::new(text).block(panel("Room")), area);
        return;
    };
    let [left, side] =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(SIDE_WIDTH)]).areas(area);
    let [top, chart_area] =
        Layout::vertical([Constraint::Percentage(64), Constraint::Min(8)]).areas(left);
    // The plan is as wide as the room's shape needs (cells are about twice as tall as
    // wide), the words take the rest.
    let room = d.setup.room;
    let max_plan = top.width / 2 + 4;
    let plan_width = ((f64::from(top.height) * 2.0 * room.width / room.length) as u16)
        .saturating_add(8)
        .clamp(24.min(max_plan), max_plan);
    let [plan_area, advice_area] =
        Layout::horizontal([Constraint::Length(plan_width), Constraint::Min(20)]).areas(top);
    let shown_rooms = r.entries().len().min(MAX_ROOMS_SHOWN) as u16;
    let [rooms_area, fields_area, modes_area] = Layout::vertical([
        Constraint::Length(shown_rooms + 2),
        Constraint::Length(Field::ALL.len() as u16 + 2),
        Constraint::Min(4),
    ])
    .areas(side);
    rooms(frame, rooms_area, app);

    let mode = d.listed.get(r.mode).copied();
    plan(frame, plan_area, d, mode, r.view, gfx);
    chart(frame, chart_area, d, app, gfx);
    fields(frame, fields_area, app);
    modes(frame, modes_area, d, app);
    let advice = advice(d, app);
    frame.render_widget(
        Paragraph::new(advice)
            .block(panel("What it means"))
            .wrap(Wrap { trim: true }),
        advice_area,
    );
}

fn panel(title: &str) -> Block<'_> {
    Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {title} "),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

/// A short name for a standing wave: "side–side 5.24 m", "front–back + side–side".
fn short(mode: &RoomMode, room: &Room) -> String {
    let names = ["front–back", "side–side", "up–down"];
    let lengths = [room.length, room.width, room.height];
    let axes: Vec<usize> = (0..3).filter(|&i| mode.order[i] > 0).collect();
    match axes.as_slice() {
        [i] if mode.order[*i] == 1 => format!("{} {:.2} m", names[*i], lengths[*i]),
        [i] => format!("{} {:.2} m ×{}", names[*i], lengths[*i], mode.order[*i]),
        [a, b] => format!("{} + {}", names[*a], names[*b]),
        _ => "all three ways".to_owned(),
    }
}

/// Most rooms listed at once.
const MAX_ROOMS_SHOWN: usize = 6;

/// "left", "middle" or "right": where the speakers are along the wall behind them.
fn side(s: &Setup) -> &'static str {
    let mid = (s.speakers[0].y + s.speakers[1].y) / 2.0;
    if mid < s.room.width / 3.0 {
        "left"
    } else if mid > s.room.width * 2.0 / 3.0 {
        "right"
    } else {
        "middle"
    }
}

/// The user's rooms and the guesses, the one shown marked.
fn rooms(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let r = &app.room;
    let all = r.entries();
    let now = all.iter().position(|e| *e == r.selected).unwrap_or(0);
    // Scrolled so the one shown is on screen.
    let first = now.saturating_sub(MAX_ROOMS_SHOWN - 1);
    let lines: Vec<Line> = all
        .iter()
        .enumerate()
        .skip(first)
        .take(MAX_ROOMS_SHOWN)
        .map(|(i, e)| {
            let (name, setup, fit) = match *e {
                Selected::Yours(k) => (
                    format!("Yours {}", k + 1),
                    r.yours[k],
                    r.yours_fit.get(k).copied().flatten(),
                ),
                Selected::Guess(k) => (
                    format!("Guess {}", k + 1),
                    r.guesses[k].setup,
                    Some(r.guesses[k].agreement_db),
                ),
            };
            let room = setup.room;
            let fit = fit.map_or(String::new(), |f| format!("±{f:.1} dB"));
            let text = format!(
                "{} {name:<8}{:>5.2}×{:.2}×{:.2} m {fit:>8}  {:<6}",
                if i == now { "▶" } else { " " },
                room.length,
                room.width,
                room.height,
                side(&setup)
            );
            let style = match (i == now, e) {
                (true, _) => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                (false, Selected::Guess(_)) => Style::default().fg(Color::DarkGray),
                (false, Selected::Yours(_)) => Style::default(),
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(panel("Rooms   g/G      size           fit   speakers")),
        area,
    );
}

/// The numbers to correct with a tape measure.
fn fields(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let r = &app.room;
    let Some(setup) = r.setup() else { return };
    let lines: Vec<Line> = Field::ALL
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let selected = i == r.field;
            let value = if selected && !r.typing.is_empty() {
                format!("{}▏ m", r.typing)
            } else {
                format!("{:.2} m", f.get(&setup))
            };
            let label = f.label(&setup);
            let width = usize::from(SIDE_WIDTH).saturating_sub(4 + label.chars().count() + 1);
            let text = format!(
                "{} {label}{value:>width$}",
                if selected { "▶" } else { " " }
            );
            let style = if selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    let title = match r.selected {
        Selected::Guess(k) => format!("Guess {}: change a number to make it yours", k + 1),
        Selected::Yours(k) => format!("Your room {}, saved", k + 1),
    };
    let block = panel(&title).border_style(if r.showing_guess() {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    });
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The standing waves: measured ones with how long they ring, then the strongest others.
fn modes(frame: &mut Frame<'_>, area: Rect, d: &Derived, app: &App) {
    let room = d.setup.room;
    let typical = app.room_reverb_s();
    let lines: Vec<Line> = d
        .listed
        .iter()
        .enumerate()
        .take(usize::from(area.height.saturating_sub(2)))
        .map(|(i, m)| {
            let measured = d
                .named
                .iter()
                .find(|n| n.mode.is_some_and(|nm| nm.order == m.order));
            let t60 = measured.map_or("–".to_owned(), |n| {
                n.measured.t60_text().replace(" s", "")
            });
            let color = match measured {
                Some(n) if n.measured.t60_s > 2.0 * typical => Color::LightRed,
                Some(n) if n.measured.t60_s > 1.5 * typical => Color::Yellow,
                Some(_) => Color::Reset,
                None => Color::DarkGray,
            };
            let mark = if i == app.room.mode { "▶" } else { " " };
            Line::from(Span::styled(
                format!(
                    "{mark}{:>6.1} Hz {:<8}{:<22}{t60:>8}",
                    m.hz,
                    m.label(),
                    short(m, &room)
                ),
                Style::default().fg(color),
            ))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(panel("Standing waves    T60 s   [ ] plan")),
        area,
    );
}

/// What the model says, in sentences.
fn advice(d: &Derived, app: &App) -> Vec<Line<'static>> {
    let r = &app.room;
    let s = d.setup;
    let typical = app.room_reverb_s();
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Selected::Guess(k) = r.selected {
        lines.push(Line::from(Span::styled(
            format!(
                "Guess {} of {}, from the measurement alone: about {:.1} × {:.1} m, {}. The \
                 height and the speakers' distance from the wall are the least certain. Measure \
                 your room and change the numbers: the guess then becomes your room, saved for \
                 next time.",
                k + 1,
                r.guesses.len(),
                s.room.length,
                s.room.width,
                where_speakers(&s)
            ),
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(""));
    }
    let agreement: Vec<f64> = d.agreement_db.iter().flatten().copied().collect();
    let worst = agreement.iter().copied().fold(0.0, f64::max);
    if let [a, b] = agreement.as_slice() {
        let (verdict, color) = if worst <= GOOD_AGREEMENT_DB {
            ("a good match", Color::Green)
        } else if worst <= POOR_AGREEMENT_DB {
            (
                "a rough match, trust it for the lowest notes",
                Color::Yellow,
            )
        } else {
            ("no match: check the numbers", Color::LightRed)
        };
        lines.push(Line::from(Span::styled(
            format!(
                "Model against your measurement below {TRUSTED_TO_HZ:.0} Hz: ±{a:.1} dB (left), \
                 ±{b:.1} dB (right), {verdict}."
            ),
            Style::default().fg(color),
        )));
        lines.push(Line::from(""));
    }
    if let Some(line) = balance_line(d) {
        lines.push(line);
        lines.push(Line::from(""));
    }
    for n in ringing(&d.named, typical).into_iter().take(2) {
        let Some(m) = n.mode else { continue };
        let at = [0, 1].map(|c| m.shape(&s.room, s.speakers[c]).abs());
        let mut text = format!(
            "{:.0} Hz rings {} (the room {typical:.1} s), {}. Traps: {}.",
            n.measured.hz,
            n.measured.t60_text(),
            m.describe(&s.room),
            trap_advice(&m)
        );
        let (strong, weak) = if at[1] > at[0] { (1, 0) } else { (0, 1) };
        if at[strong] > 1.4 * at[weak].max(0.05) {
            let name = ["left", "right"];
            text.push_str(&format!(
                " The {} speaker drives it {:.1}× harder than the {}.",
                name[strong],
                at[strong] / at[weak].max(0.05),
                name[weak]
            ));
        }
        lines.push(Line::from(text));
        lines.push(Line::from(""));
    }
    if worst > POOR_AGREEMENT_DB {
        lines.push(Line::from(Span::styled(
            "Suggestions wait until the model matches the measurement.",
            Style::default().fg(Color::DarkGray),
        )));
        return lines;
    }
    let mut suggested = false;
    for sug in [&d.seat, &d.shift].into_iter().flatten() {
        suggested = true;
        lines.push(Line::from(vec![
            Span::styled("Predicted: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "{}: bass below {JUDGED_TO_HZ:.0} Hz ±{:.1} dB instead of ±{:.1} dB. Check it with \
                 live placement (p on the measure step).",
                capitalise(&sug.what),
                sug.then_db,
                sug.now_db
            )),
        ]));
        lines.push(Line::from(""));
    }
    if !suggested {
        lines.push(Line::from(format!(
            "Your seat is about as even as any within 60 cm, and moving the whole setup would \
             not help much (±{:.1} dB below {JUDGED_TO_HZ:.0} Hz).",
            d.seat_map.at(s.seat.x, s.seat.y)
        )));
    }
    lines
}

/// Balance error that suggests the speakers are on the other side, dB.
const WRONG_SIDE_DB: f64 = 4.0;

/// The left speaker against the right one at the room's lowest waves, measured and as this
/// layout has it: which side of the room the speakers are on shows there.
fn balance_line(d: &Derived) -> Option<Line<'static>> {
    let err = d.balance_error_db?;
    let measured: Vec<String> = d
        .balance
        .iter()
        .map(|(hz, db)| format!("{db:+.0} dB at {hz:.0} Hz"))
        .collect();
    let text = format!(
        "Left speaker against the right at the lowest waves: {} (measured).",
        measured.join(", ")
    );
    Some(if err > WRONG_SIDE_DB {
        Line::from(Span::styled(
            format!(
                "{text} This layout has that {err:.0} dB wrong: is the setup on the other side \
                 of the room, or are left and right swapped?"
            ),
            Style::default().fg(Color::LightRed),
        ))
    } else {
        Line::from(Span::styled(
            format!("{text} This layout agrees within {err:.1} dB."),
            Style::default().fg(Color::DarkGray),
        ))
    })
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or(String::new(), |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

/// Where the speakers are, in words: "the speakers in the front right corner".
fn where_speakers(s: &Setup) -> String {
    let mid = (s.speakers[0].y + s.speakers[1].y) / 2.0;
    let side = if mid < s.room.width / 3.0 {
        "left"
    } else if mid > s.room.width * 2.0 / 3.0 {
        "right"
    } else {
        ""
    };
    if side.is_empty() {
        "the speakers near the middle of the wall behind them".to_owned()
    } else {
        format!("the speakers towards the {side} of the wall behind them")
    }
}

/// The bass at the seat, measured and as the model has it, for the speaker in view.
fn chart(frame: &mut Frame<'_>, area: Rect, d: &Derived, app: &App, gfx: &mut Graphics) {
    let c = app.view_channel.min(1);
    let Some(responses) = app.measure.responses[c].as_ref() else {
        return;
    };
    let range = (20.0, 200.0);
    let keep = |r: &crate::dsp::spectrum::Response| {
        let pairs: Vec<(f64, f64)> = r
            .freqs
            .iter()
            .zip(&r.db)
            .filter(|(f, _)| (range.0..=range.1).contains(*f))
            .map(|(f, d)| (*f, *d))
            .collect();
        pairs.into_iter().unzip::<f64, f64, Vec<f64>, Vec<f64>>()
    };
    let (mf, md) = keep(&responses.display);
    let predicted = &d.predicted[c];
    let model = aligned(&responses.display, &predicted.freqs, &predicted.db);
    // Drawn only where it can be trusted with a little to spare.
    let (pf, pd): (Vec<f64>, Vec<f64>) = predicted
        .freqs
        .iter()
        .zip(&model)
        .filter(|(f, _)| (range.0..=MODEL_SHOWN_TO_HZ).contains(*f))
        .map(|(f, d)| (*f, *d))
        .unzip();
    let series = if c == 0 { Series::Left } else { Series::Right };
    let curves = vec![
        Curve::new("measured", series, &mf, &md),
        Curve::new("room model", Series::Predicted, &pf, &pd),
    ];
    let name = ["Left", "Right"][c];
    plot::draw_axes(
        frame,
        area,
        &format!("{name} speaker at your seat: measured and the room model (Tab: other speaker)"),
        &curves,
        &[],
        range,
        plot::y_range(&curves, range),
        gfx,
    );
}

/// The room from above, as an image, or as coloured cells where images are not supported.
fn plan(
    frame: &mut Frame<'_>,
    area: Rect,
    d: &Derived,
    mode: Option<RoomMode>,
    view: PlanView,
    gfx: &mut Graphics,
) {
    let title = match (view, mode) {
        (PlanView::Mode, Some(m)) => {
            let rings = d
                .named
                .iter()
                .find(|n| n.mode.is_some_and(|nm| nm.order == m.order))
                .map_or(String::new(), |n| {
                    format!(", rings {}", n.measured.t60_text())
                });
            format!("{:.1} Hz, {}{rings}", m.hz, short(&m, &d.setup.room))
        }
        _ => format!("Where to sit: bass below {JUDGED_TO_HZ:.0} Hz"),
    };
    let block = panel(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if gfx.images() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        setup_hash(&d.setup, &mut hasher);
        mode.map(|m| m.order).hash(&mut hasher);
        (view == PlanView::Seats).hash(&mut hasher);
        let content = hasher.finish();
        let d = d.clone();
        gfx.chart(
            frame,
            inner,
            "room-plan",
            content,
            move |canvas, theme, font| {
                draw_plan(canvas, theme, font, &d, mode, view);
            },
        );
    } else {
        plan_cells(frame, inner, d, mode, view, gfx.theme());
    }
}

fn setup_hash(s: &Setup, h: &mut impl Hasher) {
    let r = s.room;
    for v in [r.length, r.width, r.height]
        .into_iter()
        .chain(s.speakers.iter().flat_map(|p| [p.x, p.y, p.z]))
        .chain([s.seat.x, s.seat.y, s.seat.z])
    {
        v.to_bits().hash(h);
    }
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (f64::from(x) * (1.0 - t) + f64::from(y) * t).round() as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// The colour of a place on the plan: the mode's loudness (red and blue for its two
/// phases), or how even the seat is (green even, red uneven), over the background.
/// The colour of the floor at (`x`, `y`); `range` is [`map_range`] of the seat map, worked
/// out once per drawing rather than per cell.
fn heat(
    d: &Derived,
    (mode, view, range): (Option<RoomMode>, PlanView, (f64, f64)),
    theme: &Theme,
    x: f64,
    y: f64,
) -> Rgb {
    let bg = theme.background;
    match (view, mode) {
        (PlanView::Mode, Some(m)) => {
            let v = m.shape(
                &d.setup.room,
                Point {
                    x,
                    y,
                    z: d.setup.seat.z,
                },
            );
            let c = if v >= 0.0 { theme.red } else { theme.blue };
            mix(bg, c, v.abs() * 0.6)
        }
        _ => {
            let (lo, hi) = range;
            let t = ((d.seat_map.at(x, y) - lo) / (hi - lo).max(0.1)).clamp(0.0, 1.0);
            let c = if t < 0.5 {
                mix(theme.green, theme.yellow, t * 2.0)
            } else {
                mix(theme.yellow, theme.red, (t - 0.5) * 2.0)
            };
            mix(bg, c, 0.55)
        }
    }
}

/// The range of the seat map shown, dB: its 5th to 95th percentile, so one extreme corner
/// does not wash out the rest.
fn map_range(d: &Derived) -> (f64, f64) {
    let mut v = d.seat_map.values.clone();
    v.sort_by(f64::total_cmp);
    let at = |q: f64| v[((v.len() - 1) as f64 * q) as usize];
    (at(0.05), at(0.95))
}

/// A stretch of wall, `(x, y)` to `(x, y)` in m, with how loud a mode is there (0 to 1).
type WallStretch = ((f64, f64), (f64, f64), f64);

/// How loud `mode` is along the walls, for marking where traps work.
fn along_walls(room: &Room, mode: &RoomMode, z: f64) -> Vec<WallStretch> {
    let steps = 40;
    let mut out = Vec::new();
    let walls = [
        ((0.0, 0.0), (0.0, room.width)),
        ((room.length, 0.0), (room.length, room.width)),
        ((0.0, 0.0), (room.length, 0.0)),
        ((0.0, room.width), (room.length, room.width)),
    ];
    for (a, b) in walls {
        for k in 0..steps {
            let t0 = k as f64 / steps as f64;
            let t1 = (k + 1) as f64 / steps as f64;
            let p0 = (a.0 + (b.0 - a.0) * t0, a.1 + (b.1 - a.1) * t0);
            let p1 = (a.0 + (b.0 - a.0) * t1, a.1 + (b.1 - a.1) * t1);
            let mid = Point {
                x: (p0.0 + p1.0) / 2.0,
                y: (p0.1 + p1.1) / 2.0,
                z,
            };
            out.push((p0, p1, mode.shape(room, mid).abs()));
        }
    }
    out
}

/// The plan as an image.
fn draw_plan(
    canvas: &mut Canvas,
    theme: &Theme,
    font: Option<&ab_glyph::FontVec>,
    d: &Derived,
    mode: Option<RoomMode>,
    view: PlanView,
) {
    let s = d.setup;
    let room = s.room;
    let px = canvas.label();
    let side_label = format!("{:.2} m", room.length);
    let margin_x = Canvas::text_width(font, &side_label, px) + px;
    let margin_top = px * 2.6;
    let margin_bottom = px * 4.0;
    let scale = ((f64::from(canvas.w) - 2.0 * f64::from(margin_x)) / room.width)
        .min((f64::from(canvas.h) - f64::from(margin_top + margin_bottom)) / room.length)
        .max(1.0);
    let ox = (f64::from(canvas.w) - room.width * scale) / 2.0;
    let oy = f64::from(margin_top)
        + (f64::from(canvas.h - margin_top - margin_bottom) - room.length * scale) / 2.0;
    let at = |x: f64, y: f64| ((ox + y * scale) as f32, (oy + x * scale) as f32);

    // The floor, coloured.
    let range = map_range(d);
    let cell = (px / 3.0).max(3.0);
    let (w_px, h_px) = ((room.width * scale) as f32, (room.length * scale) as f32);
    let mut py = 0.0f32;
    while py < h_px {
        let mut pxx = 0.0f32;
        while pxx < w_px {
            let x = f64::from(py + cell / 2.0) / scale;
            let y = f64::from(pxx + cell / 2.0) / scale;
            let c = heat(
                d,
                (mode, view, range),
                theme,
                x.min(room.length),
                y.min(room.width),
            );
            let (cx, cy) = at(0.0, 0.0);
            canvas.rect(cx + pxx, cy + py, cell + 1.0, cell + 1.0, c, 1.0);
            pxx += cell;
        }
        py += cell;
    }

    // A mode's silent lines, dashed.
    if let (PlanView::Mode, Some(m)) = (view, mode) {
        let pen = Pen::dashed(theme.muted, 0.7, 1.2, px * 0.5);
        for k in 0..m.order[0] {
            let x = (f64::from(k) + 0.5) * room.length / f64::from(m.order[0]);
            canvas.line(at(x, 0.0), at(x, room.width), pen);
        }
        for k in 0..m.order[1] {
            let y = (f64::from(k) + 0.5) * room.width / f64::from(m.order[1]);
            canvas.line(at(0.0, y), at(room.length, y), pen);
        }
        // Where traps work: along the walls where the mode is loud.
        for (p0, p1, v) in along_walls(&room, &m, s.seat.z) {
            if v > 0.7 {
                canvas.line(
                    at(p0.0, p0.1),
                    at(p1.0, p1.1),
                    Pen::solid(theme.green, ((v - 0.7) / 0.3) as f32 * 0.9 + 0.1, px * 0.55),
                );
            }
        }
    }

    // The walls.
    let corners = [
        at(0.0, 0.0),
        at(0.0, room.width),
        at(room.length, room.width),
        at(room.length, 0.0),
        at(0.0, 0.0),
    ];
    let wall_points: Vec<Option<(f32, f32)>> = corners.iter().map(|&p| Some(p)).collect();
    canvas.polyline(
        &wall_points,
        Pen::solid(theme.foreground, 0.9, (px / 6.0).max(2.0)),
    );

    // The most even seat nearby, in the seat view.
    if view == PlanView::Seats
        && let Some((bx, by, _)) = d.seat_map.best_near(s.seat.x, s.seat.y, 0.6)
    {
        let (cx, cy) = at(bx, by);
        let r = (0.22 * scale) as f32;
        let ring: Vec<Option<(f32, f32)>> = (0..=32)
            .map(|k| {
                let a = k as f32 / 32.0 * std::f32::consts::TAU;
                Some((cx + r * a.cos(), cy + r * a.sin()))
            })
            .collect();
        canvas.polyline(
            &ring,
            Pen::dashed(theme.green, 1.0, (px / 7.0).max(1.5), px * 0.4),
        );
    }

    // The speakers, turned towards the seat, and the seat.
    for (i, p) in s.speakers.iter().enumerate() {
        let (dx, dy) = (s.seat.x - p.x, s.seat.y - p.y);
        let len = dx.hypot(dy).max(1e-6);
        let (fx, fy) = (dx / len, dy / len);
        let (ux, uy) = (-fy, fx);
        let (w, depth) = (0.22, 0.26);
        let corner = |a: f64, b: f64| at(p.x + fx * a + ux * b, p.y + fy * a + uy * b);
        let body = [
            corner(depth / 2.0, -w / 2.0),
            corner(depth / 2.0, w / 2.0),
            corner(-depth / 2.0, w / 2.0),
            corner(-depth / 2.0, -w / 2.0),
        ];
        canvas.fill(&body, theme.foreground, 0.95);
        canvas.line(
            corner(depth / 2.0, -w / 2.0),
            corner(depth / 2.0, w / 2.0),
            Pen::solid(theme.accent, 1.0, (px / 5.0).max(2.0)),
        );
        let label = ["L", "R"][i];
        let (lx, ly) = corner(-depth / 2.0 - 0.18, 0.0);
        let tw = Canvas::text_width(font, label, px);
        canvas.text(
            font,
            label,
            (lx - tw / 2.0, ly - px / 2.0),
            px,
            theme.foreground,
            1.0,
        );
    }
    let (cx, cy) = at(s.seat.x, s.seat.y);
    let r = (0.17 * scale) as f32;
    let disc: Vec<(f32, f32)> = (0..24)
        .map(|k| {
            let a = k as f32 / 24.0 * std::f32::consts::TAU;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    canvas.fill(&disc, theme.accent, 0.95);
    let you = "you";
    let tw = Canvas::text_width(font, you, px);
    canvas.text(
        font,
        you,
        (cx - tw / 2.0, cy + r + px * 0.2),
        px,
        theme.foreground,
        1.0,
    );

    // Labels: the wall behind the speakers, the dimensions, the legend.
    let top = format!("wall behind the speakers, {:.2} m", room.width);
    let tw = Canvas::text_width(font, &top, px);
    let (tx, ty) = at(0.0, room.width / 2.0);
    canvas.text(
        font,
        &top,
        (tx - tw / 2.0, ty - px * 1.6),
        px,
        theme.muted,
        1.0,
    );
    let (sx, sy) = at(room.length * 0.3, 0.0);
    let sw = Canvas::text_width(font, &side_label, px);
    canvas.text(
        font,
        &side_label,
        (sx - sw - px * 0.5, sy - px / 2.0),
        px,
        theme.muted,
        1.0,
    );
    let legend: [&str; 2] = match view {
        PlanView::Mode => [
            "red, blue: loud, in opposite phase   dark: silent",
            "green along the walls: where traps work",
        ],
        PlanView::Seats => [
            "if you sat here: green even bass, red boomy or thin",
            "speakers where they are · ring: best seat within 60 cm",
        ],
    };
    let (lx, ly) = at(room.length, room.width / 2.0);
    // Smaller text when the plan is narrow, so the legend fits.
    let widest = legend
        .iter()
        .map(|t| Canvas::text_width(font, t, px))
        .fold(0.0f32, f32::max);
    let lpx = px * (canvas.w * 0.95 / widest.max(1.0)).min(1.0);
    for (k, text) in legend.iter().enumerate() {
        let lw = Canvas::text_width(font, text, lpx);
        let y = ly + px * 0.7 + lpx * 1.3 * k as f32;
        canvas.text(font, text, (lx - lw / 2.0, y), lpx, theme.muted, 1.0);
    }
}

/// The plan as coloured terminal cells, for terminals without images.
fn plan_cells(
    frame: &mut Frame<'_>,
    area: Rect,
    d: &Derived,
    mode: Option<RoomMode>,
    view: PlanView,
    theme: &Theme,
) {
    let s = d.setup;
    let room = s.room;
    if area.width < 4 || area.height < 3 {
        return;
    }
    // A cell is about twice as tall as wide.
    let per_col = (room.width / f64::from(area.width - 2))
        .max(room.length / (2.0 * f64::from(area.height - 2)));
    let cols = ((room.width / per_col).round() as u16).clamp(1, area.width);
    let rows = ((room.length / (2.0 * per_col)).round() as u16).clamp(1, area.height);
    let x0 = area.x + (area.width - cols) / 2;
    let y0 = area.y + (area.height - rows) / 2;
    let to_cell = |p: Point| {
        let c = ((p.y / room.width) * f64::from(cols))
            .floor()
            .clamp(0.0, f64::from(cols - 1)) as u16;
        let r = ((p.x / room.length) * f64::from(rows))
            .floor()
            .clamp(0.0, f64::from(rows - 1)) as u16;
        (x0 + c, y0 + r)
    };
    let range = map_range(d);
    let buf = frame.buffer_mut();
    for r in 0..rows {
        for c in 0..cols {
            let x = (f64::from(r) + 0.5) / f64::from(rows) * room.length;
            let y = (f64::from(c) + 0.5) / f64::from(cols) * room.width;
            let color = heat(d, (mode, view, range), theme, x, y);
            if let Some(cell) = buf.cell_mut((x0 + c, y0 + r)) {
                cell.set_bg(color.tui());
                cell.set_symbol(" ");
            }
        }
    }
    for (i, p) in s.speakers.iter().enumerate() {
        if let Some(cell) = buf.cell_mut(to_cell(*p)) {
            cell.set_symbol(["L", "R"][i]);
            cell.set_fg(theme.foreground.tui());
        }
    }
    if let Some(cell) = buf.cell_mut(to_cell(s.seat)) {
        cell.set_symbol("◉");
        cell.set_fg(theme.accent.tui());
    }
}
