//! Chart images in the terminal, as whereami shows them: drawn into pixels in the Omarchy
//! theme's colours and shown through the terminal's graphics protocol (kitty in Ghostty and
//! kitty, sixel in foot). Terminals without one get the braille charts instead.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::time::{Duration, Instant};

use ab_glyph::FontVec;
use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Capability, Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::protocol::kitty::Kitty;
use ratatui_image::{Image, Resize};

use super::raster::{self, Canvas};
use super::theme::Theme;

/// How long the terminal may take to answer the graphics query (Ghostty, kitty and foot
/// answer within milliseconds; ratatui-image's default would delay startup by 2 s elsewhere).
const QUERY_TIMEOUT: Duration = Duration::from_millis(500);
/// Widest and tallest image sent as one, in cells. The kitty protocol's placeholders (how
/// Ghostty and kitty show images in a text UI) number columns and rows up to 297; a chart
/// wider than that, on an ultrawide monitor, is sent as tiles side by side.
const MAX_TILE_CELLS: u16 = 256;
/// Tallest tile, in rows: a chart is also cut into bands this tall, so its tiles are
/// encoded and compressed on several threads at once.
const BAND_ROWS: u16 = 16;

/// Image support and the charts already sent to the terminal.
pub struct Graphics {
    picker: Option<Picker>,
    theme: Theme,
    font: Option<FontVec>,
    /// Per chart slot: the hash of what it shows (content and size) and its image, in
    /// tiles (offset in cells from the area's corner, and the tile).
    cache: HashMap<String, (u64, Vec<(Rect, Protocol)>)>,
    /// Chart slots drawn since [`Self::begin_frame`].
    drawn: HashSet<String>,
    /// True when the terminal takes kitty images compressed: a chart is then a few hundred
    /// kilobytes instead of tens of megabytes of pixels on a large screen. Kept here, as the
    /// picker is replaced when the font size changes.
    compress: bool,
    /// The kitty image id the next image gets.
    next_id: u32,
    /// Cell size in pixels when charts are kept as images instead of sent to a terminal.
    offscreen: Option<(u16, u16)>,
    /// Charts drawn offscreen since [`Self::take_captured`]: where, and the image.
    captured: Vec<(Rect, image::DynamicImage)>,
    /// Charts drawn on the GPU, in the window.
    #[cfg(feature = "window")]
    window: Option<WindowCharts>,
}

/// The window's charts: recorded for the GPU, each again only when what it shows changes.
#[cfg(feature = "window")]
struct WindowCharts {
    /// Cell size in pixels.
    cell: (u16, u16),
    /// The labels' font, for the GPU.
    font: Option<vello::peniko::FontData>,
    /// Per chart slot: the hash of what it shows (content and size), and its drawing.
    scenes: HashMap<String, (u64, std::sync::Arc<vello::Scene>)>,
    /// The charts of the frame being drawn: where, and the drawing.
    placed: Vec<(Rect, std::sync::Arc<vello::Scene>)>,
}

impl std::fmt::Debug for Graphics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graphics")
            .field("protocol", &self.picker.as_ref().map(Picker::protocol_type))
            .field("charts", &self.cache.len())
            .finish_non_exhaustive()
    }
}

impl Graphics {
    /// No images: charts are drawn with text (tests, and terminals without graphics).
    #[must_use]
    pub fn text_only() -> Self {
        Self {
            picker: None,
            theme: Theme::parse(""),
            font: None,
            cache: HashMap::new(),
            drawn: HashSet::new(),
            compress: false,
            next_id: first_image_id(),
            offscreen: None,
            captured: Vec::new(),
            #[cfg(feature = "window")]
            window: None,
        }
    }

    /// Charts drawn as images and kept (see [`Self::take_captured`]) rather than sent to a
    /// terminal: for screenshots. `cell` is a terminal cell's size in pixels.
    #[must_use]
    pub fn offscreen(theme: Theme, font: Option<FontVec>, cell: (u16, u16)) -> Self {
        Self {
            picker: None,
            theme,
            font,
            cache: HashMap::new(),
            drawn: HashSet::new(),
            compress: false,
            next_id: first_image_id(),
            offscreen: Some(cell),
            captured: Vec::new(),
            #[cfg(feature = "window")]
            window: None,
        }
    }

