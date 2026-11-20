//! Log-frequency response plots: a pixel image in the theme's colours where the terminal
//! shows images (see [`super::graphics`]), a braille canvas otherwise.

use std::hash::{Hash, Hasher};

use ab_glyph::FontVec;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::canvas::{Canvas as Braille, Line};
use ratatui::widgets::{Block, Borders};

use super::graphics::Graphics;
use super::raster::{Canvas, Pen};
use super::theme::{Rgb, Theme};
use crate::audio::Channel;

/// A frequency span for the x axis, Hz.
pub type HzRange = (f64, f64);
/// The whole audio band.
pub const FULL: HzRange = (20.0, 20_000.0);
/// The bass, where the room rules (placement and decay views).
pub const BASS: HzRange = (20.0, 500.0);

/// Labelled frequencies on the x axis.
const TICKS: [(f64, &str); 10] = [
    (20.0, "20"),
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1000.0, "1k"),
    (2000.0, "2k"),
    (5000.0, "5k"),
    (10_000.0, "10k"),
    (20_000.0, "20k"),
];

/// Unlabelled grid lines between the ticks (images only).
const MINOR: [f64; 15] = [
    30.0, 40.0, 70.0, 300.0, 400.0, 700.0, 3000.0, 4000.0, 7000.0, 60.0, 80.0, 600.0, 800.0,
    6000.0, 8000.0,
];

/// Ticks for the bass span: more of them labelled.
const BASS_TICKS: [(f64, &str); 9] = [
    (20.0, "20"),
    (30.0, "30"),
    (50.0, "50"),
    (70.0, "70"),
    (100.0, "100"),
    (150.0, "150"),
    (200.0, "200"),
    (300.0, "300"),
    (500.0, "500"),
];
const BASS_MINOR: [f64; 6] = [40.0, 60.0, 80.0, 90.0, 250.0, 400.0];

/// Labelled and unlabelled grid frequencies for a span, those inside it.
fn ticks(x: HzRange) -> (Vec<(f64, &'static str)>, Vec<f64>) {
    let (major, minor): (&[(f64, &str)], &[f64]) = if x.1 / x.0 < 100.0 {
        (&BASS_TICKS, &BASS_MINOR)
    } else {
        (&TICKS, &MINOR)
    };
    let inside = |f: f64| f >= x.0 * 0.999 && f <= x.1 * 1.001;
    (
        major.iter().copied().filter(|t| inside(t.0)).collect(),
        minor.iter().copied().filter(|&f| inside(f)).collect(),
    )
}

/// What a curve shows, which sets its colour and line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Series {
    Measured,
    Target,
    Predicted,
    Verified,
    Left,
    Right,
    /// One cutting filter's own response.
    Cut,
    /// One boosting filter's own response.
    Boost,
    /// The whole equalizer: filters, preamp and trim.
    Eq,
    /// A pinned curve to compare against, drawn behind the others.
    Reference,
    /// One mic position of a channel, faint behind the channel's average.
    Position(Channel),
    /// One time slice of a decay view, 0 (the start) to 5 (late).
    Slice(u8),
}

impl Series {
    /// The colour for text about this series (legends, numbers): the image's colour when
    /// charts are images, the terminal's otherwise.
    #[must_use]
    pub fn color(self, gfx: &Graphics) -> Color {
        if gfx.images() {
            self.theme_color(gfx.theme()).tui()
        } else {
            self.terminal_color()
        }
    }

    fn terminal_color(self) -> Color {
        match self {
            Self::Measured | Self::Left => Color::Yellow,
            Self::Target => Color::Green,
            Self::Predicted => Color::Cyan,
            Self::Verified => Color::Magenta,
            Self::Right => Color::LightBlue,
            Self::Cut => Color::Red,
            Self::Boost => Color::Green,
            Self::Eq => Color::White,
            Self::Reference => Color::Gray,
            // Braille has no faint colours: grey, so the averages stand out.
            Self::Position(_) => Color::DarkGray,
            Self::Slice(k) => [
                Color::Red,
                Color::LightRed,
                Color::Yellow,
                Color::Green,
                Color::Cyan,
                Color::Blue,
            ][usize::from(k).min(5)],
        }
    }

