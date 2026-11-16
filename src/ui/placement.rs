//! The placement screen: the bass at the seat measured over and over, against a pinned
//! reading, with a big number to read from across the room.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::decay::decay_line;
use super::graphics::Graphics;
use super::plot::{self, Curve, Series};
use super::{ACCENT, bad, bignum, panel};
use crate::app::App;
use crate::audio::Channel;

/// The placement screen: instructions, scores, a big number to read from across the room,
/// the bass of the latest reading over the pinned and the best ones, and in the sweep modes
/// how the bass decays.
pub(super) fn placement(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    p: &crate::app::PlacementState,
    gfx: &mut Graphics,
) {
    use crate::app::placement::{Mode, SCORE_FROM_HZ, SCORE_TO_HZ, Score};
    let [status, plot_area] =
        Layout::vertical([Constraint::Length(13), Constraint::Min(8)]).areas(area);
    // The big number needs room; on a narrow terminal the text has it all.
    let (text_area, big_area) = if status.width >= 110 {
        let [a, b] =
            Layout::horizontal([Constraint::Min(60), Constraint::Length(40)]).areas(status);
        (a, Some(b))
    } else {
        (status, None)
    };
    // Every row keeps its place, with a dim placeholder until it has something to say, so
    // nothing pops in and out while you watch from across the room.
    let dim = |text: &str| Span::styled(text.to_owned(), Style::default().fg(Color::DarkGray));
    let intro = match p.mode {
        Mode::Sweeps => {
            "A bass sweep every couple of seconds: the finest detail and the waterfall. Keep still \
             while it plays."
        }
        Mode::Fast => {
            "A one-second sweep in a loop, updated five times a second: a change shows within a \
             second, decay included."
        }
        Mode::Fastest => {
            "A third-of-a-second sweep in a loop, updated ten times a second: it follows a bass \
             trap as you carry it. Its waterfall is only rough."
        }
    };
    let mut lines = vec![
        Line::from(
            "Move the speakers or your seat by 10–50 cm, or try a bass trap in each corner. Space \
             pins a placement to compare with; keep the one with the smaller number (peaks and \
             dips, not level).",
        ),
        Line::from(""),
    ];
    // The modes, the current one lit, and what it does.
    let mut modes = vec![Span::raw("Mode (m): ")];
    for (i, m) in [Mode::Sweeps, Mode::Fast, Mode::Fastest]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            modes.push(dim(" · "));
        }
        modes.push(if m == p.mode {
            Span::styled(m.name(), Style::default().fg(ACCENT).bold())
        } else {
            dim(m.name())
        });
    }
    lines.push(Line::from(modes));
    lines.push(Line::from(intro));
    let speaker = p.channel.name();
    let state = if p.paused {
        Span::styled("paused (s resumes)", Style::default().fg(Color::Yellow))
    } else {
        let what = if p.mode == Mode::Sweeps {
            "sweeps"
        } else {
            "updates"
        };
        Span::raw(format!(
            "{speaker} speaker (Tab: other), {} {what}",
            p.count
        ))
    };
    let mut head = vec![state];
    if let Some(d) = app.sim_wall_m() {
        head.push(Span::styled(
            format!("   simulated speakers {:.0} cm from the wall", d * 100.0),
            Style::default().fg(Color::Magenta),
        ));
    }
    lines.push(Line::from(head));
    let score_text = |s: &Score| {
        format!(
            "±{:.1} dB  (deepest dip {:+.0} dB at {:.0} Hz, highest peak {:+.0} dB at {:.0} Hz)",
            s.evenness_db, s.deepest.1, s.deepest.0, s.highest.1, s.highest.0
        )
    };
    lines.push(match &p.latest {
        Some(r) => {
            let mut spans = vec![
                Span::raw(format!(
                    "Now ({}, {SCORE_FROM_HZ:.0}–{SCORE_TO_HZ:.0} Hz): ",
                    r.channel.name()
                )),
                Span::styled(score_text(&r.score), Style::default().bold()),
            ];
            // The comparison is in the big number when there is room for it.
            if let Some(pinned) = p.reference.as_ref().filter(|_| big_area.is_none()) {
                let (text, color) = compared(r.score.evenness_db, pinned.score.evenness_db);
                spans.push(Span::styled(
                    format!("  {text}"),
                    Style::default().fg(color),
                ));
            }
            Line::from(spans)
        }
        None => Line::from(dim("Now: waiting for the first reading…")),
    });
    lines.push(match &p.reference {
        Some(r) => Line::from(Span::styled(
            format!("Pinned ({}): {}", r.channel.name(), score_text(&r.score)),
            Style::default().fg(Color::Gray),
        )),
        None => Line::from(dim("Pinned: nothing yet; Space pins the latest reading")),
    });
    let latest_decay = p.latest.as_ref().and_then(|r| r.waterfall.as_ref());
    let rough = p.latest.as_ref().is_some_and(|r| r.mode == Mode::Fastest);
    lines.push(match latest_decay {
        Some(_) if rough => Line::from(dim(
            "Decay: rough in this mode (the loop is shorter than the room rings); fast sweeps \
             give ringing times",
        )),
        Some(w) => decay_line(
            w,
            p.reference
                .as_ref()
                .filter(|r| r.mode != Mode::Fastest)
                .and_then(|r| r.waterfall.as_ref()),
        ),
        None => Line::from(dim("Decay: after the first reading")),
    });
    // Problems stay a few seconds (see PlacementState::report); otherwise a quiet all-clear.
    lines.push(match (&p.issue, &p.latest) {
        (Some(issue), _) => Line::from(bad(issue)),
        (None, Some(_)) => Line::from(dim("✔ the sweep reaches the mic cleanly")),
        (None, None) => Line::from(""),
    });
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Placement"))
            .wrap(Wrap { trim: true }),
        text_area,
    );
    if let Some(area) = big_area {
        big_score(frame, area, p);
    }
    let mut curves = Vec::new();
    if let Some(r) = &p.reference {
        curves.push(Curve::new(
            "pinned",
            Series::Reference,
            &r.response.freqs,
            &r.response.db,
        ));
    }
    if let Some(r) = &p.latest {
        let series = match r.channel {
            Channel::Left => Series::Left,
            Channel::Right => Series::Right,
        };
        curves.push(Curve::new("now", series, &r.response.freqs, &r.response.db));
    }
    // The scale also covers the other speaker's latest reading, so Tab keeps it.
    let mut scaled: Vec<Curve> = p
        .last
        .iter()
        .flatten()
        .map(|r| Curve::new("", Series::Reference, &r.freqs, &r.db))
        .collect();
    scaled.extend(curves.iter().cloned());
    let (curve_area, decay_area) = match latest_decay {
        Some(w) => {
            let [a, b] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(plot_area);
            (a, Some((b, w)))
        }
        _ => (plot_area, None),
    };
    plot::draw_axes(
        frame,
        curve_area,
        "Bass at the mic, dB SPL",
        &curves,
        &[],
        plot::BASS,
        plot::y_range(&scaled, plot::BASS),
        gfx,
    );
    if let Some((area, w)) = decay_area {
        let slices: Vec<Vec<f64>> = (0..w.power.len()).map(|k| w.slice_db(k).db).collect();
        let marks: Vec<f64> = w.ringing(30.0).iter().take(3).map(|r| r.hz).collect();
        plot::draw_decay(
            frame,
            area,
            if rough {
                "How the bass fades: rough, the first 120 ms"
            } else {
                "How the bass fades, latest sweep"
            },
            plot::Decay {
                times_ms: &w.times_ms,
                freqs: &w.freqs,
                slices: &slices,
                marks: &marks,
            },
            gfx,
        );
    }
}

