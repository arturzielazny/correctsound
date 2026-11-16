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
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
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
    /// Cell size in pixels when charts are kept as images instead of sent to a terminal.
    offscreen: Option<(u16, u16)>,
    /// Charts drawn offscreen since [`Self::take_captured`]: where, and the image.
    captured: Vec<(Rect, image::DynamicImage)>,
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
            offscreen: None,
            captured: Vec::new(),
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
            offscreen: Some(cell),
            captured: Vec::new(),
        }
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
        Self {
            picker,
            theme: Theme::load(),
            font,
            cache: HashMap::new(),
            drawn: HashSet::new(),
            offscreen: None,
            captured: Vec::new(),
        }
    }

    /// True when charts are drawn as images.
    #[must_use]
    pub fn images(&self) -> bool {
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
            let parts: Option<Vec<(Rect, Protocol)>> = tiles(area.width, area.height)
                .into_iter()
                .map(|t| {
                    let part = image.crop_imm(
                        u32::from(t.x) * u32::from(cell.width),
                        u32::from(t.y) * u32::from(cell.height),
                        u32::from(t.width) * u32::from(cell.width),
                        u32::from(t.height) * u32::from(cell.height),
                    );
                    let size = Size::new(t.width, t.height);
                    let p = picker.new_protocol(part, size, Resize::Fit(None)).ok()?;
                    Some((t, p))
                })
                .collect();
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

/// An area of `width` × `height` cells cut into tiles no larger than [`MAX_TILE_CELLS`],
/// as offsets from its corner; one tile when it fits.
fn tiles(width: u16, height: u16) -> Vec<Rect> {
    let cuts = |len: u16| {
        let n = len.div_ceil(MAX_TILE_CELLS).max(1);
        // Equal tiles, the remainder spread over the first ones.
        (0..n).scan(0, move |at, i| {
            let size = len / n + u16::from(i < len % n);
            let start = *at;
            *at += size;
            Some((start, size))
        })
    };
    cuts(height)
        .flat_map(|(y, h)| cuts(width).map(move |(x, w)| Rect::new(x, y, w, h)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_chart_is_cut_into_tiles_that_cover_it() {
        assert_eq!(tiles(120, 40), vec![Rect::new(0, 0, 120, 40)]);
        // An ultrawide terminal, 365 columns: two tiles side by side, no gap.
        let t = tiles(365, 60);
        assert_eq!(
            t,
            vec![Rect::new(0, 0, 183, 60), Rect::new(183, 0, 182, 60)]
        );
        let t = tiles(600, 300);
        assert!(
            t.iter()
                .all(|r| r.width <= MAX_TILE_CELLS && r.height <= MAX_TILE_CELLS)
        );
        let area: u32 = t
            .iter()
            .map(|r| u32::from(r.width) * u32::from(r.height))
            .sum();
        assert_eq!(area, 600 * 300);
    }
}