    fn theme_color(self, t: &Theme) -> Rgb {
        match self {
            Self::Measured | Self::Left => t.orange,
            Self::Target => t.green,
            Self::Predicted => t.blue,
            Self::Verified => t.magenta,
            Self::Right => t.cyan,
            Self::Cut => t.red,
            Self::Boost => t.green,
            Self::Eq => t.foreground,
            Self::Reference => t.foreground,
            Self::Position(Channel::Left) => t.orange,
            Self::Position(Channel::Right) => t.cyan,
            Self::Slice(k) => time_color(t, f32::from(k) / 5.0),
        }
    }

    /// The image line: the target dashed, the measurement a little lighter than the result.
    fn pen(self, t: &Theme, label_px: f32) -> Pen {
        let color = self.theme_color(t);
        let width = (label_px / 9.0).max(1.5);
        match self {
            Self::Target => Pen::dashed(color, 0.9, width * 0.8, label_px * 0.6),
            Self::Measured => Pen::solid(color, 0.85, width * 0.8),
            Self::Predicted | Self::Verified | Self::Left | Self::Right => {
                Pen::solid(color, 1.0, width)
            }
            Self::Cut | Self::Boost => Pen::solid(color, 0.6, width * 0.6),
            Self::Eq => Pen::solid(color, 1.0, width * 1.1),
            // Solid and as wide as the latest curve, so it reads at a glance, but dimmer
            // and drawn first, so the latest stays on top.
            Self::Reference => Pen::solid(color, 0.55, width * 1.3),
            // Barely there: the spread between positions as a haze around the average, not
            // lines to read one by one.
            Self::Position(_) => Pen::solid(color, 0.16, width * 0.45),
            Self::Slice(_) => Pen::solid(color, 0.95, width * 0.9),
        }
    }

    /// Drawing order: the target and single filters behind the rest.
    fn layer(self) -> u8 {
        u8::from(!matches!(
            self,
            Self::Target | Self::Cut | Self::Boost | Self::Reference | Self::Position(_)
        ))
    }
}

/// A colour along time: the theme's red at the start through yellow and green to blue.
fn time_color(t: &Theme, at: f32) -> Rgb {
    let stops = [t.red, t.orange, t.yellow, t.green, t.cyan, t.blue];
    let x = at.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (x.floor() as usize).min(stops.len() - 2);
    let f = x - i as f32;
    let mix = |a: u8, b: u8| (f32::from(a) * (1.0 - f) + f32::from(b) * f).round() as u8;
    let (a, b) = (stops[i], stops[i + 1]);
    Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// One curve: (Hz, dB) points.
#[derive(Debug, Clone)]
pub struct Curve {
    pub label: String,
    pub series: Series,
    pub points: Vec<(f64, f64)>,
}

impl Curve {
    #[must_use]
    pub fn new(label: &str, series: Series, freqs: &[f64], db: &[f64]) -> Self {
        Self {
            label: label.to_owned(),
            series,
            points: freqs.iter().copied().zip(db.iter().copied()).collect(),
        }
    }
}

/// The corrected frequency range; the plot marks its edges and dims what lies outside.
/// From `cut_from_hz` up to `from_hz` only resonances are cut (dimmed less).
#[derive(Debug, Clone, Copy)]
pub struct Shade {
    pub cut_from_hz: f64,
    pub from_hz: f64,
    pub to_hz: f64,
}

/// Draws curves on a 20 Hz – 20 kHz log axis with a dB grid, in a titled box, over the
/// given dB range. Curves with an empty label stay out of the legend.
pub fn draw_with_range(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    curves: &[Curve],
    shades: &[Shade],
    range: (f64, f64),
    gfx: &mut Graphics,
) {
    draw_axes(frame, area, title, curves, shades, FULL, range, gfx);
}

/// Curves over the span `x` and the dB range `range`.
#[expect(
    clippy::too_many_arguments,
    reason = "a chart's parts; a struct for them would only be built here"
)]
pub fn draw_axes(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    curves: &[Curve],
    shades: &[Shade],
    x: HzRange,
    range: (f64, f64),
    gfx: &mut Graphics,
) {
    let images = gfx.images();
    let legend: Vec<Span<'_>> = curves
        .iter()
        .filter(|c| !c.label.is_empty())
        .flat_map(|c| {
            [
                Span::styled("── ", Style::default().fg(c.series.color(gfx))),
                Span::raw(format!("{}   ", c.label)),
            ]
        })
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_bottom(TextLine::from(legend));
    if images {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let content = content_hash(curves, shades, x, range);
        gfx.chart(frame, inner, title, content, |canvas, theme, font| {
            render(canvas, theme, font, curves, shades, x, range);
        });
    } else {
        draw_braille(frame, area, block, curves, shades, x, range);
    }
}

