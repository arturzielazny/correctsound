//! The app's screen on a canvas: ratatui lays it out in cells as in the terminal, the cells
//! become rectangles and runs of text ([`correctsound::ui::cells`]), and the charts are
//! pixels drawn by the app itself, each made again only when it changes.

use std::collections::HashMap;
use std::sync::Arc;

use ab_glyph::{Font, FontVec};
use correctsound::app::App;
use correctsound::ui::cells::{self, CellSize, Fill};
use correctsound::ui::graphics::Graphics;
use correctsound::ui::theme::{Rgb, Theme};
use image::RgbaImage;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use wasm_bindgen::{Clamped, JsCast, JsValue};
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};

/// Text size in CSS pixels, before the browser's zoom.
const TEXT_PX: f64 = 13.0;

/// A cell's size and where text sits in it, in the canvas's pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Metrics {
    cell_w: u16,
    cell_h: u16,
    /// The font size at which a character is exactly a cell wide.
    px: f64,
    baseline: f64,
    line: f64,
}

impl Metrics {
    /// For text `css_px` large on a screen with `dpr` device pixels per CSS pixel. A cell is
    /// a whole number of pixels wide, and the font is sized to fill it exactly, so runs of
    /// text line up with the cells as a terminal's do.
    fn new(font: &FontVec, css_px: f64, dpr: f64) -> Self {
        let upem = f64::from(font.units_per_em().unwrap_or(1000.0));
        let advance = f64::from(font.h_advance_unscaled(font.glyph_id('0'))).max(1.0);
        let em = css_px * dpr;
        let cell_w = (em * advance / upem).round().max(1.0);
        let px = cell_w * upem / advance;
        let scale = px / upem;
        let ascent = f64::from(font.ascent_unscaled()) * scale;
        let descent = f64::from(font.descent_unscaled()) * scale;
        let gap = f64::from(font.line_gap_unscaled()) * scale;
        let cell_h = (ascent - descent + gap).round().max(1.0);
        let top = ((cell_h - (ascent - descent)) / 2.0).round();
        Self {
            cell_w: cell_w as u16,
            cell_h: cell_h as u16,
            px,
            baseline: top + ascent.round(),
            line: (em * 0.06).round().max(1.0),
        }
    }

    fn cells(&self) -> CellSize {
        CellSize {
            w: f64::from(self.cell_w),
            h: f64::from(self.cell_h),
            line: self.line,
            baseline: self.baseline,
        }
    }
}

/// The canvas and what is drawn on it.
pub struct Screen {
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    /// The font's CSS family name, and the font itself (for which characters it has).
    family: String,
    font: FontVec,
    theme: Theme,
    terminal: Terminal<TestBackend>,
    gfx: Graphics,
    metrics: Metrics,
    /// The canvas's size in its own pixels, and device pixels per CSS pixel.
    size: (u32, u32),
    dpr: f64,
    /// What was drawn last: the cells and the charts.
    grid: Option<Buffer>,
    charts: Vec<(Rect, Arc<RgbaImage>)>,
    /// Each chart's pixels, ready to draw, by the chart they were made from.
    chart_canvases: HashMap<usize, (Arc<RgbaImage>, HtmlCanvasElement)>,
}

fn context(canvas: &HtmlCanvasElement) -> Result<CanvasRenderingContext2d, JsValue> {
    canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("the browser draws no 2D canvas"))?
        .dyn_into()
        .map_err(|_| JsValue::from_str("the canvas's context is not 2D"))
}

fn css(c: Rgb) -> String {
    format!("rgb({},{},{})", c.0, c.1, c.2)
}

impl Screen {
    /// The app's screen on `canvas`, its text in `font` (`font_bytes`), loaded in the page
    /// as `family`.
    ///
    /// # Errors
    ///
    /// When the font cannot be read or the canvas does not draw.
    pub fn new(
        canvas: HtmlCanvasElement,
        font_bytes: Vec<u8>,
        family: String,
    ) -> Result<Self, JsValue> {
        let font = FontVec::try_from_vec(font_bytes.clone())
            .map_err(|_| JsValue::from_str("the font cannot be read"))?;
        let ctx = context(&canvas)?;
        let theme = Theme::parse("");
        let metrics = Metrics::new(&font, TEXT_PX, 1.0);
        let gfx = Graphics::pixels(
            theme.clone(),
            correctsound::ui::raster::font_from(font_bytes),
            (metrics.cell_w, metrics.cell_h),
        );
        let terminal = Terminal::new(TestBackend::new(1, 1))
            .map_err(|e| JsValue::from_str(&format!("laying out the screen: {e}")))?;
        let mut screen = Self {
            canvas,
            ctx,
            family,
            font,
            theme,
            terminal,
            gfx,
            metrics,
            size: (0, 0),
            dpr: 0.0,
            grid: None,
            charts: Vec::new(),
            chart_canvases: HashMap::new(),
        };
        screen.fit();
        Ok(screen)
    }

