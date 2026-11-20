//! The window's fonts: the system monospace font, its bold, and others for symbols it lacks,
//! with the cell size a terminal would give them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont};
use vello::peniko::{Blob, FontData};

use crate::ui::raster;

/// One font, for measuring (ab_glyph) and for the GPU (vello).
pub struct Face {
    pub glyphs: FontVec,
    pub data: FontData,
}

impl Face {
    fn new(bytes: Vec<u8>) -> Option<Self> {
        let data = FontData::new(Blob::from(bytes.clone()), 0);
        Some(Self {
            glyphs: raster::font_from(bytes)?,
            data,
        })
    }
}

/// A cell's size and where text sits in it, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub cell_w: f32,
    pub cell_h: f32,
    /// ab_glyph's size for the text (the font's height, ascent to descent).
    pub px: f32,
    /// From a cell's top to the text's baseline.
    pub baseline: f32,
    /// Thickness of box lines and underlines.
    pub line: f32,
}

impl Metrics {
    /// The cell size as whole pixels.
    #[must_use]
    pub fn cell(&self) -> (u16, u16) {
        (self.cell_w as u16, self.cell_h as u16)
    }
}

/// Symbols the screens use that monospace fonts may lack: looked for as the window opens,
/// so they are found by the time they are first shown.
const SYMBOLS: &str = "✔✘✓›•…·↑↓←→±×≈≤≥°▸▶┃";

/// A fallback font found for a symbol: its file and contents, or `None` when no font has it.
type Found = (char, Option<(PathBuf, Vec<u8>)>);

/// The monospace font, its bold, and fallbacks found per missing symbol.
pub struct Fonts {
    /// Regular first, bold second (the regular again where there is no bold), then the
    /// fallbacks in the order they were found.
    faces: Vec<Face>,
    /// Which face draws a symbol the first two lack (`None`: no font has it).
    fallback: HashMap<char, Option<usize>>,
    /// The fallback faces' files, so a font that has several symbols is loaded once.
    files: HashMap<PathBuf, usize>,
    /// Symbols being looked for: asked of a thread, as fontconfig takes milliseconds, which
    /// a frame must not wait for. A symbol is drawn once its font arrives.
    ask: Sender<char>,
    answers: Receiver<Found>,
}

/// The regular face.
pub const REGULAR: usize = 0;
/// The bold face.
pub const BOLD: usize = 1;

impl Fonts {
    /// The system monospace font; `None` when there is none to be found.
    #[must_use]
    pub fn load() -> Option<Self> {
        let regular = raster::load_font_bytes()?;
        let bold = raster::font_bytes_matching("monospace:bold").unwrap_or_else(|| regular.clone());
        let (ask, questions) = channel::<char>();
        let (tell, answers) = channel::<Found>();
        std::thread::spawn(move || {
            for c in questions {
                let pattern = format!("monospace:charset={:x}", u32::from(c));
                let found = raster::font_file_matching(&pattern)
                    .and_then(|path| Some((path.clone(), std::fs::read(path).ok()?)));
                if tell.send((c, found)).is_err() {
                    break;
                }
            }
        });
        let mut fonts = Self {
            faces: vec![Face::new(regular)?, Face::new(bold)?],
            fallback: HashMap::new(),
            files: HashMap::new(),
            ask,
            answers,
        };
        for c in SYMBOLS.chars() {
            fonts.glyph(c, false);
        }
        Some(fonts)
    }

    /// Takes in the fallback fonts found since the last call; true when there are any (the
    /// symbols waiting for them can now be drawn).
    pub fn collect(&mut self) -> bool {
        let mut any = false;
        while let Ok((c, found)) = self.answers.try_recv() {
            let index = found.and_then(|(path, bytes)| {
                let index = match self.files.get(&path) {
                    Some(&i) => i,
                    None => {
                        self.faces.push(Face::new(bytes)?);
                        self.files.insert(path, self.faces.len() - 1);
                        self.faces.len() - 1
                    }
                };
                (self.faces[index].glyphs.glyph_id(c).0 != 0).then_some(index)
            });
            any |= index.is_some();
            self.fallback.insert(c, index);
        }
        any
    }

    /// The regular font's file contents, for the charts' labels.
    #[must_use]
    pub fn regular_bytes(&self) -> Vec<u8> {
        self.faces[REGULAR].glyphs.as_slice().to_vec()
    }

    #[must_use]
    pub fn face(&self, index: usize) -> &Face {
        &self.faces[index]
    }

    /// The face and glyph that draw `c`, bold or not: the monospace font when it has it,
    /// else a fallback (looked for once per symbol, in the background). `None` when no font
    /// has it, or none has been found yet.
    pub fn glyph(&mut self, c: char, bold: bool) -> Option<(usize, GlyphId)> {
        let first = if bold { BOLD } else { REGULAR };
        for face in [first, REGULAR] {
            let id = self.faces[face].glyphs.glyph_id(c);
            if id.0 != 0 {
                return Some((face, id));
            }
        }
        match self.fallback.get(&c) {
            Some(Some(index)) => Some((*index, self.faces[*index].glyphs.glyph_id(c))),
            Some(None) => None,
            None => {
                // Asked once; drawn when the answer is in (see [`Self::collect`]).
                self.fallback.insert(c, None);
                let _ = self.ask.send(c);
                None
            }
        }
    }

    /// The cell size for text `pt` points large on a screen `scale` times the standard
    /// density, as a terminal works it out: one advance wide and the font's line tall,
    /// rounded to whole pixels.
    #[must_use]
    pub fn metrics(&self, pt: f32, scale: f32) -> Metrics {
        let font = &self.faces[REGULAR].glyphs;
        let em = pt * 96.0 / 72.0 * scale;
        let upem = font.units_per_em().unwrap_or(1000.0);
        let px = em * font.height_unscaled() / upem;
        let sf = font.as_scaled(PxScale::from(px));
        let cell_w = sf.h_advance(font.glyph_id('0')).round().max(1.0);
        let cell_h = (sf.ascent() - sf.descent() + sf.line_gap())
            .round()
            .max(1.0);
        let top = ((cell_h - (sf.ascent() - sf.descent())) / 2.0).round();
        Metrics {
            cell_w,
            cell_h,
            px,
            baseline: top + sf.ascent().round(),
            line: (em * 0.06).round().max(1.0),
        }
    }
}
