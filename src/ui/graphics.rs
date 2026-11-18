//! Charts as pictures, as whereami shows them: drawn in the Omarchy theme's colours, then
//! shown through the terminal's graphics protocol, drawn on the GPU in the window, or kept
//! as pixels for a web page and the screenshots. Terminals without images get the braille
//! charts instead.

#[cfg(not(target_family = "wasm"))]
mod terminal;

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use ab_glyph::FontVec;
use ratatui::Frame;
use ratatui::layout::Rect;

use super::raster::Canvas;
use super::theme::Theme;

/// Where charts go.
enum Mode {
    /// Nowhere: charts are drawn with text (tests, and terminals without graphics).
    Text,
    /// To the terminal, as images.
    #[cfg(not(target_family = "wasm"))]
    Terminal(terminal::Images),
    /// Drawn into pixels every frame and handed over (the screenshots).
    Offscreen {
        cell: (u16, u16),
        captured: Vec<(Rect, image::DynamicImage)>,
    },
    /// Drawn into pixels when they change, for a web page.
    Pixels(Retained<image::RgbaImage>),
    /// Recorded for the GPU when they change, with the labels' font for it, in the window.
    #[cfg(feature = "window")]
    Window(Retained<vello::Scene>, Option<vello::peniko::FontData>),
}

/// Charts kept from frame to frame and drawn again only when what they show changes.
struct Retained<T> {
    /// Cell size in pixels.
    cell: (u16, u16),
    /// Per chart slot: the hash of what it shows (content and size), and its drawing.
    charts: HashMap<String, (u64, Arc<T>)>,
    /// The charts of the frame being drawn: where, in cells, and their drawings.
    placed: Vec<(Rect, Arc<T>)>,
}

impl<T> Retained<T> {
    fn new(cell: (u16, u16)) -> Self {
        Self {
            cell,
            charts: HashMap::new(),
            placed: Vec::new(),
        }
    }

    /// Places chart `slot` in `area`, drawn by `draw` if `key` changed since it was last.
    fn place(&mut self, area: Rect, slot: &str, key: u64, draw: impl FnOnce() -> T) {
        if self.charts.get(slot).is_none_or(|(k, _)| *k != key) {
            self.charts.insert(slot.to_owned(), (key, Arc::new(draw())));
        }
        if let Some((_, chart)) = self.charts.get(slot) {
            self.placed.push((area, Arc::clone(chart)));
        }
    }
}

/// How charts are shown, and those already drawn.
pub struct Graphics {
    mode: Mode,
    theme: Theme,
    font: Option<FontVec>,
    /// Chart slots drawn since [`Self::begin_frame`].
    drawn: HashSet<String>,
}

impl std::fmt::Debug for Graphics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode = match &self.mode {
            Mode::Text => "text",
            #[cfg(not(target_family = "wasm"))]
            Mode::Terminal(_) => "terminal",
            Mode::Offscreen { .. } => "offscreen",
            Mode::Pixels(_) => "pixels",
            #[cfg(feature = "window")]
            Mode::Window(..) => "window",
        };
        f.debug_struct("Graphics")
            .field("mode", &mode)
            .field("charts", &self.drawn.len())
            .finish_non_exhaustive()
    }
}

impl Graphics {
    fn with(mode: Mode, theme: Theme, font: Option<FontVec>) -> Self {
        Self {
            mode,
            theme,
            font,
            drawn: HashSet::new(),
        }
    }

    /// No images: charts are drawn with text (tests, and terminals without graphics).
    #[must_use]
    pub fn text_only() -> Self {
        Self::with(Mode::Text, Theme::parse(""), None)
    }

    /// Charts drawn as images and kept (see [`Self::take_captured`]) rather than sent to a
    /// terminal: for screenshots. `cell` is a terminal cell's size in pixels.
    #[must_use]
    pub fn offscreen(theme: Theme, font: Option<FontVec>, cell: (u16, u16)) -> Self {
        let captured = Vec::new();
        Self::with(Mode::Offscreen { cell, captured }, theme, font)
    }

    /// Charts drawn into pixels when they change, for cells `cell` pixels large, with
    /// `font` for labels: for a web page (see [`Self::take_placed_pixels`]).
    #[must_use]
    pub fn pixels(theme: Theme, font: Option<FontVec>, cell: (u16, u16)) -> Self {
        Self::with(Mode::Pixels(Retained::new(cell)), theme, font)
    }

