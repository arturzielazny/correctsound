//! A pixel canvas for chart images: antialiased lines and text (tiny-skia, and ab_glyph with
//! the system's monospace font), as in whereami.

use std::sync::OnceLock;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use tiny_skia::{FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

use super::theme::Rgb;

/// Labels as a share of the terminal's text size.
const LABEL_SHARE: f32 = 0.9;

/// The monospace font's height per advance width (a terminal cell is one advance wide), set
/// when the font loads; JetBrains Mono's otherwise.
static HEIGHT_PER_ADVANCE: OnceLock<f32> = OnceLock::new();

/// Monospace fonts to try where fontconfig is missing (macOS, Windows), first found wins.
const FALLBACK_FONTS: [&str; 4] = [
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "C:\\Windows\\Fonts\\CascadiaMono.ttf",
    "C:\\Windows\\Fonts\\consola.ttf",
];

/// The system monospace font's file: `fc-match monospace` (the terminal's usual font),
/// else a platform's standard one.
fn font_path() -> Option<std::path::PathBuf> {
    let from_fontconfig = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", "monospace"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|p| std::path::PathBuf::from(p.trim()))
        .filter(|p| p.is_file());
    from_fontconfig.or_else(|| {
        FALLBACK_FONTS
            .iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.is_file())
    })
}

/// Loads the system monospace font for chart labels.
#[must_use]
pub fn load_font() -> Option<FontVec> {
    // Index 0 also reads the first font of a collection (.ttc).
    let font = FontVec::try_from_vec_and_index(std::fs::read(font_path()?).ok()?, 0).ok()?;
    let advance = font.h_advance_unscaled(font.glyph_id('0'));
    if advance > 0.0 {
        let _ = HEIGHT_PER_ADVANCE.set(font.height_unscaled() / advance);
    }
    Some(font)
}

fn paint(c: Rgb, alpha: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.0, c.1, c.2, (alpha.clamp(0.0, 1.0) * 255.0) as u8);
    p.anti_alias = true;
    p
}

/// An opaque RGBA canvas.
pub struct Canvas {
    pix: Pixmap,
    pub w: f32,
    pub h: f32,
    /// The terminal's cell width in pixels: labels follow the terminal's text size.
    pub cell_w: f32,
}

impl Canvas {
    /// A `w` × `h` canvas filled with `background`.
    ///
    /// # Panics
    ///
    /// Panics if the size is too large for a pixmap (over about 2^31 bytes).
    #[must_use]
    pub fn new(w: u32, h: u32, background: Rgb, cell_w: f32) -> Self {
        let mut pix = Pixmap::new(w.max(1), h.max(1)).expect("chart image size is reasonable");
        pix.fill(tiny_skia::Color::from_rgba8(
            background.0,
            background.1,
            background.2,
            255,
        ));
        Self {
            w: pix.width() as f32,
            h: pix.height() as f32,
            pix,
            cell_w,
        }
    }

    /// Label text size in pixels: [`LABEL_SHARE`] of the terminal's text.
    #[must_use]
    pub fn label(&self) -> f32 {
        self.cell_w * HEIGHT_PER_ADVANCE.get().copied().unwrap_or(2.2) * LABEL_SHARE
    }

