//! Step 6's screen: measured, target and predicted response, the equalizer's curve and its
//! filters.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;

use super::graphics::Graphics;
use super::panel;
use super::plot::{self, Curve, Series, Shade};
use crate::app::App;
use crate::eq::response_db;

pub(super) fn result(frame: &mut Frame<'_>, area: Rect, app: &App, gfx: &mut Graphics) {
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

    // The options as asked for, at once; the numbers below them follow with the design.
    let o = &app.design_options;
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
        Line::from(if app.designing() {
            Span::styled("updating…", Style::default().fg(Color::DarkGray))
        } else {
            Span::raw("")
        }),
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
pub(super) fn channel_curves(app: &App, c: usize, with_verify: bool) -> Vec<Curve> {
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
pub(super) fn filter_curves(
    filters: &[crate::eq::Filter],
    offset_db: f64,
) -> (Vec<Curve>, (f64, f64)) {
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

pub(super) fn mean_diff(
    a: &[f64],
    b: &[f64],
    freqs: &[f64],
    target: &crate::eq::target::Target,
) -> f64 {
    let (sum, n) = a
        .iter()
        .zip(b)
        .zip(freqs)
        .filter(|(_, f)| target.in_range(**f))
        .fold((0.0, 0usize), |(s, n), ((x, y), _)| (s + x - y, n + 1));
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// One dB range for both channels' result charts, so switching or comparing them keeps
/// the scale.
pub(super) fn both_channels_range(app: &App, with_verify: bool) -> (f64, f64) {
    let curves: Vec<Curve> = (0..2)
        .flat_map(|c| channel_curves(app, c, with_verify))
        .collect();
    plot::y_range(&curves, plot::FULL)
}