    /// Asks the terminal which graphics protocol and cell size it has. Call once, in raw
    /// mode, before the event loop reads stdin.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn detect() -> Self {
        match terminal::Images::detect() {
            Some(images) => Self::with(
                Mode::Terminal(images),
                Theme::load(),
                super::raster::load_font(),
            ),
            None => Self::with(Mode::Text, Theme::load(), None),
        }
    }

    /// Charts sent as `protocol` images for cells `cell` pixels large, without asking a
    /// terminal: for measuring how long drawing takes, as it would in Ghostty or kitty.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn with_protocol(
        protocol: ratatui_image::picker::ProtocolType,
        cell: (u16, u16),
        compress: bool,
    ) -> Self {
        let images = terminal::Images::with_protocol(protocol, cell, compress);
        Self::with(
            Mode::Terminal(images),
            Theme::parse(""),
            super::raster::load_font(),
        )
    }

    /// Charts drawn on the GPU in a window whose cells are `cell` pixels large, with the
    /// font in `font_bytes` for labels (see [`Self::take_placed`]).
    #[cfg(feature = "window")]
    #[must_use]
    pub fn window(theme: Theme, font_bytes: Option<Vec<u8>>, cell: (u16, u16)) -> Self {
        let data = font_bytes
            .as_ref()
            .map(|b| vello::peniko::FontData::new(vello::peniko::Blob::from(b.clone()), 0));
        let font = font_bytes.and_then(super::raster::font_from);
        Self::with(Mode::Window(Retained::new(cell), data), theme, font)
    }

    /// The cells are now `cell` pixels large (the text was zoomed, or the screen changed).
    pub fn set_cell_size(&mut self, cell: (u16, u16)) {
        match &mut self.mode {
            Mode::Pixels(r) => r.cell = cell,
            #[cfg(feature = "window")]
            Mode::Window(r, _) => r.cell = cell,
            Mode::Offscreen { cell: c, .. } => *c = cell,
            _ => {}
        }
    }

    /// The window's charts drawn since the last call: their areas in cells, and drawings.
    #[cfg(feature = "window")]
    pub fn take_placed(&mut self) -> Vec<(Rect, Arc<vello::Scene>)> {
        match &mut self.mode {
            Mode::Window(r, _) => std::mem::take(&mut r.placed),
            _ => Vec::new(),
        }
    }

    /// The web page's charts drawn since the last call: their areas in cells, and pixels
    /// (the same `Arc` from frame to frame while a chart is unchanged).
    pub fn take_placed_pixels(&mut self) -> Vec<(Rect, Arc<image::RgbaImage>)> {
        match &mut self.mode {
            Mode::Pixels(r) => std::mem::take(&mut r.placed),
            _ => Vec::new(),
        }
    }

    /// The charts drawn offscreen since the last call, with their areas in cells.
    pub fn take_captured(&mut self) -> Vec<(Rect, image::DynamicImage)> {
        match &mut self.mode {
            Mode::Offscreen { captured, .. } => std::mem::take(captured),
            _ => Vec::new(),
        }
    }

    /// True when charts are drawn as images.
    #[must_use]
    pub fn images(&self) -> bool {
        !matches!(self.mode, Mode::Text)
    }

    /// The theme the images use.
    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Re-reads the terminal's cell size, which changes with its font size (Ctrl+/Ctrl−).
    #[cfg(not(target_family = "wasm"))]
    pub fn refresh_cell_size(&mut self) {
        if let Mode::Terminal(images) = &mut self.mode {
            images.refresh_cell_size();
        }
    }

    /// Call before drawing each frame: charts not drawn in the last frame are forgotten.
    pub fn begin_frame(&mut self) {
        let drawn = std::mem::take(&mut self.drawn);
        match &mut self.mode {
            #[cfg(not(target_family = "wasm"))]
            Mode::Terminal(images) => images.retain(|slot| drawn.contains(slot)),
            Mode::Pixels(r) => {
                r.charts.retain(|slot, _| drawn.contains(slot));
                r.placed.clear();
            }
            #[cfg(feature = "window")]
            Mode::Window(r, _) => {
                r.charts.retain(|slot, _| drawn.contains(slot));
                r.placed.clear();
            }
            _ => {}
        }
    }

    /// Shows the chart `slot` in `area`. `content` identifies what it shows; the image is
    /// drawn again only when that or the area's size changes.
    #[cfg_attr(
        target_family = "wasm",
        expect(
            unused_variables,
            reason = "the frame only takes the terminal's images"
        )
    )]
    pub fn chart(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        slot: &str,
        content: u64,
        render: impl FnOnce(&mut Canvas, &Theme, Option<&FontVec>),
    ) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let (theme, font) = (&self.theme, self.font.as_ref());
        let pixels = |cell, render| draw_pixels(area, cell, theme, font, render);
        let key = |cell: (u16, u16)| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (content, area.width, area.height, cell).hash(&mut hasher);
            hasher.finish()
        };
        match &mut self.mode {
            Mode::Text => {}
            Mode::Offscreen { cell, captured } => captured.push((area, pixels(*cell, render))),
            Mode::Pixels(r) => {
                self.drawn.insert(slot.to_owned());
                let cell = r.cell;
                r.place(area, slot, key(cell), || pixels(cell, render).into_rgba8());
            }
            #[cfg(not(target_family = "wasm"))]
            Mode::Terminal(images) => {
                self.drawn.insert(slot.to_owned());
                let cell = images.cell();
                images.show(frame, area, slot, key(cell), || pixels(cell, render));
            }
            #[cfg(feature = "window")]
            Mode::Window(r, gpu_font) => {
                self.drawn.insert(slot.to_owned());
                let cell = r.cell;
                r.place(area, slot, key(cell), || {
                    let mut canvas = Canvas::scene(
                        u32::from(area.width) * u32::from(cell.0),
                        u32::from(area.height) * u32::from(cell.1),
                        theme.background,
                        f32::from(cell.0),
                        gpu_font.clone(),
                    );
                    render(&mut canvas, theme, font);
                    canvas.into_scene().unwrap_or_default()
                });
            }
        }
    }
}

/// The chart `render` draws for `area`, in pixels for cells `cell` pixels large.
fn draw_pixels(
    area: Rect,
    cell: (u16, u16),
    theme: &Theme,
    font: Option<&FontVec>,
    render: impl FnOnce(&mut Canvas, &Theme, Option<&FontVec>),
) -> image::DynamicImage {
    let mut canvas = Canvas::new(
        u32::from(area.width) * u32::from(cell.0),
        u32::from(area.height) * u32::from(cell.1),
        theme.background,
        f32::from(cell.0),
    );
    render(&mut canvas, theme, font);
    canvas.into_image()
}