    /// Charts sent as `protocol` images for cells `cell` pixels large, without asking a
    /// terminal: for measuring how long drawing takes, as it would in Ghostty or kitty.
    #[must_use]
    pub fn with_protocol(protocol: ProtocolType, cell: (u16, u16), compress: bool) -> Self {
        #[expect(
            deprecated,
            reason = "the cell size is given, not queried from a terminal"
        )]
        let mut picker = Picker::from_fontsize(cell.into());
        picker.set_protocol_type(protocol);
        Self {
            picker: Some(picker),
            theme: Theme::parse(""),
            font: raster::load_font(),
            cache: HashMap::new(),
            drawn: HashSet::new(),
            compress,
            next_id: first_image_id(),
            offscreen: None,
            captured: Vec::new(),
            #[cfg(feature = "window")]
            window: None,
        }
    }

    /// Charts drawn on the GPU in a window whose cells are `cell` pixels large, with the
    /// font in `font_bytes` for labels (see [`Self::take_placed`]).
    #[cfg(feature = "window")]
    #[must_use]
    pub fn window(theme: Theme, font_bytes: Option<Vec<u8>>, cell: (u16, u16)) -> Self {
        let data = font_bytes
            .as_ref()
            .map(|b| vello::peniko::FontData::new(vello::peniko::Blob::from(b.clone()), 0));
        Self {
            picker: None,
            theme,
            font: font_bytes.and_then(raster::font_from),
            cache: HashMap::new(),
            drawn: HashSet::new(),
            compress: false,
            next_id: first_image_id(),
            offscreen: None,
            captured: Vec::new(),
            window: Some(WindowCharts {
                cell,
                font: data,
                scenes: HashMap::new(),
                placed: Vec::new(),
            }),
        }
    }

    /// The window's cell size changed (its text was zoomed or it moved to another screen).
    #[cfg(feature = "window")]
    pub fn set_cell_size(&mut self, cell: (u16, u16)) {
        if let Some(w) = &mut self.window {
            w.cell = cell;
        }
    }

    /// The window's charts drawn since the last call: their areas in cells, and drawings.
    #[cfg(feature = "window")]
    pub fn take_placed(&mut self) -> Vec<(Rect, std::sync::Arc<vello::Scene>)> {
        self.window
            .as_mut()
            .map(|w| std::mem::take(&mut w.placed))
            .unwrap_or_default()
    }

    /// The charts drawn offscreen since the last call, with their areas in cells.
    pub fn take_captured(&mut self) -> Vec<(Rect, image::DynamicImage)> {
        std::mem::take(&mut self.captured)
    }

    /// Asks the terminal which graphics protocol and cell size it has. Call once, in raw
    /// mode, before the event loop reads stdin.
    #[must_use]
    pub fn detect() -> Self {
        let asked = Instant::now();
        let options = QueryStdioOptions {
            timeout: QUERY_TIMEOUT,
            kitty_compression: true,
            ..QueryStdioOptions::default()
        };
        let picker = Picker::from_query_stdio_with_options(options).ok();
        if asked.elapsed() >= QUERY_TIMEOUT {
            // The terminal did not answer in time (tmux may pass the query to a terminal
            // that never answers). The picker's reader thread is still waiting on stdin and
            // would eat the first key; it stops at a Device Status Report, so ask for one.
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[5n").and_then(|()| out.flush());
            std::thread::sleep(Duration::from_millis(100));
        }
        // Half-block "images" are coarser than the braille charts.
        let picker = picker.filter(|p| p.protocol_type() != ProtocolType::Halfblocks);
        let font = picker.as_ref().and_then(|_| raster::load_font());
        let compress = picker
            .as_ref()
            .is_some_and(|p| p.capabilities().contains(&Capability::KittyCompression));
        Self {
            picker,
            theme: Theme::load(),
            font,
            cache: HashMap::new(),
            drawn: HashSet::new(),
            compress,
            next_id: first_image_id(),
            offscreen: None,
            captured: Vec::new(),
            #[cfg(feature = "window")]
            window: None,
        }
    }

    /// True when charts are drawn as images.
    #[must_use]
    pub fn images(&self) -> bool {
        #[cfg(feature = "window")]
        if self.window.is_some() {
            return true;
        }
        self.picker.is_some() || self.offscreen.is_some()
    }

    /// The theme the images use.
    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Re-reads the terminal's cell size, which changes with its font size (Ctrl+/Ctrl−),
    /// from the window size it reports (no query: the event loop owns stdin).
    pub fn refresh_cell_size(&mut self) {
        let Some(picker) = &self.picker else { return };
        let Ok(ws) = crossterm::terminal::window_size() else {
            return;
        };
        if ws.width == 0 || ws.height == 0 || ws.columns == 0 || ws.rows == 0 {
            return;
        }
        let size = (ws.width / ws.columns, ws.height / ws.rows);
        let current = picker.font_size();
        if (current.width, current.height) == size {
            return;
        }
        #[expect(
            deprecated,
            reason = "the cell size comes from the window size, not a query"
        )]
        let mut fresh = Picker::from_fontsize(size.into());
        fresh.set_protocol_type(picker.protocol_type());
        self.picker = Some(fresh);
    }

    /// Call before drawing each frame. A chart that was off screen for a frame is sent to
    /// the terminal again when it comes back: the terminal keeps only so many images (Ghostty
    /// drops the oldest past its storage limit, which large charts on a 5K monitor reach
    /// within seconds of placement updates), so the one sent earlier may be gone.
    pub fn begin_frame(&mut self) {
        let drawn = std::mem::take(&mut self.drawn);
        self.cache.retain(|slot, _| drawn.contains(slot));
        #[cfg(feature = "window")]
        if let Some(w) = &mut self.window {
            w.scenes.retain(|slot, _| drawn.contains(slot));
            w.placed.clear();
        }
    }

    /// Shows the chart `slot` in `area`. `content` identifies what it shows; the image is
    /// drawn again only when that or the area's size changes.
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
        #[cfg(feature = "window")]
        if let Some(w) = &mut self.window {
            self.drawn.insert(slot.to_owned());
            let (cw, ch) = w.cell;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (content, area.width, area.height, cw, ch).hash(&mut hasher);
            let key = hasher.finish();
            if w.scenes.get(slot).is_none_or(|(k, _)| *k != key) {
                let mut canvas = Canvas::scene(
                    u32::from(area.width) * u32::from(cw),
                    u32::from(area.height) * u32::from(ch),
                    self.theme.background,
                    f32::from(cw),
                    w.font.clone(),
                );
                render(&mut canvas, &self.theme, self.font.as_ref());
                let scene = canvas.into_scene().unwrap_or_default();
                w.scenes
                    .insert(slot.to_owned(), (key, std::sync::Arc::new(scene)));
            }
            if let Some((_, scene)) = w.scenes.get(slot) {
                w.placed.push((area, std::sync::Arc::clone(scene)));
            }
            return;
        }
        if let Some((cw, ch)) = self.offscreen {
            let mut canvas = Canvas::new(
                u32::from(area.width) * u32::from(cw),
                u32::from(area.height) * u32::from(ch),
                self.theme.background,
                f32::from(cw),
            );
            render(&mut canvas, &self.theme, self.font.as_ref());
            self.captured.push((area, canvas.into_image()));
            return;
        }
        let Some(picker) = &self.picker else { return };
        self.drawn.insert(slot.to_owned());
        let cell = picker.font_size();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (content, area.width, area.height, cell.width, cell.height).hash(&mut hasher);
        let key = hasher.finish();
        if self.cache.get(slot).is_none_or(|(k, _)| *k != key) {
            let mut canvas = Canvas::new(
                u32::from(area.width) * u32::from(cell.width),
                u32::from(area.height) * u32::from(cell.height),
                self.theme.background,
                f32::from(cell.width),
            );
            render(&mut canvas, &self.theme, self.font.as_ref());
            let image = canvas.into_image();
            let tiles = tiles(area.width, area.height);
            let first_id = self.next_id;
            self.next_id = self.next_id.wrapping_add(tiles.len() as u32).max(1);
            let (compress, tmux) = (self.compress, picker.tmux_detected());
            // Each tile is encoded (and compressed) on its own thread.
            let parts: Option<Vec<(Rect, Protocol)>> = std::thread::scope(|scope| {
                let handles: Vec<_> = tiles
                    .into_iter()
                    .enumerate()
                    .map(|(k, t)| {
                        let part = image.crop_imm(
                            u32::from(t.x) * u32::from(cell.width),
                            u32::from(t.y) * u32::from(cell.height),
                            u32::from(t.width) * u32::from(cell.width),
                            u32::from(t.height) * u32::from(cell.height),
                        );
                        let id = first_id.wrapping_add(k as u32).max(1);
                        scope.spawn(move || {
                            let size = Size::new(t.width, t.height);
                            let protocol = match picker.protocol_type() {
                                // Built here rather than by the picker, which only compresses
                                // when it asked the terminal itself.
                                ProtocolType::Kitty => {
                                    Kitty::new(part, size, id, tmux, compress).map(Protocol::Kitty)
                                }
                                _ => picker.new_protocol(part, size, Resize::Fit(None)),
                            };
                            protocol.ok().map(|p| (t, p))
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("image threads do not panic"))
                    .collect()
            });
            match parts {
                Some(t) => {
                    self.cache.insert(slot.to_owned(), (key, t));
                }
                None => {
                    self.cache.remove(slot);
                }
            }
        }
        if let Some((_, parts)) = self.cache.get(slot) {
            for (t, p) in parts {
                let at = Rect::new(area.x + t.x, area.y + t.y, t.width, t.height);
                frame.render_widget(Image::new(p), at);
            }
        }
    }
}

/// A kitty image id to start from: different on every run, so images left in the terminal
/// by an earlier run are not mistaken for this run's.
fn first_image_id() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    (nanos ^ std::process::id().rotate_left(16)).max(1)
}

