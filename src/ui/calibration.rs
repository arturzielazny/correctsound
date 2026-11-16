//! Step 2's screen: the mic's serial number and orientation, and the download.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::{ACCENT, bad, ok, panel, warn};
use crate::app::{App, CalStatus};
use crate::cal::Orientation;

pub(super) fn calibration(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut lines = vec![
        Line::from(
            "Every UMIK-1 has its own calibration file. Enter the serial number printed on the mic",
        ),
        Line::from("(e.g. 700-1234); the file is downloaded from miniDSP and cached."),
        Line::from(""),
        Line::from(vec![
            Span::raw("Serial: "),
            Span::styled(
                format!("{}▏", app.serial_input),
                Style::default().bold().fg(ACCENT),
            ),
        ]),
        Line::from(""),
        Line::from("Mic orientation:"),
    ];
    for o in [Orientation::OnAxis, Orientation::Vertical] {
        let selected = o == app.orientation;
        let mark = if selected { "(•) " } else { "( ) " };
        let style = if selected {
            Style::default().fg(ACCENT)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("  {mark}{}", o.label()),
            style,
        )));
    }
    lines.push(Line::from(""));
    match (&app.cal_status, &app.cal) {
        (CalStatus::Downloading, _) => lines.push(Line::from(warn("Downloading…"))),
        (CalStatus::Failed(e), _) => lines.push(Line::from(bad(e))),
        (CalStatus::Idle, Some(c)) => {
            lines.push(Line::from(ok(&format!(
                "Calibration loaded: serial {}, sensitivity {:+.2} dB, {} points. Press Enter to continue.",
                c.serial.as_deref().unwrap_or("?"),
                c.sens_factor,
                c.len()
            ))));
            if c.is_old_generation() {
                lines.push(Line::from(warn(
                    "This is an older 18 dB-gain UMIK-1: SPL readings will be approximate (the EQ is unaffected).",
                )));
            }
        }
        (CalStatus::Idle, None) => lines.push(Line::from("Press Enter to download.")),
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Mic calibration"))
            .wrap(Wrap { trim: true }),
        area,
    );
}
