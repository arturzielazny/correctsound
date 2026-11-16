//! Renders the wizard with ratatui.

pub mod bignum;
mod calibration;
mod decay;
mod devices;
pub mod graphics;
pub mod info;
mod level;
mod measure;
mod mic_gain;
mod placement;
pub mod plot;
pub mod raster;
mod result;
pub mod room;
mod save;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{App, LevelPhase, Overlay, Step};
use graphics::Graphics;

const ACCENT: Color = Color::Cyan;

/// Draws the whole screen.
pub fn draw(frame: &mut Frame<'_>, app: &App, gfx: &mut Graphics) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    draw_header(frame, header, app);
    match app.step {
        Step::Preflight => devices::preflight(frame, body, app),
        Step::Calibration => calibration::calibration(frame, body, app),
        Step::MicGain => mic_gain::mic_gain(frame, body, app),
        Step::Level => level::level(frame, body, app),
        Step::Measure => measure::measure(frame, body, app, gfx),
        Step::Result => result::result(frame, body, app, gfx),
        Step::Save => save::save(frame, body, app, gfx),
    }
    if let Some(overlay) = app.overlay {
        draw_overlay(frame, body, app, overlay, gfx);
    }
    draw_footer(frame, footer, app);
}

/// The info, curiosities or decay screen over the step's body.
fn draw_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &App,
    overlay: Overlay,
    gfx: &mut Graphics,
) {
    let (title, lines) = match overlay {
        Overlay::Decay => {
            frame.render_widget(ratatui::widgets::Clear, area);
            decay::decay(frame, area, app, gfx);
            return;
        }
        Overlay::Room => {
            frame.render_widget(ratatui::widgets::Clear, area);
            room::draw(frame, area, app, gfx);
            return;
        }
        Overlay::Info { technical } => (
            if technical {
                "How it works: technical"
            } else {
                "How it works"
            },
            info::lines(technical),
        ),
        Overlay::Curiosities => ("Curiosities", curiosity_lines(app)),
    };
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel(title))
            .wrap(Wrap { trim: true })
            .scroll((app.overlay_scroll, 0)),
        area,
    );
}