/// The chart as an image of `width` × `height` pixels, with labels sized for terminal cells
/// `cell_w` pixels wide (for exporting, and for checking the drawing).
#[must_use]
pub fn to_image(
    curves: &[Curve],
    shades: &[Shade],
    (width, height): (u32, u32),
    cell_w: f32,
    theme: &Theme,
    font: Option<&FontVec>,
) -> image::DynamicImage {
    let mut canvas = Canvas::new(width, height, theme.background, cell_w);
    render(
        &mut canvas,
        theme,
        font,
        curves,
        shades,
        FULL,
        y_range(curves, FULL),
    );
    canvas.into_image()
}

fn content_hash(curves: &[Curve], shades: &[Shade], x: HzRange, range: (f64, f64)) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (x.0.to_bits(), x.1.to_bits()).hash(&mut h);
    for c in curves {
        c.series.hash(&mut h);
        c.label.hash(&mut h);
        for (f, d) in &c.points {
            (f.to_bits(), d.to_bits()).hash(&mut h);
        }
    }
    for s in shades {
        (
            s.cut_from_hz.to_bits(),
            s.from_hz.to_bits(),
            s.to_hz.to_bits(),
        )
            .hash(&mut h);
    }
    (range.0.to_bits(), range.1.to_bits()).hash(&mut h);
    h.finish()
}

/// The chart as pixels, styled like whereami's: theme background, a faint grid, labels at
/// the terminal's text size, curves as antialiased lines.
fn render(
    c: &mut Canvas,
    t: &Theme,
    font: Option<&FontVec>,
    curves: &[Curve],
    shades: &[Shade],
    (low_hz, high_hz): HzRange,
    (lo, hi): (f64, f64),
) {
    let (major_ticks, minor_ticks) = ticks((low_hz, high_hz));
    let px = c.label();
    let left = Canvas::text_width(font, "100", px) + px * 0.7;
    let right = Canvas::text_width(font, "20k", px) * 0.5 + px * 0.3;
    let (top, bottom) = (px * 0.6, px * 1.6);
    let (x0, x1, y0, y1) = (left, c.w - right, top, c.h - bottom);
    let span = (high_hz / low_hz).log10();
    let x_at = |f: f64| x0 + ((f / low_hz).log10() / span) as f32 * (x1 - x0);
    let y_at = |db: f64| y0 + ((hi - db) / (hi - lo)) as f32 * (y1 - y0);

    for s in shades {
        let cut = x_at(s.cut_from_hz.clamp(low_hz, high_hz));
        let (a, b) = (x_at(s.from_hz.max(low_hz)), x_at(s.to_hz.min(high_hz)));
        c.rect(x0, y0, cut - x0, y1 - y0, t.muted, 0.22);
        c.rect(cut, y0, a - cut, y1 - y0, t.muted, 0.1);
        c.rect(b, y0, x1 - b, y1 - y0, t.muted, 0.22);
    }
    for &f in &minor_ticks {
        let x = x_at(f);
        c.line((x, y0), (x, y1), Pen::solid(t.muted, 0.18, 1.0));
    }
    for &(f, label) in &major_ticks {
        let x = x_at(f);
        c.line((x, y0), (x, y1), Pen::solid(t.muted, 0.45, 1.0));
        let w = Canvas::text_width(font, label, px);
        let min_lx = x0 - w / 2.0;
        let max_lx = (c.w - w - 2.0).max(min_lx);
        let lx = (x - w / 2.0).clamp(min_lx, max_lx);
        c.text(font, label, (lx, y1 + px * 0.3), px, t.foreground, 0.75);
    }
    let (minor, major_step) = grid_steps(lo, hi);
    let mut db = (lo / minor).ceil() * minor;
    while db <= hi + 1e-9 {
        let y = y_at(db);
        let major = ((db / major_step).round() * major_step - db).abs() < 1e-6;
        let alpha = if major { 0.55 } else { 0.25 };
        c.line((x0, y), (x1, y), Pen::solid(t.muted, alpha, 1.0));
        if major {
            let label = format!("{db:.0}");
            let w = Canvas::text_width(font, &label, px);
            c.text(
                font,
                &label,
                (
                    left - w - px * 0.4,
                    (y - px * 0.6).clamp(0.0, (c.h - px * 1.3).max(0.0)),
                ),
                px,
                t.foreground,
                0.75,
            );
        }
        db += minor;
    }
    for s in shades {
        for f in [s.from_hz, s.to_hz] {
            if f > low_hz && f < high_hz {
                let x = x_at(f);
                c.line((x, y0), (x, y1), Pen::dashed(t.accent, 0.8, 1.5, px * 0.45));
            }
        }
    }

    let mut order: Vec<&Curve> = curves.iter().collect();
    order.sort_by_key(|c| c.series.layer());
    for curve in order {
        let points: Vec<Option<(f32, f32)>> = curve
            .points
            .iter()
            .filter(|(f, _)| (low_hz..=high_hz).contains(f))
            .map(|&(f, d)| d.is_finite().then(|| (x_at(f), y_at(d.clamp(lo, hi)))))
            .collect();
        c.polyline(&points, curve.series.pen(t, px));
    }
}

