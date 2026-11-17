//! The screen's text, as a terminal shows it, drawn on the GPU: ratatui's grid of cells
//! becomes one vello scene. Box lines and block characters are drawn as exact rectangles
//! (as terminals do), so borders join up and big numbers have no seams.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill;

use super::fonts::{Fonts, Metrics};
use crate::ui::raster::gpu::{color, em};
use crate::ui::theme::{Rgb, Theme};

/// Which arms of a box-drawing character are drawn: left, right, up, down.
fn arms(c: char) -> Option<(bool, bool, bool, bool)> {
    Some(match c {
        '─' => (true, true, false, false),
        '│' => (false, false, true, true),
        '┌' => (false, true, false, true),
        '┐' => (true, false, false, true),
        '└' => (false, true, true, false),
        '┘' => (true, false, true, false),
        '├' => (false, true, true, true),
        '┤' => (true, false, true, true),
        '┬' => (true, true, false, true),
        '┴' => (true, true, true, false),
        '┼' => (true, true, true, true),
        _ => return None,
    })
}

/// A block character as the share of the cell it fills (left, top, right, bottom) and its
/// opacity.
fn block(c: char) -> Option<((f32, f32, f32, f32), f32)> {
    Some(match c {
        '█' => ((0.0, 0.0, 1.0, 1.0), 1.0),
        '▀' => ((0.0, 0.0, 1.0, 0.5), 1.0),
        '▄' => ((0.0, 0.5, 1.0, 1.0), 1.0),
        '▌' => ((0.0, 0.0, 0.5, 1.0), 1.0),
        '▐' => ((0.5, 0.0, 1.0, 1.0), 1.0),
        '▏' => ((0.0, 0.0, 0.125, 1.0), 1.0),
        '░' => ((0.0, 0.0, 1.0, 1.0), 0.25),
        '▒' => ((0.0, 0.0, 1.0, 1.0), 0.5),
        '▓' => ((0.0, 0.0, 1.0, 1.0), 0.75),
        _ => return None,
    })
}

/// Rectangles of one colour, merged where they continue each other.
#[derive(Default)]
struct Rects {
    list: Vec<(Rgb, f32, Rect)>,
}

impl Rects {
    fn push(&mut self, c: Rgb, alpha: f32, r: Rect) {
        self.list.push((c, alpha, r));
    }

    fn draw(&self, scene: &mut Scene) {
        for (c, alpha, r) in &self.list {
            scene.fill(Fill::NonZero, Affine::IDENTITY, color(*c, *alpha), None, r);
        }
    }
}

/// A line along a row or a column being extended cell by cell: colour, start, end.
type Open = Option<(Rgb, f64, f64)>;

/// Extends `open` with the span `from`–`to` in `c`, or ends it (into `out`, as `rect` makes
/// it) and starts anew.
fn extend(
    open: &mut Open,
    c: Rgb,
    (from, to): (f64, f64),
    out: &mut Rects,
    rect: impl Fn(f64, f64) -> Rect,
) {
    match open {
        Some((oc, _, end)) if *oc == c && (*end - from).abs() < 0.01 => *end = to,
        _ => {
            close(open, out, &rect);
            *open = Some((c, from, to));
        }
    }
}

fn close(open: &mut Open, out: &mut Rects, rect: impl Fn(f64, f64) -> Rect) {
    if let Some((c, from, to)) = open.take() {
        out.push(c, 1.0, rect(from, to));
    }
}

