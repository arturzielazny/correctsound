//! Step 1's screen, setup: the mic, its calibration file and input gain, the EQ host, other
//! apps playing, the output to use, and what Enter will do. On a later run everything is
//! remembered and ticked, and the last line says Enter continues.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use super::{ACCENT, bad, ok, panel, warn};
use crate::app::{App, CalStatus};
use crate::cal::Orientation;

fn dim(text: &str) -> Span<'static> {
    Span::styled(text.to_owned(), Style::default().fg(Color::DarkGray))
}

pub(super) fn setup(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut lines: Vec<Line<'_>> = Vec::new();
    for t in &app.missing_tools {
        lines.push(Line::from(bad(&format!("missing {t}"))));
    }
    match (app.mic(), &app.devices) {
        (Some(m), _) => lines.push(Line::from(ok(&format!("Mic: {}", m.description)))),
        (None, Some(_)) => lines.push(Line::from(warn(
            "Plug in the UMIK-1 — waiting for it to appear…",
        ))),
        // The devices could not be listed; the reason is shown below.
        (None, None) => {}
    }
    calibration(app, &mut lines);
    if app.mic().is_some() {
        lines.push(match &app.mic_gain {
            None => Line::from(dim("Mic gain: reading…")),
            Some(g) if g.is_unity() => {
                Line::from(ok("Mic gain: at unity, as the calibration assumes"))
            }
            Some(g) => Line::from(warn(&format!(
                "Mic gain: {}. Enter sets it to unity, as the calibration assumes; it goes \
                 back when you quit.",
                g.off_unity()
            ))),
        });
    }
    if let Some(d) = &app.devices {
        match &d.eq_host {
            Some(h) if h.running => lines.push(Line::from(ok(&format!(
                "{} is running; it will be bypassed while measuring and restored afterwards",
                h.name
            )))),
            Some(h) => lines.push(Line::from(warn(&format!(
                "{} is not running; EQ files will still be written",
                h.name
            )))),
            None => lines.push(Line::from(warn(
                "No EQ program is supported here yet; EQ files will be written",
            ))),
        }
        if !d.other_streams.is_empty() {
            lines.push(Line::from(warn(&format!(
                "These apps have audio streams open; pause any that are playing: {}",
                d.other_streams.join(", ")
            ))));
        }
    }
    if let Some(w) = app.output_warning() {
        lines.push(Line::from(bad(&w)));
    }
    if let Some(next) = what_enter_does(app) {
        lines.push(Line::from(""));
        lines.push(next);
    }
    // Room for every line as it wraps (a word or two of slack each), and at least half the
    // screen for the outputs.
    let inner = usize::from(area.width.saturating_sub(2)).max(1);
    let rows: usize = lines
        .iter()
        .map(|l| (l.width() + inner / 8).div_ceil(inner).max(1))
        .sum();
    let height = u16::try_from(rows + 2)
        .unwrap_or(u16::MAX)
        .min(area.height / 2)
        .max(6);
    let [top, bottom] =
        Layout::vertical([Constraint::Length(height), Constraint::Min(4)]).areas(area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Setup"))
            .wrap(Wrap { trim: true }),
        top,
    );
    let sinks: Vec<ListItem<'_>> = app
        .devices
        .as_ref()
        .map(|d| {
            d.sinks
                .iter()
                .map(|s| {
                    let mut label = s.description.clone();
                    if d.preferred_sink.as_deref() == Some(s.name.as_str()) {
                        match &d.eq_host {
                            Some(h) => label.push_str(&format!("  ({} output)", h.name)),
                            None => label.push_str("  (default)"),
                        }
                    }
                    if app.last_measured_sink.as_deref() == Some(s.name.as_str()) {
                        label.push_str("  (last measurement)");
                    }
                    ListItem::new(label)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut state = ListState::default().with_selected(Some(app.sink_index));
    frame.render_stateful_widget(
        List::new(sinks)
            .block(panel("Speakers are connected to (↑/↓)"))
            .highlight_style(Style::default().fg(Color::Black).bg(ACCENT))
            .highlight_symbol("› "),
        bottom,
        &mut state,
    );
}

/// The calibration's lines: the serial number being typed and the orientation, then whether
/// the file is loaded, downloading or missing.
fn calibration(app: &App, lines: &mut Vec<Line<'_>>) {
    if !app.calibration_by_serial() {
        match (&app.cal, &app.cal_path) {
            (Some(_), Some(path)) => {
                lines.push(Line::from(ok(&format!("Calibration: {}", path.display()))))
            }
            (Some(_), None) => lines.push(Line::from(ok("Calibration: the simulated mic's"))),
            (None, _) => lines.push(Line::from(bad("Calibration: the file could not be read"))),
        }
        return;
    }
    let short = |o: Orientation| match o {
        Orientation::OnAxis => "0°, pointing at the speakers",
        Orientation::Vertical => "90°, pointing at the ceiling",
    };
    lines.push(Line::from(vec![
        Span::raw("  Mic serial number: "),
        Span::styled(
            format!("{}▏", app.serial_input),
            Style::default().bold().fg(ACCENT),
        ),
        Span::raw("   orientation (Tab): "),
        Span::styled(short(app.orientation), Style::default().fg(ACCENT)),
    ]));
    match (&app.cal_status, &app.cal) {
        (CalStatus::Downloading, _) => lines.push(Line::from(warn(
            "Calibration: downloading the file from miniDSP…",
        ))),
        (CalStatus::Failed(e), _) => lines.push(Line::from(bad(&format!("Calibration: {e}")))),
        (status, Some(c)) => {
            let source = if *status == CalStatus::Downloaded {
                "downloaded from miniDSP and saved"
            } else {
                "saved copy"
            };
            lines.push(Line::from(ok(&format!(
                "Calibration: sensitivity {:+.2} dB, {} points ({source})",
                c.sens_factor,
                c.len()
            ))));
            if c.is_old_generation() {
                lines.push(Line::from(warn(
                    "This is an older 18 dB-gain UMIK-1: SPL readings will be approximate (the \
                     EQ is unaffected).",
                )));
            }
        }
        (_, None) if app.serial_input.is_empty() => lines.push(Line::from(warn(
            "Calibration: every UMIK-1 has its own file. Type the serial number printed on \
             the mic (like 700-1234); the file is downloaded from miniDSP once and saved.",
        ))),
        (_, None) => lines.push(Line::from(dim(
            "Calibration: not downloaded yet for this serial number and orientation",
        ))),
    }
}

/// The last line: what Enter does now, or what it waits for.
fn what_enter_does(app: &App) -> Option<Line<'static>> {
    if !app.missing_tools.is_empty() || app.mic().is_none() {
        return None;
    }
    let text = if app.cal.is_none() {
        if !app.calibration_by_serial() {
            return None;
        }
        match app.cal_status {
            CalStatus::Downloading if app.continue_after_download => {
                return Some(Line::from(dim(
                    "Continuing as soon as the calibration file arrives…",
                )));
            }
            CalStatus::Downloading => " continues once the calibration file arrives.",
            _ if app.serial_input.parse::<crate::cal::Serial>().is_err() => {
                return None;
            }
            _ => " downloads the calibration file and continues.",
        }
    } else if app.mic_gain_off_unity() {
        " sets the mic gain and continues."
    } else {
        " continues."
    };
    Some(Line::from(vec![
        Span::styled("Enter", Style::default().fg(ACCENT).bold()),
        Span::raw(text),
    ]))
}
