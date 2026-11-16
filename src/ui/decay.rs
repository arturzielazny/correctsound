//! How the bass fades: the waterfall overlay (`w`), and the decay line the placement
//! screen shows.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::graphics::Graphics;
use super::plot::{self};
use super::{ok, panel};
use crate::app::App;
use crate::audio::Channel;

/// The time `w` takes to fade near `hz`, ms, and whether that is the longest it measures.
pub(super) fn fade_at(w: &crate::dsp::decay::Waterfall, hz: f64) -> (f64, bool) {
    let fade = w.fade_ms();
    let i = w
        .freqs
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 / hz).ln().abs().total_cmp(&(b.1 / hz).ln().abs()))
        .map_or(0, |(i, _)| i);
    let ms = fade.get(i).copied().unwrap_or(0.0);
    (ms, ms >= w.times_ms.last().copied().unwrap_or(400.0) - 1e-6)
}

pub(super) fn ms_text((ms, capped): (f64, bool)) -> String {
    if capped {
        format!("≥{ms:.0} ms")
    } else {
        format!("{ms:.0} ms")
    }
}

/// The notes that ring longest now, and with a pinned sweep, how long they rang then.
pub(super) fn decay_line(
    now: &crate::dsp::decay::Waterfall,
    pinned: Option<&crate::dsp::decay::Waterfall>,
) -> Line<'static> {
    let mut notes: Vec<f64> = now.ringing(30.0).iter().take(3).map(|r| r.hz).collect();
    if let Some(p) = pinned {
        for r in p.ringing(30.0).iter().take(3) {
            if notes.iter().all(|n| (n / r.hz).log2().abs() > 1.0 / 6.0) {
                notes.push(r.hz);
            }
        }
    }
    let typical = now
        .median_fade_ms(30.0, 300.0)
        .map_or(String::new(), |m| format!("typical {m:.0} ms"));
    if notes.is_empty() {
        return Line::from(vec![
            Span::raw(format!("Decay: {typical}; ")),
            Span::styled(
                "no note rings much longer",
                Style::default().fg(Color::Green),
            ),
        ]);
    }
    let mut spans = vec![Span::raw(format!("Decay ({typical}): "))];
    for (i, &hz) in notes.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(", "));
        }
        let n = fade_at(now, hz);
        match pinned.map(|p| fade_at(p, hz)) {
            Some(was) => {
                let color = if n.0 < was.0 * 0.85 {
                    Color::Green
                } else if n.0 > was.0 * 1.15 {
                    Color::LightRed
                } else {
                    Color::Reset
                };
                spans.push(Span::styled(
                    format!("{hz:.0} Hz {} (pinned {})", ms_text(n), ms_text(was)),
                    Style::default().fg(color),
                ));
            }
            None => spans.push(Span::raw(format!("{hz:.0} Hz {}", ms_text(n)))),
        }
    }
    Line::from(spans)
}

/// The decay view: the waterfall of the shown channel and the notes that ring longest.
pub(super) fn decay(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
    use crate::dsp::decay::FADE_DB;
    let c = app.view_channel;
    let name = Channel::BOTH[c].name();
    let Some(w) = app.waterfall(c) else {
        frame.render_widget(
            Paragraph::new("Not measured yet.").block(panel("Decay")),
            area,
        );
        return;
    };
    let [chart, side] =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(40)]).areas(area);
    // Both speakers against the louder one's start, so Tab keeps the scale.
    let top = (0..2)
        .filter_map(|ch| app.waterfall(ch))
        .map(|w| w.peak())
        .fold(0.0, f64::max);
    let slices: Vec<Vec<f64>> = (0..w.power.len())
        .map(|k| w.slice_db_from(k, top).db)
        .collect();
    let ringing = w.ringing(30.0);
    let marks: Vec<f64> = ringing.iter().take(4).map(|r| r.hz).collect();
    plot::draw_decay(
        frame,
        chart,
        &format!("{name} speaker: how the bass fades (Tab to switch)"),
        plot::Decay {
            times_ms: &w.times_ms,
            freqs: &w.freqs,
            slices: &slices,
            marks: &marks,
        },
        gfx,
    );
    let mut lines = vec![
        Line::from("After a note stops, the room keeps it"),
        Line::from("sounding for a while. Notes that ring"),
        Line::from("much longer than the rest sound boomy"),
        Line::from("and blur the bass line."),
        Line::from(""),
    ];
    if let Some(m) = w.median_fade_ms(30.0, 300.0) {
        lines.push(Line::from(format!(
            "Typical time to fade {FADE_DB:.0} dB: {m:.0} ms"
        )));
        lines.push(Line::from(""));
    }
    let modes = app.modes(c);
    if ringing.is_empty() {
        lines.push(Line::from(ok("No note rings much longer")));
        lines.push(Line::from("  than the rest."));
    } else {
        lines.push(Line::from(Span::styled(
            "Rings longest",
            Style::default().bold(),
        )));
        let longest = w.times_ms.last().copied().unwrap_or(400.0) - 1e-6;
        for r in ringing.iter().take(6) {
            // The waterfall stops at 400 ms; a mode measured on its own says how long.
            let time = match crate::dsp::modes::near(&modes, r.hz) {
                Some(m) if r.fade_ms >= longest => m.fade_20_text(),
                _ if r.fade_ms >= longest => format!("≥{:.0} ms", r.fade_ms),
                _ => format!("{:.0} ms", r.fade_ms),
            };
            lines.push(Line::from(format!("  {:>6.0} Hz  {time:>9}", r.hz)));
        }
    }
    if !modes.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Room modes: time to fade 60 dB",
            Style::default().bold(),
        )));
        for m in &modes {
            let color = if m.t60_s > 1.0 {
                Color::LightRed
            } else if m.t60_s > 0.6 {
                Color::Yellow
            } else {
                Color::Reset
            };
            lines.push(Line::from(Span::styled(
                format!("  {:>6.1} Hz  {:>9}", m.hz, m.t60_text()),
                Style::default().fg(color),
            )));
        }
        if let Some(rt) = app.typical_reverb_s(c) {
            lines.push(Line::from(format!("  the room elsewhere: {rt:.1} s")));
        }
    }
    if !ringing.is_empty() || modes.iter().any(|m| m.t60_s > 0.6) {
        lines.push(Line::from(""));
        lines.push(Line::from("EQ cuts lower these notes, which also"));
        lines.push(Line::from("shortens what you hear of them. Bass"));
        lines.push(Line::from("traps in corners, or moving the seat"));
        lines.push(Line::from("or speakers (p on the measure step),"));
        lines.push(Line::from("treat the cause."));
    }
    frame.render_widget(Paragraph::new(lines).block(panel("Decay")), side);
}