/// Scores closer than this are about the same, dB.
pub(super) const SAME_DB: f64 = 0.3;

/// How a score compares with the pinned one, and its colour: green when more even.
pub(super) fn compared(now_db: f64, pinned_db: f64) -> (String, Color) {
    let delta = now_db - pinned_db;
    if delta < -SAME_DB {
        (
            format!("{:.1} dB more even than pinned", -delta),
            Color::Green,
        )
    } else if delta > SAME_DB {
        (
            format!("{delta:.1} dB less even than pinned"),
            Color::LightRed,
        )
    } else {
        ("about the same as pinned".to_owned(), Color::Gray)
    }
}

/// The number that matters while moving things, big enough to read from across the room:
/// how much more even than the pinned reading the latest one is, or without a pin, how
/// uneven it is.
pub(super) fn big_score(frame: &mut Frame<'_>, area: Rect, p: &crate::app::PlacementState) {
    let Some(now) = &p.latest else {
        frame.render_widget(panel("Evenness"), area);
        return;
    };
    let (number, caption, color) = match &p.reference {
        Some(pinned) => {
            let gain = pinned.score.evenness_db - now.score.evenness_db;
            let color = compared(now.score.evenness_db, pinned.score.evenness_db).1;
            let caption = if gain >= 0.0 {
                "dB more even than pinned"
            } else {
                "dB less even than pinned"
            };
            (format!("{gain:+.1}"), caption, color)
        }
        None => (
            format!("±{:.1}", now.score.evenness_db),
            "dB uneven; Space pins it",
            Color::Reset,
        ),
    };
    let mut lines = bignum::lines(&number, Style::default().fg(color));
    lines.push(Line::from(Span::styled(
        caption,
        Style::default().fg(color),
    )));
    // Centred vertically inside the border.
    let pad = usize::from(area.height.saturating_sub(2)).saturating_sub(lines.len()) / 2;
    lines.splice(0..0, std::iter::repeat_n(Line::from(""), pad));
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(panel("Evenness")),
        area,
    );
}
