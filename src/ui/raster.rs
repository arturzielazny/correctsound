//! A canvas for charts: antialiased lines, areas and text with the system's monospace font,
//! as in whereami. It draws into pixels (tiny-skia and ab_glyph) for the terminal and the
//! screenshots, or, in the window, records the same drawing for the GPU (a vello scene).

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

/// The file of the font fontconfig picks for `pattern` (such as `monospace:bold`, or
/// `monospace:charset=2714` for one with ✔), where fontconfig is installed.
#[must_use]
pub fn font_file_matching(pattern: &str) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    let path = std::path::PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    path.is_file().then_some(path)
}

/// That font file's contents.
#[must_use]
pub fn font_bytes_matching(pattern: &str) -> Option<Vec<u8>> {
    std::fs::read(font_file_matching(pattern)?).ok()
}

/// Loads the system monospace font for chart labels.
#[must_use]
pub fn load_font() -> Option<FontVec> {
    font_from(load_font_bytes()?)
}

/// The system monospace font file's contents.
#[must_use]
pub fn load_font_bytes() -> Option<Vec<u8>> {
    std::fs::read(font_path()?).ok()
}

/// The font in `bytes` (the first of a collection), for chart labels.
#[must_use]
pub fn font_from(bytes: Vec<u8>) -> Option<FontVec> {
    // Index 0 also reads the first font of a collection (.ttc).
    let font = FontVec::try_from_vec_and_index(bytes, 0).ok()?;
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

/// What a canvas draws into.
enum Target {
    Pixels(Pixmap),
    /// The drawing recorded for the GPU, with the labels' font.
    #[cfg(feature = "window")]
    Scene {
        scene: Box<vello::Scene>,
        font: Option<vello::peniko::FontData>,
    },
}

/// An opaque canvas.
pub struct Canvas {
    target: Target,
    pub w: f32,
    pub h: f32,
    /// The terminal's cell width in pixels: labels follow the terminal's text size.
    pub cell_w: f32,
}

impl Canvas {
    /// A `w` × `h` canvas of pixels filled with `background`.
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
            target: Target::Pixels(pix),
            cell_w,
        }
    }

    /// A `w` × `h` canvas recorded for the GPU, filled with `background` and clipped to its
    /// size. `font` is the labels' font (the one passed to [`Self::text`]) for the GPU.
    #[cfg(feature = "window")]
    #[must_use]
    pub fn scene(
        w: u32,
        h: u32,
        background: Rgb,
        cell_w: f32,
        font: Option<vello::peniko::FontData>,
    ) -> Self {
        use vello::kurbo::{Affine, Rect};
        let mut scene = vello::Scene::new();
        let bounds = Rect::new(0.0, 0.0, f64::from(w.max(1)), f64::from(h.max(1)));
        scene.push_clip_layer(vello::peniko::Fill::NonZero, Affine::IDENTITY, &bounds);
        scene.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            gpu::color(background, 1.0),
            None,
            &bounds,
        );
        Self {
            target: Target::Scene {
                scene: Box::new(scene),
                font,
            },
            w: w.max(1) as f32,
            h: h.max(1) as f32,
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
        self.polyline(&[Some(from), Some(to)], style);
    }

    /// Joined line segments; `None` breaks the line.
    pub fn polyline(&mut self, points: &[Option<(f32, f32)>], style: Pen) {
        match &mut self.target {
            Target::Pixels(pix) => {
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
                    stroke(pix, &p, style);
                }
            }
            #[cfg(feature = "window")]
            Target::Scene { scene, .. } => gpu::polyline(scene, points, style),
        }
    }

    /// A filled rectangle.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgb, alpha: f32) {
        match &mut self.target {
            Target::Pixels(pix) => {
                if let Some(r) = tiny_skia::Rect::from_xywh(x, y, w.max(0.1), h.max(0.1)) {
                    pix.fill_rect(r, &paint(c, alpha), Transform::identity(), None);
                }
            }
            #[cfg(feature = "window")]
            Target::Scene { scene, .. } => {
                gpu::rect(scene, (x, y, w.max(0.1), h.max(0.1)), c, alpha)
            }
        }
    }

    /// A filled polygon.
    pub fn fill(&mut self, points: &[(f32, f32)], c: Rgb, alpha: f32) {
        match &mut self.target {
            Target::Pixels(pix) => {
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
                    pix.fill_path(
                        &p,
                        &paint(c, alpha),
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }
            #[cfg(feature = "window")]
            Target::Scene { scene, .. } => gpu::fill(scene, points, c, alpha),
        }
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
        match &mut self.target {
            Target::Pixels(pix) => text_pixels(pix, font, s, at, px, c, alpha),
            #[cfg(feature = "window")]
            Target::Scene { scene, font: data } => match data {
                Some(data) => gpu::text(scene, (font, data), s, at, px, c, alpha),
                None => Self::text_width(Some(font), s, px),
            },
        }
    }

    /// Width of `s` at `px` pixels.
    #[must_use]
    pub fn text_width(font: Option<&FontVec>, s: &str, px: f32) -> f32 {
        let Some(font) = font else { return 0.0 };
        let sf = font.as_scaled(PxScale::from(px));
        s.chars().map(|ch| sf.h_advance(font.glyph_id(ch))).sum()
    }

    /// The finished image (an empty one for a canvas recorded for the GPU).
    ///
    /// # Panics
    ///
    /// Never in practice: the buffer always matches the pixmap's size.
    #[must_use]
    pub fn into_image(self) -> image::DynamicImage {
        #[cfg(feature = "window")]
        let Target::Pixels(pix) = self.target else {
            return image::DynamicImage::new_rgba8(1, 1);
        };
        #[cfg(not(feature = "window"))]
        let Target::Pixels(pix) = self.target;
        let (w, h) = (pix.width(), pix.height());
        // Opaque (background filled), so premultiplied RGBA equals straight RGBA.
        let rgba =
            image::RgbaImage::from_raw(w, h, pix.take()).expect("pixmap buffer is w × h × 4 bytes");
        image::DynamicImage::ImageRgba8(rgba)
    }

    /// The finished drawing for the GPU; `None` for a canvas of pixels.
    #[cfg(feature = "window")]
    #[must_use]
    pub fn into_scene(self) -> Option<vello::Scene> {
        match self.target {
            Target::Pixels(_) => None,
            Target::Scene { mut scene, .. } => {
                scene.pop_layer();
                Some(*scene)
            }
        }
    }
}