fn draw_braille(
    frame: &mut Frame<'_>,
    area: Rect,
    block: Block<'_>,
    curves: &[Curve],
    shades: &[Shade],
    (low_hz, high_hz): HzRange,
    (lo, hi): (f64, f64),
) {
    let (major_ticks, _) = ticks((low_hz, high_hz));
    let x0 = low_hz.log10();
    let x1 = high_hz.log10();
    // Width of one character cell in axis units, for placing labels.
    let cell = (x1 - x0) / f64::from(area.width.saturating_sub(2).max(1));
    let canvas = Braille::default()
        .block(block)
        .marker(Marker::Braille)
        .x_bounds([x0, x1])
        .y_bounds([lo, hi])
        .paint(|ctx| {
            let grid = Color::DarkGray;
            for s in shades {
                // Dotted vertical boundaries for the corrected range.
                for f in [s.from_hz, s.to_hz] {
                    if f > low_hz && f < high_hz {
                        let x = f.log10();
                        let mut y = lo;
                        while y < hi {
                            ctx.draw(&Line::new(x, y, x, (y + 0.6).min(hi), Color::Blue));
                            y += 1.5;
                        }
                    }
                }
            }
            for &(f, _) in &major_ticks {
                ctx.draw(&Line::new(f.log10(), lo, f.log10(), hi, grid));
            }
            let (minor, major) = grid_steps(lo, hi);
            let mut y = (lo / minor).ceil() * minor;
            while y <= hi + 1e-9 {
                ctx.draw(&Line::new(x0, y, x1, y, grid));
                y += minor;
            }
            ctx.layer();
            for c in curves {
                for w in c.points.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    if a.0 < low_hz || b.0 > high_hz || !a.1.is_finite() || !b.1.is_finite() {
                        continue;
                    }
                    ctx.draw(&Line::new(
                        a.0.log10(),
                        a.1.clamp(lo, hi),
                        b.0.log10(),
                        b.1.clamp(lo, hi),
                        c.series.terminal_color(),
                    ));
                }
                ctx.layer();
            }
            for &(f, label) in &major_ticks {
                let len = label.len() as f64;
                let min_x = x0 + cell * 3.0;
                let max_x = (x1 - cell * len).max(min_x);
                let x = (f.log10() - cell * len / 2.0).clamp(min_x, max_x);
                ctx.print(x, lo, Span::styled(label, Style::default().fg(Color::Gray)));
            }
            // dB labels on the left edge, skipping the bottom row used by the Hz labels.
            let mut y = ((lo + 1.0) / major).ceil() * major;
            while y <= hi + 1e-9 {
                ctx.print(
                    x0,
                    y,
                    Span::styled(format!("{y:.0}"), Style::default().fg(Color::Gray)),
                );
                y += major;
            }
        });
    frame.render_widget(canvas, area);
}

