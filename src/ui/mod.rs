//! Renders the wizard with ratatui.

pub mod bignum;
pub mod graphics;
pub mod info;
pub mod plot;
pub mod raster;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::level::{ABORT_SPL, TOLERANCE};
use crate::app::{App, CalStatus, LevelPhase, Overlay, Step, VerifyResult};
use crate::audio::Channel;
use crate::cal::Orientation;
use crate::eq::response_db;
use graphics::Graphics;
use plot::{Curve, Series, Shade};

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
        Step::Preflight => preflight(frame, body, app),
        Step::Calibration => calibration(frame, body, app),
        Step::MicGain => mic_gain(frame, body, app),
        Step::Level => level(frame, body, app),
        Step::Measure => measure(frame, body, app, gfx),
        Step::Result => result(frame, body, app, gfx),
        Step::Save => save(frame, body, app, gfx),
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
            decay(frame, area, app, gfx);
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

fn hints(app: &App) -> String {
    if let Some(overlay) = app.overlay {
        return match overlay {
            Overlay::Info { technical: false } => "↑/↓ scroll   m technical details   Esc close",
            Overlay::Info { technical: true } => "↑/↓ scroll   m plain version   Esc close",
            Overlay::Curiosities => "↑/↓ scroll   i how it works   Esc close",
            Overlay::Decay => "Tab channel   i how it works   Esc close",
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

// ----- 1 -----

fn preflight(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let [top, bottom] = Layout::vertical([Constraint::Length(7), Constraint::Min(4)]).areas(area);
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

// ----- 2 -----

fn calibration(frame: &mut Frame<'_>, area: Rect, app: &App) {
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

// ----- 3 -----

fn mic_gain(frame: &mut Frame<'_>, area: Rect, app: &App) {
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

// ----- 4 -----

fn level(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let l = &app.level;
    if l.phase == LevelPhase::Listening {
        listening(frame, area, app);
        return;
    }
    let target = l.target_spl;
    let [info, meter, details] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(7),
        Constraint::Min(4),
    ])
    .areas(area);
    let instructions = vec![
        Line::from(format!(
            "Pink noise (500–2000 Hz) plays from one speaker. Turn your amplifier until the meter \
             reads {target:.0} dB: the level your room's background noise calls for."
        )),
        Line::from(
            "Put the mic where your head is when listening, at ear height, pointing as chosen in step 2.",
        ),
        Line::from("No amplifier knob? Use ↑/↓ to change the output volume instead."),
    ];
    frame.render_widget(
        Paragraph::new(instructions)
            .block(panel("Set the level"))
            .wrap(Wrap { trim: true }),
        info,
    );

    let title = match &l.phase {
        LevelPhase::Listening => "Listening to the room…".to_owned(),
        LevelPhase::Playing if l.fading_in() => {
            format!("{} speaker — fading in…", l.channel.name())
        }
        LevelPhase::Playing => format!("{} speaker", l.channel.name()),
        LevelPhase::Stopped(_) => "Stopped".to_owned(),
        LevelPhase::Idle => "Idle".to_owned(),
    };
    let spl = l.spl.filter(|_| l.phase == LevelPhase::Playing);
    let (ratio, color, label) = match spl {
        Some(s) => {
            let lo = target - 20.0;
            let ratio = ((s - lo) / 30.0).clamp(0.0, 1.0);
            let color = if (s - target).abs() <= TOLERANCE {
                Color::Green
            } else if s > target {
                Color::LightRed
            } else {
                Color::Yellow
            };
            let hint = if (s - target).abs() <= TOLERANCE {
                "  ✔ good".to_owned()
            } else if s > target {
                format!("  turn down {:.1} dB", s - target)
            } else {
                format!("  turn up {:.1} dB", target - s)
            };
            (ratio, color, format!("{s:.1} dB SPL{hint}"))
        }
        None => (0.0, Color::DarkGray, "—".to_owned()),
    };
    let [meter_title, gauge, scale] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .margin(1)
    .areas(meter);
    frame.render_widget(panel("Meter"), meter);
    frame.render_widget(
        Paragraph::new(title).alignment(Alignment::Center),
        meter_title,
    );
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(color).bg(Color::Black))
            .ratio(ratio)
            .label(Span::styled(label, Style::default().bold())),
        gauge,
    );
    // Scale under the gauge: 20 dB below the target to 10 above, with the target marked.
    let width = scale.width as usize;
    let mut marks = vec![' '; width];
    let pos = |db: f64| (((db - (target - 20.0)) / 30.0) * width as f64).round() as usize;
    for (db, ch) in [
        (target - TOLERANCE, '['),
        (target, '▲'),
        (target + TOLERANCE, ']'),
    ] {
        if let Some(m) = marks.get_mut(pos(db).min(width.saturating_sub(1))) {
            *m = ch;
        }
    }
    let scale_text: String = marks.into_iter().collect();
    frame.render_widget(Paragraph::new(scale_text).fg(Color::Green), scale);

    let mut lines = Vec::new();
    if let LevelPhase::Stopped(reason) = &l.phase {
        lines.push(Line::from(bad(reason)));
    }
    if let Some(n) = &l.room_noise {
        let groups: Vec<String> = NOISE_GROUPS
            .iter()
            .map(|g| format!("{} {:.0}", g.name.to_lowercase(), group_level(n, g)))
            .collect();
        lines.push(Line::from(format!(
            "Background noise: {} dB SPL (l listens again)",
            groups.join(", ")
        )));
    }
    if let Some(a) = l.ambient_spl {
        let text = format!("In the meter's band the background is {a:.0} dB SPL");
        lines.push(Line::from(if l.room_is_noisy() {
            warn(&format!(
                "{text}, close enough to the noise to raise the reading a little"
            ))
        } else {
            ok(&text)
        }));
    }
    if let Some(v) = l.sink_volume {
        lines.push(Line::from(format!("Output volume: {v:.2}")));
    }
    let heard: Vec<String> = Channel::BOTH
        .iter()
        .filter_map(|c| l.heard[c.index()].map(|s| format!("{} {s:.1} dB", c.name())))
        .collect();
    if !heard.is_empty() {
        lines.push(Line::from(format!("Last readings: {}", heard.join(", "))));
    }
    if let [Some(a), Some(b)] = l.heard {
        lines.push(Line::from(format!(
            "Left/right difference {:+.1} dB — the EQ will even this out.",
            a - b
        )));
    }
    if app.spl_is_approximate() {
        lines.push(Line::from(warn(
            "SPL readings are approximate for this mic.",
        )));
    }
    lines.push(Line::from(Span::styled(
        format!("Playback stops automatically above {ABORT_SPL:.0} dB SPL."),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Status"))
            .wrap(Wrap { trim: true }),
        details,
    );
}

/// A range of octaves shown as one line of the background noise.
struct NoiseGroup {
    name: &'static str,
    range: &'static str,
    /// Indices into [`crate::dsp::ambient::OCTAVES`].
    octaves: std::ops::RangeInclusive<usize>,
    /// What usually makes noise there.
    sources: &'static str,
}

const NOISE_GROUPS: [NoiseGroup; 3] = [
    NoiseGroup {
        name: "Bass",
        range: "20–180 Hz",
        octaves: 0..=2,
        sources: "traffic, ventilation, fridges, the building",
    },
    NoiseGroup {
        name: "Mids",
        range: "180 Hz–1.4 kHz",
        octaves: 3..=5,
        sources: "voices, fans, appliances",
    },
    NoiseGroup {
        name: "Highs",
        range: "1.4–20 kHz",
        octaves: 6..=9,
        sources: "hiss, computer fans, birds",
    },
];

/// A group's level: the power sum of its octaves, dB SPL.
fn group_level(noise: &crate::dsp::ambient::OctaveLevels, g: &NoiseGroup) -> f64 {
    crate::dsp::power_to_db(
        noise[g.octaves.clone()]
            .iter()
            .map(|&d| crate::dsp::db_to_power(d))
            .sum(),
    )
}

/// What a measurement level means, in words and colour.
fn level_verdict(needs: f64) -> (&'static str, Color) {
    use crate::dsp::ambient::{MAX_LEVEL_SPL, MIN_LEVEL_SPL};
    if needs <= MIN_LEVEL_SPL {
        ("quiet", Color::Green)
    } else if needs <= 70.0 {
        ("some noise", Color::Yellow)
    } else if needs <= MAX_LEVEL_SPL {
        ("noisy", Color::LightRed)
    } else {
        ("too noisy", Color::Red)
    }
}

/// Step 4, first part: listening to the room, live, and the level it calls for.
fn listening(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::dsp::ambient::{MARGIN_GOAL_DB, MAX_LEVEL_SPL, MIN_LEVEL_SPL};
    let l = &app.level;
    let [info, noise, suggestion] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Min(10),
    ])
    .areas(area);
    let intro = vec![
        Line::from(Span::styled(
            "Keep quiet for a few seconds: CorrectSound is listening to your room.",
            Style::default().bold(),
        )),
        Line::from(format!(
            "Its background noise decides how loud the measurement has to be: every octave \
             should end up {MARGIN_GOAL_DB:.0} dB above it. A quiet room can be measured \
             quietly, at {MIN_LEVEL_SPL:.0} dB; a noisy one needs up to {MAX_LEVEL_SPL:.0}. \
             The suggestion follows the room as long as you listen."
        )),
        Line::from(
            "Put the mic where your head is when listening, at ear height, pointing as chosen in step 2.",
        ),
    ];
    frame.render_widget(
        Paragraph::new(intro)
            .block(panel("Listen to the room"))
            .wrap(Wrap { trim: true }),
        info,
    );

    let mut rows = Vec::new();
    match (&l.room_noise, &l.suggestion) {
        (Some(n), Some(s)) => {
            // Bars from 0 to 70 dB SPL.
            let bar_width = 24usize;
            for g in &NOISE_GROUPS {
                let level = group_level(n, g);
                let needs = s.needs[g.octaves.clone()]
                    .iter()
                    .flatten()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                let (word, color) = level_verdict(needs);
                let filled = ((level / 70.0).clamp(0.0, 1.0) * bar_width as f64).round() as usize;
                rows.push(Line::from(vec![
                    Span::styled(format!("{:<6}", g.name), Style::default().bold()),
                    Span::raw(format!("{:<15}", g.range)),
                    Span::raw(format!("{level:>4.0} dB SPL  ")),
                    Span::styled("█".repeat(filled), Style::default().fg(color)),
                    Span::styled(
                        "░".repeat(bar_width - filled),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("  {word:<11}"), Style::default().fg(color)),
                    Span::styled(g.sources, Style::default().fg(Color::DarkGray)),
                ]));
            }
            rows.push(Line::from(""));
            rows.push(Line::from(Span::styled(
                "Levels are the steady background over the last 3 seconds; a cough or a car \
                 passing does not count.",
                Style::default().fg(Color::DarkGray),
            )));
        }
        _ => rows.push(Line::from(format!("Listening… {:.1} s", l.listened_secs()))),
    }
    frame.render_widget(
        Paragraph::new(rows)
            .block(panel("Background noise"))
            .wrap(Wrap { trim: false }),
        noise,
    );

    let mut lines = Vec::new();
    if let Some(s) = &l.suggestion {
        let color = level_verdict(s.level_spl).1;
        let color = if color == Color::Red {
            Color::LightRed
        } else {
            color
        };
        lines.extend(bignum::lines(
            &format!("{:.0}", s.level_spl),
            Style::default().fg(color),
        ));
        lines.push(Line::from(Span::styled(
            "dB SPL",
            Style::default().fg(color),
        )));
        lines.push(Line::from(""));
        let octave = if s.limited_by_hz < 1000.0 {
            format!("{:.0} Hz", s.limited_by_hz)
        } else {
            format!("{:.0} kHz", s.limited_by_hz / 1000.0)
        };
        lines.push(Line::from(if s.too_noisy {
            warn(&format!(
                "The room needs more than {MAX_LEVEL_SPL:.0} dB, most of all around {octave}. \
                 Measure at a quieter moment, or switch off what hums (ventilation, a fridge); \
                 measuring now may be rejected as too noisy."
            ))
        } else if s.level_spl <= MIN_LEVEL_SPL {
            ok(&format!(
                "A quiet room: {MIN_LEVEL_SPL:.0} dB is enough everywhere, and comfortable."
            ))
        } else {
            Span::raw(format!(
                "Set by the background around {octave}: that octave needs the loudest \
                 measurement."
            ))
        }));
        lines.push(Line::from(Span::styled(
            "Enter plays the noise; then turn the amplifier until the meter reads this.",
            Style::default().bold(),
        )));
    }
    let pad = usize::from(suggestion.height.saturating_sub(2)).saturating_sub(lines.len()) / 2;
    lines.splice(0..0, std::iter::repeat_n(Line::from(""), pad));
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(panel("Measure at")),
        suggestion,
    );
}

// ----- 5 -----

fn measure(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
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
    plot::draw(
        frame,
        plot_area,
        "Measured response, dB SPL",
        &curves,
        &[],
        gfx,
    );
}

/// The placement screen: instructions, scores, a big number to read from across the room,
/// the bass of the latest reading over the pinned and the best ones, and in the sweep modes
/// how the bass decays.
fn placement(
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
    let intro = match p.mode {
        Mode::Sweeps => {
            "Sweeps: a bass sweep every couple of seconds while you move the speakers or your \
             seat by 10–50 cm, or try a bass trap in each corner. The waterfall shows how the \
             bass decays: traps shorten the ringing."
        }
        Mode::Fast => {
            "Fast sweeps: a one-second sweep loops without pauses; the curve and the waterfall \
             are recomputed five times a second, so a change shows within a second, decay \
             included."
        }
        Mode::Live => {
            "Live: bass noise plays continuously and the curve is recomputed ten times a second, \
             so it follows a bass trap as you carry it. Sweeps show finer detail and the decay."
        }
    };
    let mut lines = vec![
        Line::from(intro),
        Line::from(
            "Space pins a placement to compare with; keep the one with the smaller number (peaks \
             and dips, not level). Faded green: the most even so far. m changes the mode.",
        ),
        Line::from(""),
    ];
    let speaker = p.channel.name().to_lowercase();
    let (label, state) = match (p.mode, p.paused, p.running()) {
        (_, true, _) => (String::new(), "paused (s resumes)".to_owned()),
        (Mode::Sweeps, false, true) => (
            format!("Sweep {}  ", p.count),
            format!("sweeping the {speaker} speaker…"),
        ),
        (Mode::Fast, false, true) => (
            format!("Fast sweeps, {} updates  ", p.count),
            format!("sweeping the {speaker} speaker in a loop"),
        ),
        (Mode::Live, false, true) => (
            format!("Live, {} updates  ", p.count),
            format!("noise from the {speaker} speaker"),
        ),
        (_, false, false) => (String::new(), "starting…".to_owned()),
    };
    let mut head = vec![Span::raw(label), Span::raw(state)];
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
    if let Some(r) = &p.latest {
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
        lines.push(Line::from(spans));
    }
    if let Some(r) = &p.reference {
        lines.push(Line::from(Span::styled(
            format!("Pinned ({}): {}", r.channel.name(), score_text(&r.score)),
            Style::default().fg(Color::Gray),
        )));
    }
    // The best so far, when it is clearly better than now and not the pinned one itself.
    let best = p.best_other_than_latest().filter(|b| {
        Some(*b) != p.reference.as_ref()
            && p.latest
                .as_ref()
                .is_none_or(|l| b.score.evenness_db < l.score.evenness_db - SAME_DB)
    });
    if let Some(b) = best {
        let mut text = format!(
            "Best so far ({}): {}",
            b.channel.name(),
            score_text(&b.score)
        );
        if let Some(pinned) = p.reference.as_ref().filter(|_| big_area.is_none()) {
            text = format!(
                "{text}, {}",
                compared(b.score.evenness_db, pinned.score.evenness_db).0
            );
        }
        lines.push(Line::from(Span::styled(
            text,
            Style::default().fg(Series::Best.color(gfx)),
        )));
    }
    let latest_decay = p.latest.as_ref().and_then(|r| r.waterfall.as_ref());
    if p.mode != Mode::Live
        && let Some(w) = latest_decay
    {
        lines.push(decay_line(
            w,
            p.reference.as_ref().and_then(|r| r.waterfall.as_ref()),
        ));
    }
    if let Some(issue) = &p.issue {
        lines.push(Line::from(bad(issue)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Placement"))
            .wrap(Wrap { trim: true }),
        text_area,
    );
    if let Some(area) = big_area {
        big_score(frame, area, p, gfx);
    }
    let mut curves = Vec::new();
    if let Some(b) = best {
        curves.push(Curve::new(
            "best",
            Series::Best,
            &b.response.freqs,
            &b.response.db,
        ));
    }
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
    let (curve_area, decay_area) = match (p.mode, latest_decay) {
        (Mode::Sweeps | Mode::Fast, Some(w)) => {
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
            "How the bass fades, latest sweep",
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
const SAME_DB: f64 = 0.3;

/// How a score compares with the pinned one, and its colour: green when more even.
fn compared(now_db: f64, pinned_db: f64) -> (String, Color) {
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
fn big_score(frame: &mut Frame<'_>, area: Rect, p: &crate::app::PlacementState, gfx: &Graphics) {
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
    if let (Some(best), Some(pinned)) = (&p.best[p.channel.index()], &p.reference) {
        lines.push(Line::from(Span::styled(
            format!(
                "best so far {:+.1} dB",
                pinned.score.evenness_db - best.score.evenness_db
            ),
            Style::default().fg(Series::Best.color(gfx)),
        )));
    }
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

/// The time `w` takes to fade near `hz`, ms, and whether that is the longest it measures.
fn fade_at(w: &crate::dsp::decay::Waterfall, hz: f64) -> (f64, bool) {
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

fn ms_text((ms, capped): (f64, bool)) -> String {
    if capped {
        format!("≥{ms:.0} ms")
    } else {
        format!("{ms:.0} ms")
    }
}

/// The notes that ring longest now, and with a pinned sweep, how long they rang then.
fn decay_line(
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
fn decay(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
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

fn progress_bar(progress: f64, width: usize) -> String {
    let filled = ((progress * width as f64) as usize).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

// ----- 6 -----

fn result(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
    let Some(design) = &app.design else {
        frame.render_widget(
            Paragraph::new("No measurement yet.").block(panel("Result")),
            area,
        );
        return;
    };
    let c = app.view_channel;
    let ch = &design.channels[c];
    let [plots, side] =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(38)]).areas(area);
    let [plot_area, eq_area] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(plots);
    let curves = channel_curves(app, c, false);
    let name = if c == 0 { "Left" } else { "Right" };
    let shade = Shade {
        cut_from_hz: ch.target.cut_floor_hz(),
        from_hz: ch.target.low_hz,
        to_hz: ch.target.high_hz,
    };
    plot::draw_with_range(
        frame,
        plot_area,
        &format!("{name} channel (Tab to switch)"),
        &curves,
        &[shade],
        both_channels_range(app, false),
        gfx,
    );
    let eq_curves_of = |c: usize| {
        filter_curves(
            &design.channels[c].filters,
            design.eq.trim_db[c] + design.eq.preamp_db,
        )
    };
    let (eq_curves, range) = eq_curves_of(c);
    // The other channel's range too, so Tab keeps the scale.
    let other = eq_curves_of(1 - c).1;
    let range = (range.0.min(other.0), range.1.max(other.1));
    plot::draw_with_range(
        frame,
        eq_area,
        &format!("{name} equalizer, dB"),
        &eq_curves,
        &[shade],
        range,
        gfx,
    );

    let o = &design.options;
    let mut lines = vec![
        Line::from(format!(
            "Tilt         {:+.2} dB/oct  (t/T)",
            o.tilt_db_per_oct
        )),
        Line::from(format!(
            "High limit   {:<15} (h)",
            match o.high_limit_hz {
                Some(h) => format!("{h:.0} Hz"),
                None => format!("auto ({:.0} Hz)", ch.target.high_hz),
            }
        )),
        Line::from(format!(
            "Filters      {:<15} (f/F)",
            match o.max_filters {
                Some(n) => n.to_string(),
                None => format!("auto ({})", ch.filters.len()),
            }
        )),
        Line::from(""),
        Line::from(format!(
            "Corrected    {:.0}–{:.0} Hz",
            ch.target.low_hz, ch.target.high_hz
        )),
        Line::from(if ch.target.roll_off.is_empty() {
            String::new()
        } else {
            format!("  + cuts to  {:.0} Hz", ch.target.cut_floor_hz())
        }),
        Line::from(vec![
            Span::raw("RMS error    "),
            Span::styled(
                format!("{:.1}", ch.rms_before_db),
                Style::default().fg(Series::Measured.color(gfx)),
            ),
            Span::raw(" → "),
            Span::styled(
                format!("{:.1} dB", ch.rms_after_db),
                Style::default().fg(Series::Predicted.color(gfx)).bold(),
            ),
        ]),
        Line::from(format!("Preamp       {:.1} dB", design.eq.preamp_db)),
        Line::from(format!(
            "L/R balance  {:+.1} dB (trim {:.1})",
            design.imbalance_db, design.eq.trim_db[c]
        )),
        Line::from(""),
        Line::from(Span::styled(
            "   Freq      Gain     Q",
            Style::default().bold(),
        )),
    ];
    // As many filters as fit (inside the border), the rest summarised.
    let room = usize::from(side.height.saturating_sub(2)).saturating_sub(lines.len());
    let shown = if ch.filters.len() > room {
        room.saturating_sub(1)
    } else {
        ch.filters.len()
    };
    for f in &ch.filters[..shown] {
        let color = if f.gain < 0.0 {
            Color::LightRed
        } else {
            Color::LightGreen
        };
        lines.push(Line::from(Span::styled(
            format!("{:>7.1} Hz {:>+5.1} dB {:>5.2}", f.freq, f.gain, f.q),
            Style::default().fg(color),
        )));
    }
    if shown < ch.filters.len() {
        lines.push(Line::from(Span::styled(
            format!("  + {} more (report.txt)", ch.filters.len() - shown),
            Style::default().fg(Color::Gray),
        )));
    }
    if ch.filters.is_empty() {
        lines.push(Line::from("  (no correction needed)"));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)).block(panel("EQ")), side);
}

/// Measured / target / predicted (and optionally verified) curves at display smoothing.
fn channel_curves(app: &App, c: usize, with_verify: bool) -> Vec<Curve> {
    let (Some(design), Some(r)) = (&app.design, &app.measure.responses[c]) else {
        return Vec::new();
    };
    let ch = &design.channels[c];
    let display = &r.display;
    let target = design.display_target(c);
    let eq = response_db(&ch.filters, &display.freqs);
    let predicted: Vec<f64> = display
        .db
        .iter()
        .zip(&eq)
        .map(|(m, e)| m + e + design.eq.trim_db[c])
        .collect();
    let target_db: Vec<f64> = display.freqs.iter().map(|&f| target.db_at(f)).collect();
    let mut curves = vec![
        Curve::new("measured", Series::Measured, &display.freqs, &display.db),
        Curve::new("target", Series::Target, &display.freqs, &target_db),
        Curve::new("predicted", Series::Predicted, &display.freqs, &predicted),
    ];
    if with_verify && let Some(v) = &app.verify_responses()[c] {
        // Align broadband level with the prediction (the EQ host's output has its own volume).
        let offset = mean_diff(&predicted, &v.display.db, &display.freqs, &target);
        let shifted: Vec<f64> = v.display.db.iter().map(|d| d + offset).collect();
        curves.push(Curve::new(
            "verified",
            Series::Verified,
            &v.display.freqs,
            &shifted,
        ));
    }
    curves
}

/// Each filter's own curve and the whole equalizer (filters plus `offset_db`: preamp and
/// trim), on a log grid, with a dB range that fits them (at least ±6 dB).
fn filter_curves(filters: &[crate::eq::Filter], offset_db: f64) -> (Vec<Curve>, (f64, f64)) {
    let freqs = crate::dsp::spectrum::grid();
    let mut curves = Vec::new();
    let (mut cut_labelled, mut boost_labelled) = (false, false);
    for f in filters {
        let db = response_db(std::slice::from_ref(f), &freqs);
        let (series, label) = if f.gain < 0.0 {
            let first = !std::mem::replace(&mut cut_labelled, true);
            (Series::Cut, if first { "cuts" } else { "" })
        } else {
            let first = !std::mem::replace(&mut boost_labelled, true);
            (Series::Boost, if first { "boosts" } else { "" })
        };
        curves.push(Curve::new(label, series, &freqs, &db));
    }
    let total: Vec<f64> = response_db(filters, &freqs)
        .into_iter()
        .map(|d| d + offset_db)
        .collect();
    let (lo, hi) = curves
        .iter()
        .flat_map(|c| c.points.iter().map(|p| p.1))
        .chain(total.iter().copied())
        .fold((-6.0f64, 6.0f64), |(lo, hi), d| (lo.min(d), hi.max(d)));
    curves.push(Curve::new(
        "whole EQ with preamp",
        Series::Eq,
        &freqs,
        &total,
    ));
    let range = ((lo / 3.0).floor() * 3.0, (hi / 3.0).ceil() * 3.0);
    (curves, range)
}

fn mean_diff(a: &[f64], b: &[f64], freqs: &[f64], target: &crate::eq::target::Target) -> f64 {
    let (sum, n) = a
        .iter()
        .zip(b)
        .zip(freqs)
        .filter(|(_, f)| target.in_range(**f))
        .fold((0.0, 0usize), |(s, n), ((x, y), _)| (s + x - y, n + 1));
    if n == 0 { 0.0 } else { sum / n as f64 }
}

// ----- 7 -----

fn save(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
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

/// One dB range for both channels' result charts, so switching or comparing them keeps
/// the scale.
fn both_channels_range(app: &App, with_verify: bool) -> (f64, f64) {
    let curves: Vec<Curve> = (0..2)
        .flat_map(|c| channel_curves(app, c, with_verify))
        .collect();
    plot::y_range(&curves, plot::FULL)
}

/// Verification summary: overall match, then any single spot that differs a lot.
fn verify_lines(v: &[VerifyResult; 2], host: &str, lines: &mut Vec<Line<'static>>) {
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
