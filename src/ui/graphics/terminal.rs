//! Charts sent to the terminal as images, through its graphics protocol (kitty in Ghostty
//! and kitty, sixel in foot). Only the installed program has a terminal.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Capability, Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::protocol::kitty::Kitty;
use ratatui_image::{Image, Resize};

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

/// The terminal's image support and the charts already sent to it.
pub struct Images {
    picker: Picker,
    /// Per chart slot: the hash of what it shows (content and size) and its image, in
    /// tiles (offset in cells from the area's corner, and the tile).
    cache: HashMap<String, (u64, Vec<(Rect, Protocol)>)>,
    /// True when the terminal takes kitty images compressed: a chart is then a few hundred
    /// kilobytes instead of tens of megabytes of pixels on a large screen. Kept here, as the
    /// picker is replaced when the font size changes.
    compress: bool,
    /// The kitty image id the next image gets.
    next_id: u32,
}

impl Images {
    /// Asks the terminal which graphics protocol and cell size it has; `None` when it shows
    /// no images worth having. Call once, in raw mode, before the event loop reads stdin.
    #[must_use]
    pub fn detect() -> Option<Self> {
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
        let picker = picker.filter(|p| p.protocol_type() != ProtocolType::Halfblocks)?;
        let compress = picker
            .capabilities()
            .contains(&Capability::KittyCompression);
        Some(Self::new(picker, compress))
    }

    /// Images sent as `protocol` for cells `cell` pixels large, without asking a terminal.
    #[must_use]
    pub fn with_protocol(protocol: ProtocolType, cell: (u16, u16), compress: bool) -> Self {
        #[expect(
            deprecated,
            reason = "the cell size is given, not queried from a terminal"
        )]
        let mut picker = Picker::from_fontsize(cell.into());
        picker.set_protocol_type(protocol);
        Self::new(picker, compress)
    }

    fn new(picker: Picker, compress: bool) -> Self {
        Self {
            picker,
            cache: HashMap::new(),
            compress,
            next_id: first_image_id(),
        }
    }

    /// A cell's size in pixels.
    #[must_use]
    pub fn cell(&self) -> (u16, u16) {
        let size = self.picker.font_size();
        (size.width, size.height)
    }

    /// Re-reads the terminal's cell size, which changes with its font size (Ctrl+/Ctrl−),
    /// from the window size it reports (no query: the event loop owns stdin).
    pub fn refresh_cell_size(&mut self) {
        let Ok(ws) = crossterm::terminal::window_size() else {
            return;
        };
        if ws.width == 0 || ws.height == 0 || ws.columns == 0 || ws.rows == 0 {
            return;
        }
        let size = (ws.width / ws.columns, ws.height / ws.rows);
        if self.cell() == size {
            return;
        }
        #[expect(
            deprecated,
            reason = "the cell size comes from the window size, not a query"
        )]
        let mut fresh = Picker::from_fontsize(size.into());
        fresh.set_protocol_type(self.picker.protocol_type());
        self.picker = fresh;
    }

    /// Forgets the charts not in `keep`: one that was off screen for a frame is sent to the
    /// terminal again when it comes back, as the terminal keeps only so many images (Ghostty
    /// drops the oldest past its storage limit, which large charts on a 5K monitor reach
    /// within seconds of placement updates), so the one sent earlier may be gone.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.cache.retain(|slot, _| keep(slot));
    }

    /// Shows the chart `slot` in `area`: the image `draw` makes, sent again only when `key`
    /// (what it shows and its size) changes.
    pub fn show(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        slot: &str,
        key: u64,
        draw: impl FnOnce() -> image::DynamicImage,
    ) {
        if self.cache.get(slot).is_none_or(|(k, _)| *k != key) {
            let image = draw();
            let cell = self.picker.font_size();
            let tiles = tiles(area.width, area.height);
            let first_id = self.next_id;
            self.next_id = self.next_id.wrapping_add(tiles.len() as u32).max(1);
            let (compress, tmux, picker) =
                (self.compress, self.picker.tmux_detected(), &self.picker);
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