/// dB grid spacing for a range: (every line, labelled lines).
fn grid_steps(lo: f64, hi: f64) -> (f64, f64) {
    if hi - lo > 36.0 {
        (5.0, 10.0)
    } else {
        (3.0, 6.0)
    }
}

/// A y range covering the curves within `x`, ignoring the extremes, at least 40 dB tall.
#[must_use]
pub fn y_range(curves: &[Curve], x: HzRange) -> (f64, f64) {
    let mut values: Vec<f64> = curves
        .iter()
        .flat_map(|c| c.points.iter())
        .filter(|(f, d)| (x.0..=x.1).contains(f) && d.is_finite())
        .map(|p| p.1)
        .collect();
    if values.is_empty() {
        return (40.0, 100.0);
    }
    values.sort_by(f64::total_cmp);
    // Ignore the extreme 1% so deep nulls do not squash the plot.
    let lo = values[values.len() / 100];
    let hi = values[values.len() - 1 - values.len() / 100];
    let mid = (lo + hi) / 2.0;
    let half = ((hi - lo) / 2.0 + 4.0).max(20.0);
    (
        ((mid - half) / 5.0).floor() * 5.0,
        ((mid + half) / 5.0).ceil() * 5.0,
    )
}

/// A decay view: slices of the bass response at successive times after the direct sound.
#[derive(Debug, Clone, Copy)]
pub struct Decay<'a> {
    /// (ms after the direct sound, dB relative to the loudest start) per slice, earliest
    /// first, on shared frequencies.
    pub times_ms: &'a [f64],
    pub freqs: &'a [f64],
    pub slices: &'a [Vec<f64>],
    /// Frequencies to label as ringing.
    pub marks: &'a [f64],
}

/// The lowest level shown in the decay view, dB below the loudest start.
const DECAY_FLOOR_DB: f64 = -45.0;

