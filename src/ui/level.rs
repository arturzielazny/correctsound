//! Step 4's screens: the room's background noise and the level it calls for, then the
//! meter for setting the amplifier.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Gauge, Paragraph, Wrap};

use super::{bad, bignum, ok, panel, warn};
use crate::app::level::{ABORT_SPL, COMFORT_MAX_SPL, Reading, TOLERANCE};
use crate::app::{App, LevelPhase};
use crate::audio::Channel;

pub(super) fn level(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let l = &app.level;
    if l.phase == LevelPhase::Listening {
        listening(frame, area, app);
        return;
    }
    let target = l.target_spl;
    let [info, meter, details] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Min(4),
    ])
    .areas(area);
    let instructions = vec![
        Line::from(format!(
            "Pink noise (500–2000 Hz) plays from one speaker. Turn your amplifier until the meter \
             reads {target:.0} dB: the level your room's background noise calls for. Louder is \
             fine up to {COMFORT_MAX_SPL:.0} dB."
        )),
        Line::from(
            "Put the mic where your head is when listening, at ear height, pointing as chosen in step 2.",
        ),
        Line::from("No amplifier knob? Use ↑/↓ to change the output volume instead."),
    ];
    frame.render_widget(
        Paragraph::new(instructions)
            .block(panel("Set the level"))
            .wrap(Wrap { trim: true }),
        info,
    );

    let title = match &l.phase {
        LevelPhase::Listening => "Listening to the room…".to_owned(),
        LevelPhase::Playing if l.fading_in() => {
            format!("{} speaker — fading in…", l.channel.name())
        }
        LevelPhase::Playing => format!("{} speaker", l.channel.name()),
        LevelPhase::Stopped(_) => "Stopped".to_owned(),
        LevelPhase::Idle => "Idle".to_owned(),
    };
    let spl = l.spl.filter(|_| l.phase == LevelPhase::Playing);
    // The scale runs from 20 dB below the target to the comfortable maximum (at least 10
    // dB above the target).
    let scale_lo = target - 20.0;
    let scale_hi = (target + 10.0).max(COMFORT_MAX_SPL);
    let (ratio, color, label) = match spl {
        Some(s) => {
            let ratio = ((s - scale_lo) / (scale_hi - scale_lo)).clamp(0.0, 1.0);
            let (color, hint) = match Reading::of(s, target) {
                Reading::TooQuiet => (Color::Yellow, format!("  turn up {:.1} dB", target - s)),
                Reading::Right => (Color::Green, "  ✔ good".to_owned()),
                Reading::LouderThanNeeded => (
                    Color::Green,
                    format!("  ✔ fine, {:.0} dB more than needed", s - target),
                ),
                Reading::TooLoud => (
                    Color::Yellow,
                    format!("  loud: turn down {:.1} dB", s - COMFORT_MAX_SPL),
                ),
            };
            (ratio, color, format!("{s:.1} dB SPL{hint}"))
        }
        None => (0.0, Color::DarkGray, "—".to_owned()),
    };
    let [meter_title, gauge, scale] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .margin(1)
    .areas(meter);
    frame.render_widget(panel("Meter"), meter);
    frame.render_widget(
        Paragraph::new(title).alignment(Alignment::Center),
        meter_title,
    );
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(color).bg(Color::Black))
            .ratio(ratio)
            .label(Span::styled(label, Style::default().bold())),
        gauge,
    );
    // Scale under the gauge: the target marked, and the fine range from just below it to
    // the comfortable maximum bracketed.
    let width = scale.width as usize;
    let mut marks = vec![' '; width];
    let pos = |db: f64| (((db - scale_lo) / (scale_hi - scale_lo)) * width as f64).round() as usize;
    for (db, ch) in [
        (target - TOLERANCE, '['),
        (target, '▲'),
        (COMFORT_MAX_SPL.max(target + TOLERANCE), ']'),
    ] {
        if let Some(m) = marks.get_mut(pos(db).min(width.saturating_sub(1))) {
            *m = ch;
        }
    }
    let scale_text: String = marks.into_iter().collect();
    frame.render_widget(Paragraph::new(scale_text).fg(Color::Green), scale);

    let mut lines = Vec::new();
    if let LevelPhase::Stopped(reason) = &l.phase {
        lines.push(Line::from(bad(reason)));
    }
    match spl.map(|s| Reading::of(s, target)) {
        Some(Reading::LouderThanNeeded) => lines.push(Line::from(ok(&format!(
            "Louder than this room needs ({target:.0} dB), which is fine: the sweeps only gain \
             margin over the background. Turn down if it is uncomfortable."
        )))),
        Some(Reading::TooLoud) => lines.push(Line::from(warn(&format!(
            "Louder than {COMFORT_MAX_SPL:.0} dB: tiring over a whole measurement, and small \
             speakers start to distort in the bass. {target:.0} dB is enough here."
        )))),
        _ => {}
    }
    if let Some(n) = &l.room_noise {
        let groups: Vec<String> = NOISE_GROUPS
            .iter()
            .map(|g| format!("{} {:.0}", g.name.to_lowercase(), group_level(n, g)))
            .collect();
        lines.push(Line::from(format!(
            "Background noise: {} dB SPL (l listens again)",
            groups.join(", ")
        )));
    }
    if let Some(a) = l.ambient_spl {
        let text = format!("In the meter's band the background is {a:.0} dB SPL");
        lines.push(Line::from(if l.room_is_noisy() {
            warn(&format!(
                "{text}, close enough to the noise to raise the reading a little"
            ))
        } else {
            ok(&text)
        }));
    }
    if let Some(v) = l.sink_volume {
        lines.push(Line::from(format!("Output volume: {v:.2}")));
    }
    let heard: Vec<String> = Channel::BOTH
        .iter()
        .filter_map(|c| l.heard[c.index()].map(|s| format!("{} {s:.1} dB", c.name())))
        .collect();
    if !heard.is_empty() {
        lines.push(Line::from(format!("Last readings: {}", heard.join(", "))));
    }
    if let [Some(a), Some(b)] = l.heard {
        lines.push(Line::from(format!(
            "Left/right difference {:+.1} dB — the EQ will even this out.",
            a - b
        )));
    }
    if app.spl_is_approximate() {
        lines.push(Line::from(warn(
            "SPL readings are approximate for this mic.",
        )));
    }
    lines.push(Line::from(Span::styled(
        format!("Playback stops automatically above {ABORT_SPL:.0} dB SPL."),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Status"))
            .wrap(Wrap { trim: true }),
        details,
    );
}

/// A range of octaves shown as one line of the background noise.
pub(super) struct NoiseGroup {
    name: &'static str,
    range: &'static str,
    /// Indices into [`crate::dsp::ambient::OCTAVES`].
    octaves: std::ops::RangeInclusive<usize>,
    /// What usually makes noise there.
    sources: &'static str,
}

pub(super) const NOISE_GROUPS: [NoiseGroup; 3] = [
    NoiseGroup {
        name: "Bass",
        range: "20–180 Hz",
        octaves: 0..=2,
        sources: "traffic, ventilation, fridges, the building",
    },
    NoiseGroup {
        name: "Mids",
        range: "180 Hz–1.4 kHz",
        octaves: 3..=5,
        sources: "voices, fans, appliances",
    },
    NoiseGroup {
        name: "Highs",
        range: "1.4–20 kHz",
        octaves: 6..=9,
        sources: "hiss, computer fans, birds",
    },
];

/// A group's level: the power sum of its octaves, dB SPL.
pub(super) fn group_level(noise: &crate::dsp::ambient::OctaveLevels, g: &NoiseGroup) -> f64 {
    crate::dsp::power_to_db(
        noise[g.octaves.clone()]
            .iter()
            .map(|&d| crate::dsp::db_to_power(d))
            .sum(),
    )
}

/// What a measurement level means, in words and colour.
pub(super) fn level_verdict(needs: f64) -> (&'static str, Color) {
    use crate::dsp::ambient::{MAX_LEVEL_SPL, MIN_LEVEL_SPL};
    if needs <= MIN_LEVEL_SPL {
        ("quiet", Color::Green)
    } else if needs <= 70.0 {
        ("some noise", Color::Yellow)
    } else if needs <= MAX_LEVEL_SPL {
        ("noisy", Color::LightRed)
    } else {
        ("too noisy", Color::Red)
    }
}

/// Step 4, first part: listening to the room, live, and the level it calls for.
pub(super) fn listening(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::dsp::ambient::{MARGIN_GOAL_DB, MAX_LEVEL_SPL, MIN_LEVEL_SPL};
    let l = &app.level;
    let [info, noise, suggestion] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Min(10),
    ])
    .areas(area);
    let intro = vec![
        Line::from(Span::styled(
            "Keep quiet for a few seconds: CorrectSound is listening to your room.",
            Style::default().bold(),
        )),
        Line::from(format!(
            "Its background noise decides how loud the measurement has to be: every octave \
             should end up {MARGIN_GOAL_DB:.0} dB above it. A quiet room can be measured \
             quietly, at {MIN_LEVEL_SPL:.0} dB; a noisy one needs up to {MAX_LEVEL_SPL:.0}. \
             The suggestion follows the room as long as you listen."
        )),
        Line::from(
            "Put the mic where your head is when listening, at ear height, pointing as chosen in step 2.",
        ),
    ];
    frame.render_widget(
        Paragraph::new(intro)
            .block(panel("Listen to the room"))
            .wrap(Wrap { trim: true }),
        info,
    );

    let mut rows = Vec::new();
    match (&l.room_noise, &l.suggestion) {
        (Some(n), Some(s)) => {
            // Bars from 0 to 70 dB SPL.
            let bar_width = 24usize;
            for g in &NOISE_GROUPS {
                let level = group_level(n, g);
                let needs = s.needs[g.octaves.clone()]
                    .iter()
                    .flatten()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                let (word, color) = level_verdict(needs);
                let filled = ((level / 70.0).clamp(0.0, 1.0) * bar_width as f64).round() as usize;
                rows.push(Line::from(vec![
                    Span::styled(format!("{:<6}", g.name), Style::default().bold()),
                    Span::raw(format!("{:<15}", g.range)),
                    Span::raw(format!("{level:>4.0} dB SPL  ")),
                    Span::styled("█".repeat(filled), Style::default().fg(color)),
                    Span::styled(
                        "░".repeat(bar_width - filled),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("  {word:<11}"), Style::default().fg(color)),
                    Span::styled(g.sources, Style::default().fg(Color::DarkGray)),
                ]));
            }
            rows.push(Line::from(""));
            rows.push(Line::from(Span::styled(
                "Levels are the steady background over the last 3 seconds; a cough or a car \
                 passing does not count.",
                Style::default().fg(Color::DarkGray),
            )));
        }
        _ => rows.push(Line::from(format!("Listening… {:.1} s", l.listened_secs()))),
    }
    frame.render_widget(
        Paragraph::new(rows)
            .block(panel("Background noise"))
            .wrap(Wrap { trim: false }),
        noise,
    );

    // The big number is drawn without wrapping: trimming would strip the spaces that place
    // each row of a digit, turning a 7 into a T.
    let mut big = Vec::new();
    let mut lines = Vec::new();
    if let Some(s) = &l.suggestion {
        let color = level_verdict(s.level_spl).1;
        let color = if color == Color::Red {
            Color::LightRed
        } else {
            color
        };
        big.extend(bignum::lines(
            &format!("{:.0}", s.level_spl),
            Style::default().fg(color),
        ));
        big.push(Line::from(Span::styled(
            "dB SPL",
            Style::default().fg(color),
        )));
        big.push(Line::from(""));
        let octave = if s.limited_by_hz < 1000.0 {
            format!("{:.0} Hz", s.limited_by_hz)
        } else {
            format!("{:.0} kHz", s.limited_by_hz / 1000.0)
        };
        lines.push(Line::from(if s.too_noisy {
            warn(&format!(
                "The room needs more than {MAX_LEVEL_SPL:.0} dB, most of all around {octave}. \
                 Measure at a quieter moment, or switch off what hums (ventilation, a fridge); \
                 measuring now may be rejected as too noisy."
            ))
        } else if s.level_spl <= MIN_LEVEL_SPL {
            ok(&format!(
                "A quiet room: {MIN_LEVEL_SPL:.0} dB is enough everywhere, and comfortable."
            ))
        } else {
            Span::raw(format!(
                "Set by the background around {octave}: that octave needs the loudest \
                 measurement."
            ))
        }));
        lines.push(Line::from(Span::styled(
            "Enter plays the noise; then turn the amplifier until the meter reads this.",
            Style::default().bold(),
        )));
    }
    let block = panel("Measure at");
    let inner = block.inner(suggestion);
    frame.render_widget(block, suggestion);
    let pad = usize::from(inner.height).saturating_sub(big.len() + lines.len()) / 2;
    let [_, big_area, text_area] = Layout::vertical([
        Constraint::Length(u16::try_from(pad).unwrap_or(0)),
        Constraint::Length(u16::try_from(big.len()).unwrap_or(0)),
        Constraint::Min(0),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(big).alignment(Alignment::Center), big_area);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        text_area,
    );
}
