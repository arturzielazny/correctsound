//! The screen's text, as a terminal shows it, drawn on the GPU: the grid's shapes
//! ([`crate::ui::cells`]) become one vello scene, the text in runs of one font and colour.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill as FillRule;

use super::fonts::{Fonts, Metrics};
use crate::ui::cells::{self, CellSize, Fill};
use crate::ui::raster::gpu::{color, em};
use crate::ui::theme::{Rgb, Theme};

fn draw(scene: &mut Scene, fills: &[Fill]) {
    for f in fills {
        let a = f.area;
        scene.fill(
            FillRule::NonZero,
            Affine::IDENTITY,
            color(f.color, f.alpha),
            None,
            &Rect::new(a.x0, a.y0, a.x1, a.y1),
        );
    }
}

/// The grid `buf` as a scene, cells `m` large, in `theme`'s colours. The background itself
/// is left to the renderer.
#[must_use]
pub fn scene(buf: &Buffer, theme: &Theme, fonts: &mut Fonts, m: &Metrics) -> Scene {
    let shapes = cells::shapes(
        buf,
        theme,
        CellSize {
            w: f64::from(m.cell_w),
            h: f64::from(m.cell_h),
            line: f64::from(m.line),
            baseline: f64::from(m.baseline),
        },
    );
    let mut scene = Scene::new();
    draw(&mut scene, &shapes.backgrounds);
    draw(&mut scene, &shapes.lines);
    draw(&mut scene, &shapes.blocks);
    // Glyphs in runs of one font and colour.
    let mut runs: Vec<((usize, Rgb), Vec<vello::Glyph>)> = Vec::new();
    let mut run_of: HashMap<(usize, Rgb), usize> = HashMap::new();
    for run in &shapes.text {
        let y = f32::from(run.y) * m.cell_h + m.baseline;
        for (i, c) in run.text.chars().enumerate() {
            if c == ' ' {
                continue;
            }
            let Some((face, id)) = fonts.glyph(c, run.bold) else {
                continue;
            };
            let key = (face, run.color);
            let k = *run_of.entry(key).or_insert_with(|| {
                runs.push((key, Vec::new()));
                runs.len() - 1
            });
            runs[k].1.push(vello::Glyph {
                id: u32::from(id.0),
                x: (usize::from(run.x) + i) as f32 * m.cell_w,
                y,
            });
        }
    }
    for ((face, fg), glyphs) in runs {
        let face = fonts.face(face);
        scene
            .draw_glyphs(&face.data)
            .font_size(em(&face.glyphs, m.px))
            .brush(color(fg, 1.0))
            .draw(FillRule::NonZero, glyphs.into_iter());
    }
    draw(&mut scene, &shapes.underlines);
    scene
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect as Area;
    use ratatui::widgets::{Block, Borders, Widget};

    use super::*;

    #[test]
    fn a_panel_is_four_rectangles() {
        let Some(mut fonts) = Fonts::load() else {
            eprintln!("skipped: no monospace font");
            return;
        };
        let m = fonts.metrics(9.0, 2.0);
        let mut one = Buffer::empty(Area::new(0, 0, 10, 5));
        Block::default()
            .borders(Borders::ALL)
            .render(one.area, &mut one);
        let paths = scene(&one, &Theme::parse(""), &mut fonts, &m)
            .encoding()
            .n_paths;
        assert_eq!(paths, 4);
    }
}