/// Draws a decay view: a waterfall (time running towards the viewer) as an image, or a few
/// slices as curves in text terminals.
pub fn draw_decay(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    decay: Decay<'_>,
    gfx: &mut Graphics,
) {
    let picks: Vec<usize> = [0.0, 50.0, 100.0, 200.0, 300.0, 400.0]
        .iter()
        .filter_map(|&t| decay.times_ms.iter().position(|&m| (m - t).abs() < 1e-6))
        .collect();
    if !gfx.images() {
        let curves: Vec<Curve> = picks
            .iter()
            .enumerate()
            .map(|(i, &k)| {
                Curve::new(
                    &format!("{:.0} ms", decay.times_ms[k]),
                    Series::Slice(i as u8),
                    decay.freqs,
                    &decay.slices[k],
                )
            })
            .collect();
        draw_axes(
            frame,
            area,
            title,
            &curves,
            &[],
            BASS,
            (DECAY_FLOOR_DB, 3.0),
            gfx,
        );
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_bottom(TextLine::from(Span::styled(
            if area.width > 100 {
                " time runs towards you: the back slice is the sound as it starts, each one in front 20 ms later "
            } else {
                " time runs towards you, 20 ms per slice "
            },
            Style::default().fg(Color::Gray),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in decay.slices {
        for d in s {
            d.to_bits().hash(&mut h);
        }
    }
    for m in decay.marks {
        m.to_bits().hash(&mut h);
    }
    gfx.chart(frame, inner, title, h.finish(), |canvas, theme, font| {
        render_waterfall(canvas, theme, font, decay);
    });
}

/// The waterfall as pixels: slices drawn from the back (the start) to the front (late),
/// each hiding what lies behind it.
fn render_waterfall(c: &mut Canvas, t: &Theme, font: Option<&FontVec>, d: Decay<'_>) {
    let k_count = d.slices.len();
    if k_count == 0 || d.freqs.is_empty() {
        return;
    }
    let (lo, hi) = (DECAY_FLOOR_DB, 3.0);
    let (low_hz, high_hz) = BASS;
    let px = c.label();
    let left = Canvas::text_width(font, "-40", px) + px * 0.7;
    let right = Canvas::text_width(font, "400 ms", px) + px * 0.6;
    let (top, bottom) = (px * 1.6, px * 1.8);
    let (avail_w, avail_h) = (c.w - left - right, c.h - top - bottom);
    // Depth: the back slice sits up and to the right of the front one.
    let (depth_x, depth_y) = (avail_w * 0.22, avail_h * 0.42);
    let (w, h) = (avail_w - depth_x, avail_h - depth_y);
    let (x0, y1) = (left, c.h - bottom);
    let span = (high_hz / low_hz).log10();
    let x_at = |f: f64| x0 + ((f / low_hz).log10() / span) as f32 * w;
    let y_at = |db: f64| y1 - ((db.clamp(lo, hi) - lo) / (hi - lo)) as f32 * h;
    let shift = |k: usize| {
        let o = if k_count > 1 {
            (k_count - 1 - k) as f32 / (k_count - 1) as f32
        } else {
            0.0
        };
        (o * depth_x, -o * depth_y)
    };
    let (major_ticks, _) = ticks(BASS);
    let grid = Pen::solid(t.muted, 0.35, 1.0);
    // The floor: frequency lines running from the front to the back.
    let (bx, by) = shift(0);
    for &(f, label) in &major_ticks {
        let x = x_at(f);
        c.line((x, y1), (x + bx, y1 + by), grid);
        let tw = Canvas::text_width(font, label, px);
        c.text(
            font,
            label,
            (x - tw / 2.0, y1 + px * 0.3),
            px,
            t.foreground,
            0.75,
        );
    }
    c.line((x0, y1), (x0 + w, y1), grid);
    c.line((x0 + bx, y1 + by), (x0 + w + bx, y1 + by), grid);
    // The back wall: dB lines behind the first slice.
    let mut db = (lo / 10.0).ceil() * 10.0;
    while db <= hi {
        let y = y_at(db) + by;
        c.line((x0 + bx, y), (x0 + w + bx, y), grid);
        db += 10.0;
    }
    c.line((x0 + bx, y_at(hi) + by), (x0 + bx, y1 + by), grid);
    // Slices, back to front.
    for (k, slice) in d.slices.iter().enumerate() {
        let (sx, sy) = shift(k);
        let color = time_color(t, k as f32 / (k_count - 1).max(1) as f32);
        let points: Vec<(f32, f32)> = d
            .freqs
            .iter()
            .zip(slice)
            .filter(|(f, v)| (low_hz..=high_hz).contains(*f) && v.is_finite())
            .map(|(&f, &v)| (x_at(f) + sx, y_at(v) + sy))
            .collect();
        let (Some(first), Some(last)) = (points.first().copied(), points.last().copied()) else {
            continue;
        };
        let floor = y1 + sy;
        let mut outline = points.clone();
        outline.push((last.0, floor));
        outline.push((first.0, floor));
        c.fill(&outline, t.background, 1.0);
        c.fill(&outline, color, 0.16);
        let line: Vec<Option<(f32, f32)>> = points.iter().map(|&p| Some(p)).collect();
        let width = (px / 10.0).max(1.2);
        c.polyline(&line, Pen::solid(color, 0.95, width));
        c.line(
            (first.0, floor),
            (last.0, floor),
            Pen::solid(color, 0.35, 1.0),
        );
        // Time labels on the right, every 100 ms.
        let ms = d.times_ms.get(k).copied().unwrap_or(0.0);
        if (ms / 100.0 - (ms / 100.0).round()).abs() < 1e-6 {
            let label = format!("{ms:.0} ms");
            c.text(
                font,
                &label,
                (x0 + w + sx + px * 0.4, floor - px * 0.7),
                px,
                color,
                0.9,
            );
        }
    }
    // dB labels at the front.
    let mut db = (lo / 10.0).ceil() * 10.0;
    while db <= hi {
        let label = format!("{db:.0}");
        let tw = Canvas::text_width(font, &label, px);
        c.text(
            font,
            &label,
            (x0 - tw - px * 0.4, y_at(db) - px * 0.6),
            px,
            t.foreground,
            0.75,
        );
        db += 10.0;
    }
    // Ringing notes, labelled above the back slice.
    for &f in d.marks {
        let x = x_at(f) + bx;
        let label = format!("{f:.0} Hz");
        let tw = Canvas::text_width(font, &label, px);
        c.line(
            (x, top - px * 0.2),
            (x, y_at(hi) + by),
            Pen::dashed(t.accent, 0.8, 1.2, px * 0.3),
        );
        c.text(
            font,
            &label,
            (x - tw / 2.0, top - px * 1.4),
            px,
            t.accent,
            0.95,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_waterfall() {
        let freqs: Vec<f64> = crate::dsp::spectrum::grid()
            .into_iter()
            .filter(|&f| f <= 500.0)
            .collect();
        let times: Vec<f64> = (0..21).map(|k| f64::from(k) * 20.0).collect();
        let slices: Vec<Vec<f64>> = times
            .iter()
            .map(|t| {
                freqs
                    .iter()
                    .map(|f| -t / 10.0 - (f / 100.0).log2().abs())
                    .collect()
            })
            .collect();
        let theme = Theme::parse("");
        let mut canvas = Canvas::new(900, 500, theme.background, 10.0);
        render_waterfall(
            &mut canvas,
            &theme,
            None,
            Decay {
                times_ms: &times,
                freqs: &freqs,
                slices: &slices,
                marks: &[97.0],
            },
        );
        let img = canvas.into_image().to_rgba8();
        let bg = image::Rgba([
            theme.background.0,
            theme.background.1,
            theme.background.2,
            255,
        ]);
        assert!(img.pixels().filter(|p| **p != bg).count() > 5000);
    }

    #[test]
    fn renders_an_image_of_the_requested_size() {
        let freqs: Vec<f64> = (0..200)
            .map(|i| 20.0 * 1000f64.powf(f64::from(i) / 199.0))
            .collect();
        let db: Vec<f64> = freqs
            .iter()
            .map(|f| 75.0 + 5.0 * (f.log10() * 3.0).sin())
            .collect();
        let curves = [
            Curve::new("measured", Series::Measured, &freqs, &db),
            Curve::new("target", Series::Target, &freqs, &vec![75.0; freqs.len()]),
        ];
        let shades = [Shade {
            cut_from_hz: 25.0,
            from_hz: 60.0,
            to_hz: 10_000.0,
        }];
        let theme = Theme::parse("");
        let mut canvas = Canvas::new(800, 400, theme.background, 10.0);
        render(
            &mut canvas,
            &theme,
            None,
            &curves,
            &shades,
            FULL,
            y_range(&curves, FULL),
        );
        let img = canvas.into_image().to_rgba8();
        assert_eq!(img.dimensions(), (800, 400));
        // Something besides the background was drawn.
        let bg = image::Rgba([
            theme.background.0,
            theme.background.1,
            theme.background.2,
            255,
        ]);
        assert!(img.pixels().filter(|p| **p != bg).count() > 1000);
    }

    #[test]
    fn tiny_plots_do_not_panic() {
        let theme = Theme::parse("");
        let curves = [Curve::new(
            "measured",
            Series::Measured,
            &[20.0, 100.0, 1000.0],
            &[70.0, f64::NAN, 75.0],
        )];
        let mut canvas = Canvas::new(12, 8, theme.background, 10.0);
        render(&mut canvas, &theme, None, &curves, &[], FULL, (50.0, 90.0));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(6, 4)).expect("backend");
        let mut gfx = Graphics::text_only();
        terminal
            .draw(|f| {
                draw_axes(
                    f,
                    f.area(),
                    "tiny",
                    &curves,
                    &[],
                    FULL,
                    (50.0, 90.0),
                    &mut gfx,
                )
            })
            .expect("draw tiny braille plot");
    }
}
