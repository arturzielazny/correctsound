//! Renders the wizard's screens to PNG files, driving it against the simulator: the
//! screenshots on the project's website.
//!
//! ```sh
//! cargo run --release --example screenshots -- docs/img
//! ```
//!
//! Nothing plays: the simulator stands in for the speakers and the mic. Charts are drawn
//! exactly as the app draws them in Ghostty or kitty; the text is drawn with the system's
//! monospace font, in Tokyo Night colours.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use correctsound::app::{App, LevelPhase, Options, Overlay, Step};
use correctsound::ui::graphics::Graphics;
use correctsound::ui::raster::{Canvas, Pen};
use correctsound::ui::theme::{Rgb, Theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

const COLS: u16 = 150;
const ROWS: u16 = 44;
/// Cell size in pixels.
const CELL: (u16, u16) = (12, 26);

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "docs/img".into()));
    std::fs::create_dir_all(&out).expect("output directory");
    let root = std::env::temp_dir().join(format!("correctsound-shots-{}", std::process::id()));
    correctsound::paths::set_root(root.clone());
    let mut shots = Shots::new(out);
    let mut app = App::new(Options {
        simulate: true,
        ..Options::default()
    });
    shots.take(&app, "1-devices");

    press(&mut app, KeyCode::Enter);
    shots.take(&app, "3-mic-gain");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    // Listening to the room, then the noise at the level it calls for.
    tick_until(&mut app, |a| a.level.listened_secs() >= 3.0);
    shots.take(&app, "4a-listening");
    press(&mut app, KeyCode::Enter);
    tick_until(&mut app, |a| {
        a.level.phase == LevelPhase::Playing && a.level.heard[0].is_some() && !a.level.fading_in()
    });
    wait(&mut app, 1500);
    // Turn the (simulated) amplifier to the room's level, as a user would.
    for _ in 0..40 {
        let over = app.level.spl.unwrap_or(0.0) - app.level.target_spl;
        if over.abs() < 0.7 {
            break;
        }
        press(
            &mut app,
            if over > 0.0 {
                KeyCode::Down
            } else {
                KeyCode::Up
            },
        );
        wait(&mut app, 1200);
    }
    shots.take(&app, "4-level");
    press(&mut app, KeyCode::Tab);
    tick_until(&mut app, |a| a.level.heard[1].is_some());
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Measure);

    // Placement: pin where the speakers start, move them, compare.
    press(&mut app, KeyCode::Char('p'));
    tick_until(&mut app, |a| {
        a.placement.as_ref().is_some_and(|p| p.count >= 1)
    });
    // Too close to the wall: pin it, then move back out.
    for _ in 0..4 {
        press(&mut app, KeyCode::Left);
    }
    let seen = app.placement.as_ref().map_or(0, |p| p.count);
    tick_until(&mut app, |a| {
        a.placement.as_ref().is_some_and(|p| p.count >= seen + 2)
    });
    press(&mut app, KeyCode::Char(' '));
    for _ in 0..4 {
        press(&mut app, KeyCode::Right);
    }
    let seen = app.placement.as_ref().map_or(0, |p| p.count);
    tick_until(&mut app, |a| {
        a.placement.as_ref().is_some_and(|p| p.count >= seen + 2)
    });
    app.message = None;
    shots.take(&app, "5b-placement");
    // Fast sweeps: a sweep looping without pauses; then a worse placement, so the best one
    // so far shows faded behind it.
    press(&mut app, KeyCode::Char('m'));
    let seen = app.placement.as_ref().map_or(0, |p| p.count);
    tick_until(&mut app, |a| {
        a.placement.as_ref().is_some_and(|p| p.count >= seen + 5)
    });
    for _ in 0..6 {
        press(&mut app, KeyCode::Right);
    }
    let seen = app.placement.as_ref().map_or(0, |p| p.count);
    tick_until(&mut app, |a| {
        a.placement.as_ref().is_some_and(|p| p.count >= seen + 10)
    });
    app.message = None;
    shots.take(&app, "5d-fast");
    for _ in 0..6 {
        press(&mut app, KeyCode::Left);
    }
    // Live mode: the same comparison, from noise, updated ten times a second.
    press(&mut app, KeyCode::Char('m'));
    let seen = app.placement.as_ref().map_or(0, |p| p.count);
    tick_until(&mut app, |a| {
        a.placement.as_ref().is_some_and(|p| p.count >= seen + 15)
    });
    app.message = None;
    shots.take(&app, "5c-live");
    press(&mut app, KeyCode::Esc);

    press(&mut app, KeyCode::Enter);
    tick_until(&mut app, |a| {
        a.measure.progress().is_some_and(|(_, p)| p > 0.45)
    });
    shots.take(&app, "5a-sweeping");
    tick_until(&mut app, |a| {
        a.measure.positions.iter().all(|p| p.len() == 1) && !a.measure.running()
    });
    press(&mut app, KeyCode::Char('a'));
    tick_until(&mut app, |a| {
        a.measure.positions.iter().all(|p| p.len() == 2) && !a.measure.running()
    });
    shots.take(&app, "5-measured");

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Result);
    shots.take(&app, "6-result");
    press(&mut app, KeyCode::Char('c'));
    shots.take(&app, "6b-curiosities");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('w'));
    shots.take(&app, "6c-decay");
    press(&mut app, KeyCode::Esc);
    // The room from above: guessed from the measurement, then the seat map.
    press(&mut app, KeyCode::Char('r'));
    app.message = None;
    shots.take(&app, "6f-room");
    press(&mut app, KeyCode::Char('v'));
    shots.take(&app, "6g-room-seats");
    press(&mut app, KeyCode::Char('v'));
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('i'));
    shots.take(&app, "6d-info");
    press(&mut app, KeyCode::Char('m'));
    shots.take(&app, "6e-info-technical");
    press(&mut app, KeyCode::Esc);

    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Char('v'));
    tick_until(&mut app, |a| a.save.verify.is_some());
    shots.take(&app, "7-saved-verified");
    let _ = std::fs::remove_dir_all(root);
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn tick_until(app: &mut App, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out: {app:?} {:?}",
            app.message
        );
        app.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait(app: &mut App, ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        app.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Fonts for the text: regular, bold, and fallbacks for symbols the first lacks.
struct Fonts {
    regular: FontVec,
    bold: FontVec,
    fallbacks: Vec<FontVec>,
    px: f32,
}

fn font_file(pattern: &str) -> Option<FontVec> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    FontVec::try_from_vec(std::fs::read(path.trim()).ok()?).ok()
}