    /// A straight line; `dash` is the dash length in pixels.
    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), style: Pen) {
        let mut pb = PathBuilder::new();
        pb.move_to(from.0, from.1);
        pb.line_to(to.0, to.1);
        if let Some(p) = pb.finish() {
            self.stroke(&p, style);
        }
    }

    /// Joined line segments; `None` breaks the line.
    pub fn polyline(&mut self, points: &[Option<(f32, f32)>], style: Pen) {
        let mut pb = PathBuilder::new();
        let mut open = false;
        for p in points {
            match p {
                Some((x, y)) if open => pb.line_to(*x, *y),
                Some((x, y)) => {
                    pb.move_to(*x, *y);
                    open = true;
                }
                None => open = false,
            }
        }
        if let Some(p) = pb.finish() {
            self.stroke(&p, style);
        }
    }

    /// A filled rectangle.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgb, alpha: f32) {
        if let Some(r) = tiny_skia::Rect::from_xywh(x, y, w.max(0.1), h.max(0.1)) {
            self.pix
                .fill_rect(r, &paint(c, alpha), Transform::identity(), None);
        }
    }

    /// A filled polygon.
    pub fn fill(&mut self, points: &[(f32, f32)], c: Rgb, alpha: f32) {
        let mut pb = PathBuilder::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        pb.close();
        if let Some(p) = pb.finish() {
            self.pix.fill_path(
                &p,
                &paint(c, alpha),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn stroke(&mut self, path: &tiny_skia::Path, pen: Pen) {
        let mut s = Stroke {
            width: pen.width.max(0.5),
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        if let Some(d) = pen.dash {
            s.line_cap = LineCap::Butt;
            s.dash = StrokeDash::new(vec![d, d * 0.7], 0.0);
        }
        self.pix.stroke_path(
            path,
            &paint(pen.color, pen.alpha),
            &s,
            Transform::identity(),
            None,
        );
    }

    /// Text with its top-left corner at (x, y), `px` pixels tall; returns its width.
    pub fn text(
        &mut self,
        font: Option<&FontVec>,
        s: &str,
        at: (f32, f32),
        px: f32,
        c: Rgb,
        alpha: f32,
    ) -> f32 {
        let Some(font) = font else { return 0.0 };
        let sf = font.as_scaled(PxScale::from(px));
        let (w, h) = (self.pix.width() as i32, self.pix.height() as i32);
        let data = self.pix.data_mut();
        let mut pen = at.0;
        let base = at.1 + sf.ascent();
        for ch in s.chars() {
            let mut glyph = sf.scaled_glyph(ch);
            let advance = sf.h_advance(glyph.id);
            glyph.position = ab_glyph::point(pen, base);
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    let x = bounds.min.x as i32 + gx as i32;
                    let y = bounds.min.y as i32 + gy as i32;
                    if x < 0 || y < 0 || x >= w || y >= h {
                        return;
                    }
                    let i = ((y * w + x) * 4) as usize;
                    let a = coverage * alpha;
                    for (k, v) in [c.0, c.1, c.2].into_iter().enumerate() {
                        data[i + k] = (f32::from(data[i + k]) * (1.0 - a) + f32::from(v) * a) as u8;
                    }
                });
            }
            pen += advance;
        }
        pen - at.0
    }

    /// Width of `s` at `px` pixels.
    #[must_use]
    pub fn text_width(font: Option<&FontVec>, s: &str, px: f32) -> f32 {
        let Some(font) = font else { return 0.0 };
        let sf = font.as_scaled(PxScale::from(px));
        s.chars().map(|ch| sf.h_advance(font.glyph_id(ch))).sum()
    }

    /// The finished image.
    ///
    /// # Panics
    ///
    /// Never in practice: the buffer always matches the pixmap's size.
    #[must_use]
    pub fn into_image(self) -> image::DynamicImage {
        let (w, h) = (self.pix.width(), self.pix.height());
        // Opaque (background filled), so premultiplied RGBA equals straight RGBA.
        let rgba = image::RgbaImage::from_raw(w, h, self.pix.take())
            .expect("pixmap buffer is w × h × 4 bytes");
        image::DynamicImage::ImageRgba8(rgba)
    }
}

/// How a line is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pen {
    pub color: Rgb,
    pub alpha: f32,
    pub width: f32,
    /// Dash length in pixels; `None` is solid.
    pub dash: Option<f32>,
}

impl Pen {
    #[must_use]
    pub fn solid(color: Rgb, alpha: f32, width: f32) -> Self {
        Self {
            color,
            alpha,
            width,
            dash: None,
        }
    }

    #[must_use]
    pub fn dashed(color: Rgb, alpha: f32, width: f32, dash: f32) -> Self {
        Self {
            color,
            alpha,
            width,
            dash: Some(dash),
        }
    }
}
