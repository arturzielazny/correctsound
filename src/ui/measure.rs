//! Step 5's screen: the sweeps' progress and the measured response, each mic position
//! faintly behind the average.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::graphics::Graphics;
use super::placement::placement;
use super::plot::{self, Curve, Series};
use super::{bad, ok, panel};
use crate::app::App;
use crate::audio::Channel;

pub(super) fn measure(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
    if let Some(p) = &app.placement {
        placement(frame, area, app, p, gfx);
        return;
    }
    let m = &app.measure;
    let [status, plot_area] =
        Layout::vertical([Constraint::Length(6), Constraint::Min(8)]).areas(area);
    let mut lines = Vec::new();
    let counts = [m.positions[0].len(), m.positions[1].len()];
    if let Some((channel, progress)) = m.progress() {
        lines.push(Line::from(format!(
            "Sweeping the {} speaker… keep the room quiet.",
            channel.name().to_lowercase()
        )));
        lines.push(Line::from(progress_bar(
            progress,
            status.width.saturating_sub(4) as usize,
        )));
    } else if counts == [0, 0] {
        lines.push(Line::from(
            "Press Enter to measure: a 5.5 s sweep plays from the left speaker, then from the right.",
        ));
        lines.push(Line::from(
            "Do not move the mic or touch the volume while it runs.",
        ));
    } else {
        lines.push(Line::from(ok(&format!(
            "Measured {} mic position{} per channel.",
            counts[0].min(counts[1]),
            if counts[0].min(counts[1]) == 1 {
                ""
            } else {
                "s"
            }
        ))));
        lines.push(Line::from(
            "Optional but worthwhile: move the mic 20–30 cm and press a to add a position (they are averaged).",
        ));
    }
    if let Some(issue) = &m.issue {
        lines.push(Line::from(bad(issue)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Measure"))
            .wrap(Wrap { trim: true }),
        status,
    );
    let mut curves = Vec::new();
    // Each mic position faintly, behind the averages.
    for c in Channel::BOTH {
        if let Some(r) = &m.responses[c.index()] {
            for p in &r.positions_display {
                curves.push(Curve::new("", Series::Position(c), &p.freqs, &p.db));
            }
        }
    }
    for (c, series) in [
        (Channel::Left, Series::Left),
        (Channel::Right, Series::Right),
    ] {
        if let Some(r) = &m.responses[c.index()] {
            curves.push(Curve::new(
                c.name(),
                series,
                &r.display.freqs,
                &r.display.db,
            ));
        }
    }
    let several = m
        .responses
        .iter()
        .flatten()
        .any(|r| !r.positions_display.is_empty());
    plot::draw(
        frame,
        plot_area,
        if several {
            "Measured response, dB SPL: the average of the mic positions, each one faint"
        } else {
            "Measured response, dB SPL"
        },
        &curves,
        &[],
        gfx,
    );
}

pub(super) fn progress_bar(progress: f64, width: usize) -> String {
    let filled = ((progress * width as f64) as usize).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}
