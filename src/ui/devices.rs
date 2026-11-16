//! Step 1's screen: the mic, the EQ host, other apps playing, and the output to use.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use super::{ACCENT, bad, ok, panel, warn};
use crate::app::App;

pub(super) fn preflight(frame: &mut Frame<'_>, area: Rect, app: &App) {
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
    // Room for every line, and for the longer ones to wrap once.
    let height = u16::try_from(lines.len() * 2 + 2)
        .unwrap_or(u16::MAX)
        .clamp(7, 12);
    let [top, bottom] =
        Layout::vertical([Constraint::Length(height), Constraint::Min(4)]).areas(area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Devices"))
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
            .block(panel("Speakers are connected to"))
            .highlight_style(Style::default().fg(Color::Black).bg(ACCENT))
            .highlight_symbol("› "),
        bottom,
        &mut state,
    );
}
