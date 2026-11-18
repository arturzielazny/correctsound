//! The screen's text, as a terminal shows it, as shapes to draw: ratatui's grid of cells
//! becomes a few rectangles and runs of text, for the window (on the GPU) and the web page
//! (on a canvas). Box lines and block characters become exact rectangles, as terminals draw
//! them, so borders join up and big numbers have no seams; lines continuing each other are
//! one rectangle, so a panel's border is four.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use super::theme::{Rgb, Theme};

/// A cell's size in pixels, and the thickness of box lines and underlines.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellSize {
    pub w: f64,
    pub h: f64,
    pub line: f64,
    /// Where the text's baseline is, down from a cell's top.
    pub baseline: f64,
}

/// A rectangle in pixels: left, top, right, bottom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// A filled rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fill {
    pub color: Rgb,
    pub alpha: f32,
    pub area: Area,
}

/// Text in one colour and weight on one row, a character per cell from column `x`.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub x: u16,
    pub y: u16,
    pub color: Rgb,
    pub bold: bool,
    pub text: String,
}

/// The screen as shapes, in the order they are drawn: backgrounds, box lines, blocks, the
/// text, then underlines. The theme's background itself is left out.
#[derive(Debug, Default)]
pub struct Shapes {
    pub backgrounds: Vec<Fill>,
    pub lines: Vec<Fill>,
    pub blocks: Vec<Fill>,
    pub text: Vec<Run>,
    pub underlines: Vec<Fill>,
}

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
fn block(c: char) -> Option<((f64, f64, f64, f64), f32)> {
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

/// A line along a row or a column being extended cell by cell: colour, start, end.
type Open = Option<(Rgb, f64, f64)>;

/// Extends `open` with the span `from`–`to` in `c`, or ends it (into `out`, as `rect` makes
/// it) and starts anew.
fn extend(
    open: &mut Open,
    c: Rgb,
    (from, to): (f64, f64),
    out: &mut Vec<Fill>,
    rect: impl Fn(f64, f64) -> Area,
) {
    match open {
        Some((oc, _, end)) if *oc == c && (*end - from).abs() < 0.01 => *end = to,
        _ => {
            close(open, out, &rect);
            *open = Some((c, from, to));
        }
    }
}

fn close(open: &mut Open, out: &mut Vec<Fill>, rect: impl Fn(f64, f64) -> Area) {
    if let Some((color, from, to)) = open.take() {
        out.push(Fill {
            color,
            alpha: 1.0,
            area: rect(from, to),
        });
    }
}

/// A run of text being collected along a row, and the spaces after it (which join it to
/// more text in the same style, or are dropped).
#[derive(Default)]
struct OpenRun {
    run: Option<Run>,
    spaces: usize,
}

impl OpenRun {
    fn push(&mut self, x: u16, y: u16, color: Rgb, bold: bool, c: char, out: &mut Vec<Run>) {
        match &mut self.run {
            Some(r) if r.color == color && r.bold == bold => {
                r.text.extend(std::iter::repeat_n(' ', self.spaces));
                r.text.push(c);
            }
            _ => {
                self.close(out);
                self.run = Some(Run {
                    x,
                    y,
                    color,
                    bold,
                    text: c.to_string(),
                });
            }
        }
        self.spaces = 0;
    }

    fn space(&mut self) {
        if self.run.is_some() {
            self.spaces += 1;
        }
    }

    fn close(&mut self, out: &mut Vec<Run>) {
        out.extend(self.run.take());
        self.spaces = 0;
    }
}

/// The grid `buf` as shapes, cells `m` large, in `theme`'s colours.
#[must_use]
pub fn shapes(buf: &Buffer, theme: &Theme, m: CellSize) -> Shapes {
    let mut out = Shapes::default();
    let (cols, rows) = (buf.area.width, buf.area.height);
    let (cw, ch, t) = (m.w, m.h, m.line);
    // Box lines down each column, extended row by row.
    let mut down: Vec<Open> = vec![None; usize::from(cols)];
    let column = |x: u16| {
        let left = (f64::from(x) * cw + (cw - t) / 2.0).floor();
        move |from: f64, to: f64| Area {
            x0: left,
            y0: from,
            x1: left + t,
            y1: to,
        }
    };

    for y in 0..rows {
        let y0 = f64::from(y) * ch;
        let top = (y0 + (ch - t) / 2.0).floor();
        let row = |from: f64, to: f64| Area {
            x0: from,
            y0: top,
            x1: to,
            y1: top + t,
        };
        let mut across: Open = None;
        let mut bg_run: Option<(Rgb, u16)> = None;
        let mut text = OpenRun::default();
        for x in 0..=cols {
            // Backgrounds, a rectangle per run of one colour.
            let bg = (x < cols).then(|| theme.cell(buf[(x, y)].bg, false));
            if bg_run.map(|(c, _)| Some(c)) != Some(bg) {
                if let Some((color, from)) = bg_run.take()
                    && color != theme.background
                {
                    out.backgrounds.push(Fill {
                        color,
                        alpha: 1.0,
                        area: Area {
                            x0: f64::from(from) * cw,
                            y0,
                            x1: f64::from(x) * cw,
                            y1: y0 + ch,
                        },
                    });
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
                    text.close(&mut out.text);
                    if l || r {
                        let from = if l { x0 } else { left };
                        let to = if r { x0 + cw } else { left + t };
                        extend(&mut across, fg, (from, to), &mut out.lines, row);
                    } else {
                        close(&mut across, &mut out.lines, row);
                    }
                    let open = &mut down[usize::from(x)];
                    if u || d {
                        let from = if u { y0 } else { top };
                        let to = if d { y0 + ch } else { top + t };
                        extend(open, fg, (from, to), &mut out.lines, column(x));
                    } else {
                        close(open, &mut out.lines, column(x));
                    }
                    continue;
                }
                (None, Some(((l, tp, r, b), alpha))) => {
                    text.close(&mut out.text);
                    out.blocks.push(Fill {
                        color: fg,
                        alpha,
                        area: Area {
                            x0: x0 + l * cw,
                            y0: y0 + tp * ch,
                            x1: x0 + r * cw,
                            y1: y0 + b * ch,
                        },
                    });
                }
                // The second cell of a wide character, already drawn.
                (None, None) if sym.is_empty() => text.close(&mut out.text),
                (None, None) if c == ' ' => text.space(),
                (None, None) => {
                    let bold = cell.modifier.contains(Modifier::BOLD);
                    text.push(x, y, fg, bold, c, &mut out.text);
                }
            }
            close(&mut across, &mut out.lines, row);
            close(&mut down[usize::from(x)], &mut out.lines, column(x));
            if cell.modifier.contains(Modifier::UNDERLINED) {
                let at = y0 + m.baseline + t;
                out.underlines.push(Fill {
                    color: fg,
                    alpha: 1.0,
                    area: Area {
                        x0,
                        y0: at,
                        x1: x0 + cw,
                        y1: at + t,
                    },
                });
            }
        }
        text.close(&mut out.text);
        close(&mut across, &mut out.lines, row);
    }
    for (x, open) in down.iter_mut().enumerate() {
        close(open, &mut out.lines, column(x as u16));
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use ratatui::widgets::{Block, Borders, Widget};

    use super::*;

    const CELL: CellSize = CellSize {
        w: 10.0,
        h: 20.0,
        line: 2.0,
        baseline: 15.0,
    };

    #[test]
    fn borders_and_backgrounds_are_a_few_long_rectangles_not_one_per_cell() {
        let theme = Theme::parse("");
        let mut buf = Buffer::empty(Rect::new(0, 0, 300, 60));
        // Two panels side by side and a highlighted line across both: hundreds of cells of
        // border and background.
        Block::default()
            .borders(Borders::ALL)
            .render(Rect::new(0, 0, 150, 60), &mut buf);
        Block::default()
            .borders(Borders::ALL)
            .render(Rect::new(150, 0, 150, 60), &mut buf);
        buf.set_style(Rect::new(5, 10, 290, 1), Style::default().bg(Color::Cyan));
        let s = shapes(&buf, &theme, CELL);
        // Per panel two lines across and two down; the highlight is one background.
        assert!(s.lines.len() <= 8, "{} lines for two panels", s.lines.len());
        assert_eq!(s.backgrounds.len(), 1);

        // One panel: its top and bottom, its left and right side, corners included.
        let mut one = Buffer::empty(Rect::new(0, 0, 10, 5));
        Block::default()
            .borders(Borders::ALL)
            .render(one.area, &mut one);
        assert_eq!(shapes(&one, &theme, CELL).lines.len(), 4);
    }

    #[test]
    fn text_is_runs_of_one_style_with_their_spaces() {
        let theme = Theme::parse("");
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 2));
        buf.set_string(2, 0, "Press  Enter", Style::default());
        buf.set_string(16, 0, "now", Style::default().fg(Color::Cyan));
        buf.set_string(0, 1, "a  ", Style::default());
        let runs = shapes(&buf, &theme, CELL).text;
        let texts: Vec<(u16, u16, &str)> =
            runs.iter().map(|r| (r.x, r.y, r.text.as_str())).collect();
        assert_eq!(
            texts,
            vec![(2, 0, "Press  Enter"), (16, 0, "now"), (0, 1, "a")]
        );
    }
}