/// The grid `buf` as a scene, cells `m` large, in `theme`'s colours. The background itself
/// is left to the renderer.
#[must_use]
pub fn scene(buf: &Buffer, theme: &Theme, fonts: &mut Fonts, m: &Metrics) -> Scene {
    let mut scene = Scene::new();
    let (cols, rows) = (buf.area.width, buf.area.height);
    let (cw, ch, t) = (f64::from(m.cell_w), f64::from(m.cell_h), f64::from(m.line));
    let mut backgrounds = Rects::default();
    let mut lines = Rects::default();
    let mut blocks = Rects::default();
    let mut underlines = Rects::default();
    // Text in runs of one font and colour.
    let mut runs: Vec<((usize, Rgb), Vec<vello::Glyph>)> = Vec::new();
    let mut run_of: HashMap<(usize, Rgb), usize> = HashMap::new();
    // Box lines down each column, extended row by row.
    let mut down: Vec<Open> = vec![None; usize::from(cols)];
    let column = |x: u16| {
        let left = (f64::from(x) * cw + (cw - t) / 2.0).floor();
        move |from: f64, to: f64| Rect::new(left, from, left + t, to)
    };

    for y in 0..rows {
        let y0 = f64::from(y) * ch;
        let top = (y0 + (ch - t) / 2.0).floor();
        let row = |from: f64, to: f64| Rect::new(from, top, to, top + t);
        let mut across: Open = None;
        let mut bg_run: Option<(Rgb, u16)> = None;
        for x in 0..=cols {
            // Backgrounds, a rectangle per run of one colour.
            let bg = (x < cols).then(|| theme.cell(buf[(x, y)].bg, false));
            if bg_run.map(|(c, _)| Some(c)) != Some(bg) {
                if let Some((c, from)) = bg_run.take()
                    && c != theme.background
                {
                    backgrounds.push(
                        c,
                        1.0,
                        Rect::new(f64::from(from) * cw, y0, f64::from(x) * cw, y0 + ch),
                    );
                }
                bg_run = bg.map(|c| (c, x));
            }
            if x == cols {
                break;
            }
            let cell = &buf[(x, y)];
            let x0 = f64::from(x) * cw;
            let fg = theme.cell(cell.fg, true);
            let sym = cell.symbol();
            let c = sym.chars().next().unwrap_or(' ');
            let left = (x0 + (cw - t) / 2.0).floor();
            match (arms(c), block(c)) {
                (Some((l, r, u, d)), _) => {
                    if l || r {
                        let from = if l { x0 } else { left };
                        let to = if r { x0 + cw } else { left + t };
                        extend(&mut across, fg, (from, to), &mut lines, row);
                    } else {
                        close(&mut across, &mut lines, row);
                    }
                    let open = &mut down[usize::from(x)];
                    if u || d {
                        let from = if u { y0 } else { top };
                        let to = if d { y0 + ch } else { top + t };
                        extend(open, fg, (from, to), &mut lines, column(x));
                    } else {
                        close(open, &mut lines, column(x));
                    }
                    continue;
                }
                (None, Some(((l, tp, r, b), alpha))) => {
                    blocks.push(
                        fg,
                        alpha,
                        Rect::new(
                            x0 + f64::from(l) * cw,
                            y0 + f64::from(tp) * ch,
                            x0 + f64::from(r) * cw,
                            y0 + f64::from(b) * ch,
                        ),
                    );
                }
                (None, None) if c != ' ' && !sym.is_empty() => {
                    let bold = cell.modifier.contains(Modifier::BOLD);
                    if let Some((face, id)) = fonts.glyph(c, bold) {
                        let key = (face, fg);
                        let k = *run_of.entry(key).or_insert_with(|| {
                            runs.push((key, Vec::new()));
                            runs.len() - 1
                        });
                        runs[k].1.push(vello::Glyph {
                            id: u32::from(id.0),
                            x: x0 as f32,
                            y: y0 as f32 + m.baseline,
                        });
                    }
                }
                (None, None) => {}
            }
            close(&mut across, &mut lines, row);
            close(&mut down[usize::from(x)], &mut lines, column(x));
            if cell.modifier.contains(Modifier::UNDERLINED) {
                let at = y0 + f64::from(m.baseline) + t;
                underlines.push(fg, 1.0, Rect::new(x0, at, x0 + cw, at + t));
            }
        }
        close(&mut across, &mut lines, row);
    }
    for (x, open) in down.iter_mut().enumerate() {
        close(open, &mut lines, column(x as u16));
    }

    backgrounds.draw(&mut scene);
    lines.draw(&mut scene);
    blocks.draw(&mut scene);
    for ((face, fg), glyphs) in runs {
        let face = fonts.face(face);
        scene
            .draw_glyphs(&face.data)
            .font_size(em(&face.glyphs, m.px))
            .brush(color(fg, 1.0))
            .draw(Fill::NonZero, glyphs.into_iter());
    }
    underlines.draw(&mut scene);
    scene
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect as Area;
    use ratatui::style::{Color, Style};
    use ratatui::widgets::{Block, Borders, Widget};

    use super::*;

    #[test]
    fn borders_and_backgrounds_are_a_few_long_rectangles_not_one_per_cell() {
        let Some(mut fonts) = Fonts::load() else {
            eprintln!("skipped: no monospace font");
            return;
        };
        let m = fonts.metrics(9.0, 2.0);
        let theme = Theme::parse("");
        let mut buf = Buffer::empty(Area::new(0, 0, 300, 60));
        // Two panels side by side and a highlighted line across both: hundreds of cells of
        // border and background.
        Block::default()
            .borders(Borders::ALL)
            .render(Area::new(0, 0, 150, 60), &mut buf);
        Block::default()
            .borders(Borders::ALL)
            .render(Area::new(150, 0, 150, 60), &mut buf);
        buf.set_style(Area::new(5, 10, 290, 1), Style::default().bg(Color::Cyan));
        let scene = scene(&buf, &theme, &mut fonts, &m);
        // Per panel two lines across and two down; one background run per panel.
        let paths = scene.encoding().n_paths;
        assert!(paths <= 12, "{paths} shapes for two panels");

        // One panel: its top and bottom, its left and right side, corners included.
        let mut one = Buffer::empty(Area::new(0, 0, 10, 5));
        Block::default()
            .borders(Borders::ALL)
            .render(one.area, &mut one);
        assert_eq!(scene_paths(&one, &theme, &mut fonts, &m), 4);
    }

    fn scene_paths(buf: &Buffer, theme: &Theme, fonts: &mut Fonts, m: &Metrics) -> u32 {
        scene(buf, theme, fonts, m).encoding().n_paths
    }
}
