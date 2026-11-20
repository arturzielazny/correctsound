//! Step 5's screen: the files written, applying them, and the verification through the EQ
//! host.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::graphics::Graphics;
use super::plot::{self};
use super::result::{both_channels_range, channel_curves};
use super::{ACCENT, bad, ok, panel, warn};
use crate::app::{App, VerifyResult};

pub(super) fn save(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
    let [info, plots] = Layout::vertical([Constraint::Length(14), Constraint::Min(6)]).areas(area);
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
    if app.can_apply() || s.applied.is_some() {
        steps(app, host, &mut lines);
    } else {
        lines.push(Line::from(
            match app.devices.as_ref().and_then(|d| d.eq_host.as_ref()) {
                Some(h) => format!(
                    "{} is not running: load left.txt / right.txt into its equalizer by hand, one per channel.",
                    h.name
                ),
                None => "No EQ program to load it into here: load left.txt / right.txt into your \
                         equalizer by hand, one per channel."
                    .to_owned(),
            },
        ));
    }
    if let Some(issue) = &app.measure.issue {
        lines.push(Line::from(bad(issue)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Save, try and keep"))
            .wrap(Wrap { trim: true }),
        info,
    );
    // The channels shown (Tab) side by side, on one scale.
    let shown = app.shown.channels();
    let columns = Layout::horizontal(vec![Constraint::Fill(1); shown.len()]).split(plots);
    let range = both_channels_range(app, true);
    for (&c, &area) in shown.iter().zip(columns.iter()) {
        let name = if c == 0 { "Left" } else { "Right" };
        let curves = channel_curves(app, c, true);
        plot::draw_with_range(frame, area, name, &curves, &[], range, gfx);
    }
}

/// Installing, verifying and rolling back, as three steps: what is done says so, the next
/// step is lit with its key, the others are dim with theirs.
fn steps(app: &App, host: &str, lines: &mut Vec<Line<'static>>) {
    let s = &app.save;
    lines.push(Line::from(format!(
        "Try it in {host}, check it, then keep it or roll back to how {host} was:"
    )));
    // 1. Install.
    match &s.applied {
        Some(a) => {
            lines.push(Line::from(ok(&format!(
                "Installed \"{}\" in {host} and loaded it; its earlier settings are backed up in {}",
                a.preset_name,
                crate::paths::tilde(&a.backup_dir)
            ))));
            lines.push(Line::from(match &a.autoload {
                Some(route) => Span::raw(format!(
                    "   {host} loads it automatically for {route}, also after a restart"
                )),
                None => warn(&format!(
                    "Could not set it to load automatically: set that in {host}'s settings"
                )),
            }));
        }
        None => lines.push(step(
            "1",
            "a",
            &format!("install: load it in {host} (its current settings are backed up first)"),
            true,
        )),
    }
    // 2. Verify.
    let installed = s.applied.is_some();
    match (&s.verify, app.measure.progress()) {
        (_, Some((ch, p))) => lines.push(Line::from(format!(
            "   Verifying {}… {:.0}%",
            ch.name().to_lowercase(),
            p * 100.0
        ))),
        (Some(v), None) if installed => verify_lines(v, host, lines),
        _ => {
            let where_mic = if app.measure.positions[0].len() > 1 {
                "put the mic back where the first measurement was; "
            } else {
                "keep the mic where it is; "
            };
            lines.push(step(
                "2",
                "v",
                &format!(
                    "verify: {where_mic}the sweeps play again through {host} and are compared \
                     with the prediction"
                ),
                installed,
            ));
        }
    }
    // 3. Keep it or roll back.
    match (&s.applied, &s.rolled_back) {
        (None, Some(done)) => {
            lines.push(Line::from(ok(&format!("Rolled back. {done}"))));
            lines.push(Line::from("   a installs it again."));
        }
        _ => lines.push(step(
            "3",
            "u",
            &format!("roll back: {host} goes back to how it was. Or Enter keeps it and quits."),
            installed && !app.measure.running(),
        )),
    }
}

/// One step: its number, its key and what it does; lit when it is the one to do now.
fn step(number: &str, key: &str, text: &str, now: bool) -> Line<'static> {
    let (key_style, text_style) = if now {
        (Style::default().fg(ACCENT).bold(), Style::default())
    } else {
        let dim = Style::default().fg(Color::DarkGray);
        (dim, dim)
    };
    Line::from(vec![
        Span::styled(format!("{number}  "), text_style),
        Span::styled(format!("{key} "), key_style),
        Span::styled(text.to_owned(), text_style),
    ])
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
