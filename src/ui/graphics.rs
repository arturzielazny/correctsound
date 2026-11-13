//! Chart images in the terminal, as whereami shows them: drawn into pixels in the Omarchy
//! theme's colours and shown through the terminal's graphics protocol (kitty in Ghostty and
//! kitty, sixel in foot). Terminals without one get the braille charts instead.

use std::collections::HashMap;
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

/// Image support and the charts already sent to the terminal.
pub struct Graphics {
    picker: Option<Picker>,
    theme: Theme,
    font: Option<FontVec>,
    /// Per chart slot: the hash of what it shows (content and size) and its image.
    cache: HashMap<String, (u64, Protocol)>,
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
            let size = Size::new(area.width, area.height);
            match picker.new_protocol(canvas.into_image(), size, Resize::Fit(None)) {
                Ok(p) => {
                    self.cache.insert(slot.to_owned(), (key, p));
                }
                Err(_) => {
                    self.cache.remove(slot);
                }
            }
        }
        if let Some((_, p)) = self.cache.get(slot) {
            frame.render_widget(Image::new(p), area);
        }
    }
}