fn curiosity_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        "What the measurement says about your room and speakers. None of it changes the EQ.",
        Style::default().fg(Color::Gray),
    ))];
    for section in &app.curiosities {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            section.title,
            Style::default().bold(),
        )));
        for l in &section.lines {
            lines.push(Line::from(format!("• {l}")));
        }
    }
    lines
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(
        " CorrectSound ",
        Style::default().bold().fg(Color::Black).bg(ACCENT),
    )];
    if app.simulated() {
        spans.push(Span::styled(
            " SIMULATED ",
            Style::default().bold().fg(Color::Black).bg(Color::Magenta),
        ));
    }
    if app.reviewing() {
        let name = app
            .session_dir()
            .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        spans.push(Span::styled(
            format!(" REVIEW {name} "),
            Style::default().bold().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    spans.push(Span::raw("  "));
    for (i, step) in Step::ALL.iter().enumerate() {
        let label = format!("{} {}", i + 1, step.title());
        let style = if *step == app.step {
            Style::default()
                .bold()
                .fg(ACCENT)
                .add_modifier(Modifier::UNDERLINED)
        } else if *step < app.step {
            Style::default().fg(Color::Gray)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(label, style));
        if i + 1 < Step::ALL.len() {
            spans.push(Span::styled(" › ", Style::default().fg(Color::DarkGray)));
        }
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

/// The room screen's keys, as they apply now.
fn room_hints(app: &App) -> String {
    let r = &app.room;
    if !r.typing.is_empty() {
        return "Enter set   Backspace   Esc cancel".to_owned();
    }
    let mut keys = vec!["↑/↓ number", "←/→ ±5 cm", "or type metres"];
    keys.push("m mirror");
    keys.push("g/G other rooms");
    match r.view {
        crate::app::room::PlanView::Mode => {
            keys.push("[ ] standing wave");
            keys.push("v where to sit");
        }
        crate::app::room::PlanView::Seats => keys.push("v standing waves"),
    }
    keys.push("Tab speaker");
    if !r.showing_guess() {
        keys.push("x delete");
    }
    keys.push("Esc close");
    keys.join("   ")
}

fn hints(app: &App) -> String {
    if app.overlay == Some(Overlay::Room) {
        return room_hints(app);
    }
    if let Some(overlay) = app.overlay {
        return match overlay {
            Overlay::Info { technical: false } => "↑/↓ scroll   m technical details   Esc close",
            Overlay::Info { technical: true } => "↑/↓ scroll   m plain version   Esc close",
            Overlay::Curiosities => "↑/↓ scroll   i how it works   Esc close",
            Overlay::Decay => "Tab channel   i how it works   Esc close",
            Overlay::Room => "",
        }
        .to_owned();
    }
    if app.placement.is_some() {
        return if app.simulated() {
            "Space pin / unpin   m mode   Tab speaker   s pause   ←/→ move the simulated speakers   Esc done"
        } else {
            "Space pin / unpin   m mode (sweeps, fast, live)   Tab speaker   s pause   Esc done"
        }
        .to_owned();
    }
    let mut text = step_hints(app).to_owned();
    if !app.curiosities.is_empty() {
        text.push_str("   c curiosities");
    }
    if app.measure.positions.iter().any(|p| !p.is_empty()) && app.step >= Step::Measure {
        text.push_str("   w decay");
        if app.measure.responses.iter().all(Option::is_some) {
            text.push_str("   r room");
        }
    }
    text.push_str("   i info");
    text
}

fn step_hints(app: &App) -> &'static str {
    match app.step {
        Step::Preflight => "↑/↓ choose output   r refresh   Enter continue   q quit",
        Step::Calibration => {
            "type the serial   Tab mic orientation   Enter download / continue   Esc back"
        }
        Step::MicGain => "Enter set to unity / continue   Esc back   q quit",
        Step::Level if app.level.phase == LevelPhase::Listening => {
            "Enter play the noise at the suggested level   Esc back"
        }
        Step::Level => {
            "↑/↓ output volume   Tab other channel   Space restart   l listen again   Enter continue   Esc back"
        }
        Step::Measure if app.measure.running() => "s stop",
        Step::Measure => {
            if app.measure.positions.iter().all(|p| !p.is_empty()) {
                "Enter continue   a add mic position   x clear   p placement   Esc back"
            } else {
                "Enter measure   p placement   Esc back   q quit"
            }
        }
        Step::Result if app.reviewing() => {
            "Tab channel   t/T tilt   h high limit   f/F filters   d defaults   Enter save   q quit"
        }
        Step::Result => {
            "Tab channel   t/T tilt   h high limit   f/F filters   d defaults   Enter save   Esc back"
        }
        Step::Save if app.measure.running() => "s stop",
        Step::Save if app.save.applied.is_some() => "v verify   Enter/q quit   Esc back",
        Step::Save => "a apply the EQ   Enter/q quit   Esc back",
    }
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let mut lines = vec![Line::from(Span::styled(
        hints(app),
        Style::default().fg(Color::Gray),
    ))];
    if let Some(m) = &app.message {
        lines.push(Line::from(Span::styled(
            m.clone(),
            Style::default().fg(Color::LightRed),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::TOP))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(format!(" {title} "), Style::default().bold()))
}

fn ok(text: &str) -> Span<'static> {
    Span::styled(format!("✔ {text}"), Style::default().fg(Color::Green))
}

fn warn(text: &str) -> Span<'static> {
    Span::styled(format!("! {text}"), Style::default().fg(Color::Yellow))
}

fn bad(text: &str) -> Span<'static> {
    Span::styled(format!("✘ {text}"), Style::default().fg(Color::LightRed))
}
