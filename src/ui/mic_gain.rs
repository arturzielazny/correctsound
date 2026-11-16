//! Step 3's screen: the mic's input gain, and setting it to unity.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::{bad, ok, panel, warn};
use crate::app::App;

pub(super) fn mic_gain(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut lines = vec![
        Line::from("The calibration assumes the mic signal is not scaled anywhere on the way in."),
        Line::from(""),
    ];
    match &app.mic_gain {
        None => lines.push(Line::from(warn("Reading mic gain…"))),
        Some(g) => {
            for setting in &g.settings {
                let text = format!("{} {}", setting.label, setting.value);
                lines.push(Line::from(if setting.at_unity {
                    ok(&text)
                } else {
                    bad(&text)
                }));
            }
            lines.push(Line::from(""));
            if g.is_unity() {
                lines.push(Line::from("All set. Press Enter to continue."));
            } else {
                lines.push(Line::from(
                    "Press Enter to set these to unity. The previous values are restored when you quit.",
                ));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Mic gain"))
            .wrap(Wrap { trim: true }),
        area,
    );
}