fn stroke(pix: &mut Pixmap, path: &tiny_skia::Path, pen: Pen) {
    let mut s = Stroke {
        width: pen.width.max(0.5),
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    if let Some(d) = pen.dash {
        s.line_cap = LineCap::Butt;
        s.dash = StrokeDash::new(vec![d, d * 0.7], 0.0);
    }
    pix.stroke_path(
        path,
        &paint(pen.color, pen.alpha),
        &s,
        Transform::identity(),
        None,
    );
}

/// Draws `s` into `pix` with its top-left corner at `at`; returns its width.
fn text_pixels(
    pix: &mut Pixmap,
    font: &FontVec,
    s: &str,
    at: (f32, f32),
    px: f32,
    c: Rgb,
    alpha: f32,
) -> f32 {
    let sf = font.as_scaled(PxScale::from(px));
    let (w, h) = (pix.width() as i32, pix.height() as i32);
    let data = pix.data_mut();
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

/// The same drawing, recorded for the GPU.
#[cfg(feature = "window")]
pub mod gpu {
    use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
    use vello::Scene;
    use vello::kurbo::{Affine, BezPath, Cap, Rect, Stroke};
    use vello::peniko::{Color, Fill, FontData};

    use super::{Pen, Rgb};

    /// `c` at opacity `alpha` (0–1).
    #[must_use]
    pub fn color(c: Rgb, alpha: f32) -> Color {
        Color::from_rgba8(c.0, c.1, c.2, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
    }

    pub(super) fn polyline(scene: &mut Scene, points: &[Option<(f32, f32)>], pen: Pen) {
        let mut path = BezPath::new();
        let mut open = false;
        for p in points {
            match p {
                Some((x, y)) if open => path.line_to((f64::from(*x), f64::from(*y))),
                Some((x, y)) => {
                    path.move_to((f64::from(*x), f64::from(*y)));
                    open = true;
                }
                None => open = false,
            }
        }
        stroke(scene, &path, pen);
    }

    /// Strokes `path` as tiny-skia would with the same pen.
    pub fn stroke(scene: &mut Scene, path: &BezPath, pen: Pen) {
        if path.elements().is_empty() {
            return;
        }
        let mut style = Stroke::new(f64::from(pen.width.max(0.5))).with_caps(Cap::Round);
        if let Some(d) = pen.dash {
            let d = f64::from(d);
            style = style.with_caps(Cap::Butt).with_dashes(0.0, [d, d * 0.7]);
        }
        scene.stroke(
            &style,
            Affine::IDENTITY,
            color(pen.color, pen.alpha),
            None,
            path,
        );
    }

    pub(super) fn rect(scene: &mut Scene, (x, y, w, h): (f32, f32, f32, f32), c: Rgb, alpha: f32) {
        let r = Rect::new(
            f64::from(x),
            f64::from(y),
            f64::from(x + w),
            f64::from(y + h),
        );
        scene.fill(Fill::NonZero, Affine::IDENTITY, color(c, alpha), None, &r);
    }

    pub(super) fn fill(scene: &mut Scene, points: &[(f32, f32)], c: Rgb, alpha: f32) {
        let mut path = BezPath::new();
        for (i, &(x, y)) in points.iter().enumerate() {
            let p = (f64::from(x), f64::from(y));
            if i == 0 {
                path.move_to(p);
            } else {
                path.line_to(p);
            }
        }
        if points.is_empty() {
            return;
        }
        path.close_path();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color(c, alpha),
            None,
            &path,
        );
    }

    /// The em size vello takes for ab_glyph's `px` (the font's height, ascent to descent).
    #[must_use]
    pub fn em(font: &FontVec, px: f32) -> f32 {
        let height = font.height_unscaled();
        match font.units_per_em() {
            Some(upem) if height > 0.0 => px * upem / height,
            _ => px,
        }
    }

    pub(super) fn text(
        scene: &mut Scene,
        (font, data): (&FontVec, &FontData),
        s: &str,
        at: (f32, f32),
        px: f32,
        c: Rgb,
        alpha: f32,
    ) -> f32 {
        let sf = font.as_scaled(PxScale::from(px));
        let base = at.1 + sf.ascent();
        let mut pen = at.0;
        let glyphs: Vec<vello::Glyph> = s
            .chars()
            .map(|ch| {
                let id = font.glyph_id(ch);
                let g = vello::Glyph {
                    id: u32::from(id.0),
                    x: pen,
                    y: base,
                };
                pen += sf.h_advance(id);
                g
            })
            .collect();
        scene
            .draw_glyphs(data)
            .font_size(em(font, px))
            .brush(color(c, alpha))
            .draw(Fill::NonZero, glyphs.into_iter());
        pen - at.0
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
