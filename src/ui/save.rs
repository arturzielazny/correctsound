//! Step 7's screen: the files written, applying them, and the verification through the EQ
//! host.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::graphics::Graphics;
use super::plot::{self};
use super::result::{both_channels_range, channel_curves};
use super::{bad, ok, panel, warn};
use crate::app::{App, VerifyResult};

pub(super) fn save(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
    let [info, plots] = Layout::vertical([Constraint::Length(12), Constraint::Min(6)]).areas(area);
    let s = &app.save;
    let mut lines = Vec::new();
    if let Some(e) = &s.error {
        lines.push(Line::from(bad(e)));
    }
    if let (false, Some(dir)) = (s.written.is_empty(), app.session_dir()) {
        let names: Vec<String> = s
            .written
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        lines.push(Line::from(ok(&format!("Saved {}", names.join(", ")))));
        lines.push(Line::from(format!("    in {}", crate::paths::tilde(&dir))));
        lines.push(Line::from(
            "    left.txt / right.txt are Equalizer APO files; CorrectSound.json is an EasyEffects preset; report.txt lists the filters.",
        ));
    }
    lines.push(Line::from(""));
    let host = app.eq_host_name();
    match (&s.applied, app.can_apply()) {
        (Some(a), _) => {
            lines.push(Line::from(ok(&format!(
                "Loaded preset \"{}\" in {host}; its previous settings are backed up in {}",
                a.preset_name,
                crate::paths::tilde(&a.backup_dir)
            ))));
            lines.push(Line::from(match &a.autoload {
                Some(route) => ok(&format!(
                    "{host} will load it automatically for {route}, also after a restart"
                )),
                None => warn(&format!(
                    "Could not set it to load automatically: set that in {host}'s settings"
                )),
            }));
            match (&s.verify, app.measure.progress()) {
                (_, Some((ch, p))) => lines.push(Line::from(format!(
                    "Verifying {}… {:.0}%",
                    ch.name().to_lowercase(),
                    p * 100.0
                ))),
                (Some(v), None) => verify_lines(v, host, &mut lines),
                (None, None) if app.measure.positions[0].len() > 1 => {
                    lines.push(Line::from(format!(
                        "Press v to verify: put the mic back where the first measurement was, \
                         then the sweeps play again, this time through {host}."
                    )));
                }
                (None, None) => lines.push(Line::from(format!(
                    "Press v to verify: the sweeps play again, this time through {host}. Don't move the mic."
                ))),
            }
        }
        (None, true) => lines.push(Line::from(format!(
            "Press a to install the preset in {host} and load it (its current settings are backed up first)."
        ))),
        (None, false) => lines.push(Line::from(
            match app.devices.as_ref().and_then(|d| d.eq_host.as_ref()) {
                Some(h) => format!(
                    "{} is not running: load left.txt / right.txt into its equalizer by hand, one per channel.",
                    h.name
                ),
                None => "No EQ program to load it into here: load left.txt / right.txt into your \
                         equalizer by hand, one per channel."
                    .to_owned(),
            },
        )),
    }
    if let Some(issue) = &app.measure.issue {
        lines.push(Line::from(bad(issue)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Save & apply"))
            .wrap(Wrap { trim: true }),
        info,
    );
    let [l, r] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(plots);
    let range = both_channels_range(app, true);
    for (c, area) in [(0, l), (1, r)] {
        let name = if c == 0 { "Left" } else { "Right" };
        let curves = channel_curves(app, c, true);
        plot::draw_with_range(frame, area, name, &curves, &[], range, gfx);
    }
}

/// Verification summary: overall match, then any single spot that differs a lot.
pub(super) fn verify_lines(v: &[VerifyResult; 2], host: &str, lines: &mut Vec<Line<'static>>) {
    let text = format!(
        "Verification: the result matches the prediction within {:.1} dB RMS (L) / {:.1} dB RMS (R)",
        v[0].rms_db, v[1].rms_db
    );
    if v.iter().all(|r| r.rms_db <= VerifyResult::RMS_OK_DB) {
        lines.push(Line::from(ok(&text)));
    } else {
        lines.push(Line::from(warn(&format!(
            "{text}. That is more than expected: check that {host} has no other effects \
             active, and that the mic has not moved."
        ))));
    }
    for (r, name) in v.iter().zip(["Left", "Right"]) {
        if r.worst_db.abs() > VerifyResult::SPOT_NOTE_DB {
            lines.push(Line::from(warn(&format!(
                "{name}: {:+.1} dB around {:.0} Hz. A single spot like this usually means \
                 something near the mic changed (a person, chair or laptop lid); measuring \
                 more mic positions averages it out.",
                r.worst_db, r.worst_hz
            ))));
        }
    }
}