impl Fonts {
    fn load() -> Self {
        let regular = font_file("monospace").expect("a monospace font (fc-match monospace)");
        let bold = font_file("monospace:bold")
            .or_else(|| font_file("monospace"))
            .expect("a monospace font");
        let fallbacks = [
            "DejaVu Sans Mono",
            "Noto Sans Symbols2",
            "Noto Sans Symbols",
        ]
        .iter()
        .filter_map(|p| font_file(p))
        .collect();
        // A size whose advance is one cell wide.
        let advance = regular
            .as_scaled(PxScale::from(100.0))
            .h_advance(regular.glyph_id('0'));
        Self {
            px: f32::from(CELL.0) * 100.0 / advance,
            regular,
            bold,
            fallbacks,
        }
    }

    fn pick(&self, c: char, bold: bool) -> &FontVec {
        let first = if bold { &self.bold } else { &self.regular };
        if first.glyph_id(c).0 != 0 {
            return first;
        }
        self.fallbacks
            .iter()
            .find(|f| f.glyph_id(c).0 != 0)
            .unwrap_or(first)
    }
}

struct Shots {
    out: PathBuf,
    theme: Theme,
    fonts: Fonts,
}

impl Shots {
    fn new(out: PathBuf) -> Self {
        Self {
            out,
            theme: Theme::parse(""),
            fonts: Fonts::load(),
        }
    }