/// An area of `width` × `height` cells cut into tiles no wider than [`MAX_TILE_CELLS`] and
/// no taller than [`BAND_ROWS`], as offsets from its corner.
fn tiles(width: u16, height: u16) -> Vec<Rect> {
    let cuts = |len: u16, most: u16| {
        let n = len.div_ceil(most).max(1);
        // Equal tiles, the remainder spread over the first ones.
        (0..n).scan(0, move |at, i| {
            let size = len / n + u16::from(i < len % n);
            let start = *at;
            *at += size;
            Some((start, size))
        })
    };
    cuts(height, BAND_ROWS)
        .flat_map(|(y, h)| cuts(width, MAX_TILE_CELLS).map(move |(x, w)| Rect::new(x, y, w, h)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_chart_is_cut_into_tiles_that_cover_it() {
        assert_eq!(tiles(120, 12), vec![Rect::new(0, 0, 120, 12)]);
        // Taller: bands of equal height, a row more in the first ones.
        assert_eq!(
            tiles(120, 40),
            vec![
                Rect::new(0, 0, 120, 14),
                Rect::new(0, 14, 120, 13),
                Rect::new(0, 27, 120, 13)
            ]
        );
        // An ultrawide terminal, 365 columns: two tiles side by side, no gap.
        let t = tiles(365, 16);
        assert_eq!(
            t,
            vec![Rect::new(0, 0, 183, 16), Rect::new(183, 0, 182, 16)]
        );
        let t = tiles(600, 300);
        assert!(
            t.iter()
                .all(|r| r.width <= MAX_TILE_CELLS && r.height <= BAND_ROWS)
        );
        let area: u32 = t
            .iter()
            .map(|r| u32::from(r.width) * u32::from(r.height))
            .sum();
        assert_eq!(area, 600 * 300);
    }
}