    /// Follows the canvas's size on the page and the browser's zoom; true when they changed.
    pub fn fit(&mut self) -> bool {
        let dpr = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        let size = (
            (f64::from(self.canvas.client_width()) * dpr)
                .round()
                .max(1.0) as u32,
            (f64::from(self.canvas.client_height()) * dpr)
                .round()
                .max(1.0) as u32,
        );
        if size == self.size && dpr == self.dpr {
            return false;
        }
        self.size = size;
        self.canvas.set_width(size.0);
        self.canvas.set_height(size.1);
        if dpr != self.dpr {
            self.dpr = dpr;
            self.metrics = Metrics::new(&self.font, TEXT_PX, dpr);
            self.gfx
                .set_cell_size((self.metrics.cell_w, self.metrics.cell_h));
        }
        let fit = |px: u32, cell: u16| (px / u32::from(cell)).clamp(1, u32::from(u16::MAX)) as u16;
        let (cols, rows) = (
            fit(size.0, self.metrics.cell_w),
            fit(size.1, self.metrics.cell_h),
        );
        self.terminal.backend_mut().resize(cols, rows);
        self.grid = None;
        true
    }

    /// Lays the app's screen out again; true when what it shows changed.
    ///
    /// # Errors
    ///
    /// Never in practice: laying out into memory does not fail.
    pub fn update(&mut self, app: &App) -> Result<bool, JsValue> {
        self.gfx.begin_frame();
        let gfx = &mut self.gfx;
        self.terminal
            .draw(|f| correctsound::ui::draw(f, app, gfx))
            .map_err(|e| JsValue::from_str(&format!("laying out the screen: {e}")))?;
        let charts = self.gfx.take_placed_pixels();
        let buffer = self.terminal.backend().buffer();
        let grid_changed = self.grid.as_ref() != Some(buffer);
        let charts_changed = charts.len() != self.charts.len()
            || charts
                .iter()
                .zip(&self.charts)
                .any(|((a, s), (b, t))| a != b || !Arc::ptr_eq(s, t));
        if grid_changed {
            self.grid = Some(buffer.clone());
        }
        self.charts = charts;
        Ok(grid_changed || charts_changed)
    }

    /// Draws what [`Self::update`] laid out.
    ///
    /// # Errors
    ///
    /// When the canvas does not take a chart's pixels.
    pub fn paint(&mut self) -> Result<(), JsValue> {
        let Some(grid) = &self.grid else {
            return Ok(());
        };
        let ctx = &self.ctx;
        let m = self.metrics;
        let shapes = cells::shapes(grid, &self.theme, m.cells());
        ctx.set_global_alpha(1.0);
        ctx.set_fill_style_str(&css(self.theme.background));
        ctx.fill_rect(0.0, 0.0, f64::from(self.size.0), f64::from(self.size.1));
        let fill = |fills: &[Fill]| {
            for f in fills {
                ctx.set_global_alpha(f64::from(f.alpha));
                ctx.set_fill_style_str(&css(f.color));
                let a = f.area;
                ctx.fill_rect(a.x0, a.y0, a.x1 - a.x0, a.y1 - a.y0);
            }
            ctx.set_global_alpha(1.0);
        };
        fill(&shapes.backgrounds);
        fill(&shapes.lines);
        fill(&shapes.blocks);

        // The charts, each from its pixels made once.
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or_else(|| JsValue::from_str("no document"))?;
        let mut kept = HashMap::new();
        for (area, chart) in &self.charts {
            let id = Arc::as_ptr(chart) as usize;
            let canvas = match self.chart_canvases.remove(&id) {
                Some((_, canvas)) => canvas,
                None => {
                    let canvas: HtmlCanvasElement =
                        document.create_element("canvas")?.dyn_into()?;
                    canvas.set_width(chart.width());
                    canvas.set_height(chart.height());
                    let pixels = ImageData::new_with_u8_clamped_array_and_sh(
                        Clamped(chart.as_raw()),
                        chart.width(),
                        chart.height(),
                    )?;
                    context(&canvas)?.put_image_data(&pixels, 0.0, 0.0)?;
                    canvas
                }
            };
            ctx.draw_image_with_html_canvas_element(
                &canvas,
                f64::from(area.x) * f64::from(m.cell_w),
                f64::from(area.y) * f64::from(m.cell_h),
            )?;
            kept.insert(id, (Arc::clone(chart), canvas));
        }
        self.chart_canvases = kept;

        // The text: runs of the characters the font has, as one string each (it is sized so
        // a character is a cell wide); any other on its own, from the browser's fonts.
        ctx.set_text_baseline("alphabetic");
        let (regular, bold) = (
            format!("{}px \"{}\"", m.px, self.family),
            format!("bold {}px \"{}\"", m.px, self.family),
        );
        let mut current: Option<(bool, Rgb)> = None;
        for run in &shapes.text {
            if current != Some((run.bold, run.color)) {
                ctx.set_font(if run.bold { &bold } else { &regular });
                ctx.set_fill_style_str(&css(run.color));
                current = Some((run.bold, run.color));
            }
            let y = f64::from(run.y) * f64::from(m.cell_h) + m.baseline;
            let x_of = |i: usize| (usize::from(run.x) + i) as f64 * f64::from(m.cell_w);
            let mut start: Option<usize> = None;
            let chars: Vec<char> = run.text.chars().collect();
            for i in 0..=chars.len() {
                let ours = chars.get(i).is_some_and(|&c| self.font.glyph_id(c).0 != 0);
                if ours {
                    start.get_or_insert(i);
                    continue;
                }
                if let Some(s) = start.take() {
                    let text: String = chars[s..i].iter().collect();
                    ctx.fill_text(&text, x_of(s), y)?;
                }
                if let Some(&c) = chars.get(i)
                    && c != ' '
                {
                    ctx.fill_text(&c.to_string(), x_of(i), y)?;
                }
            }
        }
        fill(&shapes.underlines);
        Ok(())
    }
}