    fn take(&mut self, app: &App, name: &str) {
        let mut gfx = Graphics::offscreen(
            self.theme.clone(),
            correctsound::ui::raster::load_font(),
            CELL,
        );
        let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS)).expect("test backend");
        terminal
            .draw(|f| correctsound::ui::draw(f, app, &mut gfx))
            .expect("draw");
        let mut charts = gfx.take_captured();
        // An overlay hides the step's charts; the decay overlay draws one of its own and
        // the room overlay two (the plan and the bass at the seat), last.
        match app.overlay {
            Some(Overlay::Decay) => {
                let last = charts.pop();
                charts = last.into_iter().collect();
            }
            Some(Overlay::Room) => {
                let keep = charts.len().saturating_sub(2);
                charts.drain(..keep);
            }
            Some(_) => charts.clear(),
            None => {}
        }
        let img = self.compose(terminal.backend().buffer(), &charts);
        let path = self.out.join(format!("{name}.png"));
        save_png(&img, &path);
        println!("{}", path.display());
    }

    fn color(&self, c: Color, fg: bool) -> Rgb {
        let t = &self.theme;
        let hex = |v: u32| Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8);
        match c {
            Color::Reset if fg => t.foreground,
            Color::Reset => t.background,
            Color::Black => hex(0x15_16_1e),
            Color::Red => t.red,
            Color::LightRed => hex(0xff_7a_93),
            Color::Green => t.green,
            Color::LightGreen => hex(0xb9_f2_7c),
            Color::Yellow | Color::LightYellow => t.yellow,
            Color::Blue => t.blue,
            Color::LightBlue => hex(0x7d_a6_ff),
            Color::Magenta | Color::LightMagenta => t.magenta,
            Color::Cyan | Color::LightCyan => hex(0x7d_cf_ff),
            Color::Gray => hex(0x9a_a5_ce),
            Color::DarkGray => t.dim,
            Color::White => hex(0xc0_ca_f5),
            Color::Rgb(r, g, b) => Rgb(r, g, b),
            Color::Indexed(_) => t.foreground,
        }
    }

    fn compose(
        &self,
        buf: &ratatui::buffer::Buffer,
        charts: &[(ratatui::layout::Rect, image::DynamicImage)],
    ) -> image::RgbaImage {
        let (cw, ch) = (f32::from(CELL.0), f32::from(CELL.1));
        let mut canvas = Canvas::new(
            u32::from(COLS) * u32::from(CELL.0),
            u32::from(ROWS) * u32::from(CELL.1),
            self.theme.background,
            cw,
        );
        let px = self.fonts.px;
        let sf = self.fonts.regular.as_scaled(PxScale::from(px));
        let text_top = (ch - (sf.ascent() - sf.descent())) / 2.0;
        for y in 0..ROWS {
            for x in 0..COLS {
                let cell = &buf[(x, y)];
                let (x0, y0) = (f32::from(x) * cw, f32::from(y) * ch);
                let bg = self.color(cell.bg, false);
                if bg != self.theme.background {
                    canvas.rect(x0, y0, cw, ch, bg, 1.0);
                }
                let fg = self.color(cell.fg, true);
                let sym = cell.symbol();
                let Some(c) = sym.chars().next() else {
                    continue;
                };
                if c == ' ' {
                    continue;
                }
                if box_line(&mut canvas, c, (x0, y0), (cw, ch), fg) {
                    continue;
                }
                let bold = cell.modifier.contains(Modifier::BOLD);
                let font = self.fonts.pick(c, bold);
                canvas.text(Some(font), sym, (x0, y0 + text_top), px, fg, 1.0);
                if cell.modifier.contains(Modifier::UNDERLINED) {
                    canvas.line(
                        (x0, y0 + ch - 3.0),
                        (x0 + cw, y0 + ch - 3.0),
                        Pen::solid(fg, 1.0, 1.5),
                    );
                }
            }
        }
        let mut img = canvas.into_image().to_rgba8();
        for (area, chart) in charts {
            image::imageops::overlay(
                &mut img,
                &chart.to_rgba8(),
                i64::from(area.x) * i64::from(CELL.0),
                i64::from(area.y) * i64::from(CELL.1),
            );
        }
        img
    }
}

/// Draws the border characters as lines through the cell, so they join up exactly.
fn box_line(c: &mut Canvas, ch: char, (x, y): (f32, f32), (w, h): (f32, f32), color: Rgb) -> bool {
    let (mx, my) = (x + w / 2.0, y + h / 2.0);
    let pen = Pen::solid(color, 0.55, 1.3);
    let (left, right, up, down) = match ch {
        '─' => (true, true, false, false),
        '│' => (false, false, true, true),
        '┌' => (false, true, false, true),
        '┐' => (true, false, false, true),
        '└' => (false, true, true, false),
        '┘' => (true, false, true, false),
        _ => return false,
    };
    if left {
        c.line((x, my), (mx, my), pen);
    }
    if right {
        c.line((mx, my), (x + w, my), pen);
    }
    if up {
        c.line((mx, y), (mx, my), pen);
    }
    if down {
        c.line((mx, my), (mx, y + h), pen);
    }
    true
}

fn save_png(img: &image::RgbaImage, path: &Path) {
    img.save_with_format(path, image::ImageFormat::Png)
        .expect("write PNG");
}
