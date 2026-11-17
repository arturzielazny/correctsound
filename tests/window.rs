//! How fast the window (`--window`) draws, so a change that makes it sluggish is caught, and
//! that what it draws is a picture.
//!
//! The same review as `tests/performance.rs`, in a window the size of a 5120 × 2160 monitor
//! at twice the standard density. The work on the CPU (laying the screen out and recording
//! what changed for the GPU) is timed against the yardstick, as there. Where a GPU is found
//! (not a software renderer standing in for one), each frame is also drawn on it, timed (printed, not judged: GPUs differ too much) and
//! checked for being more than an empty background. With `CORRECTSOUND_WINDOW_SHOTS` set to
//! a directory, the frames are saved there as PNG files.
//!
//! `cargo test --features window --test window -- --nocapture` prints the times.

#![cfg(feature = "window")]

mod common;

use std::time::{Duration, Instant};

use common::{measured_session, ms, press, settle, tick_until, within, yardstick};
use correctsound::app::{App, Options, Overlay, Step};
use correctsound::ui::theme::Theme;
use correctsound::ui::window::Screen;
use correctsound::ui::window::gpu::Gpu;
use crossterm::event::KeyCode;

/// A 5120 × 2160 window at twice the standard density, with 9-point text: about the 360 ×
/// 68 cells of the terminal in `tests/performance.rs`.
const SIZE: (u32, u32) = (5120, 2160);
const PT: f32 = 9.0;
const SCALE: f32 = 2.0;

/// Budgets, in yardsticks or ms. What a 24-core desktop takes is in brackets, in the test
/// build. A frame whose charts changed: the screen laid out and the charts recorded for the
/// GPU (0.1–0.3 yardsticks; the terminal takes 2–3.5 for the same frame).
const CHANGED_FRAMES: f64 = 1.0;
/// A placement update's frame (about 0.1 yardsticks).
const PLACEMENT_FRAMES: f64 = 1.0;
/// A frame with nothing new: laid out and found unchanged (1–3 ms).
const STEADY_FRAME_MS: f64 = 30.0;

/// The window's screen, and the GPU when there is one.
struct Window {
    screen: Screen,
    gpu: Option<Gpu>,
    shots: Option<std::path::PathBuf>,
}

impl Window {
    fn new() -> Option<Self> {
        let screen = match Screen::new(Theme::parse(""), SIZE, PT, SCALE) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("skipped: {e}");
                return None;
            }
        };
        let gpu = match Gpu::headless() {
            // A software renderer stands in on build servers without a GPU; the Windows
            // one crashed under vello there. Timing it would say nothing about a screen.
            Ok(g) if g.is_software() => {
                eprintln!("{:<44} {}: the CPU's part only", "GPU", g.name());
                None
            }
            Ok(g) => {
                eprintln!("{:<44} {}", "GPU", g.name());
                Some(g)
            }
            Err(e) => {
                eprintln!("{:<44} none ({e}): the CPU's part only", "GPU");
                None
            }
        };
        let (cols, rows) = screen.cells();
        eprintln!("{:<44} {cols} × {rows} cells", "window");
        Some(Self {
            screen,
            gpu,
            shots: std::env::var_os("CORRECTSOUND_WINDOW_SHOTS").map(Into::into),
        })
    }

    /// A frame as the window draws one: laid out and recorded (timed, ms), and, when it
    /// changed and there is a GPU, drawn there. Returns the CPU's time and whether the
    /// frame changed.
    fn frame(&mut self, app: &App, name: &str) -> (f64, bool) {
        let t = Instant::now();
        let changed = self.screen.update(app).expect("laid out");
        let scene = self.screen.scene();
        let cpu = ms(t);
        if changed && let Some(gpu) = &mut self.gpu {
            let t = Instant::now();
            gpu.render(&scene, self.screen.size(), self.screen.background())
                .expect("drawn on the GPU");
            let submitted = ms(t);
            gpu.wait();
            if !name.is_empty() {
                eprintln!(
                    "  {name}: GPU {submitted:.1} ms on the CPU to hand over, {:.1} ms until drawn",
                    ms(t)
                );
            }
            let image = gpu.read_back().expect("read back");
            let bg = self.screen.background();
            let drawn = image
                .pixels()
                .filter(|p| (p[0], p[1], p[2]) != (bg.0, bg.1, bg.2))
                .count();
            let share = drawn as f64 / f64::from(image.width() * image.height());
            assert!(
                share > 0.02,
                "{name}: only {share:.3} of the frame is drawn"
            );
            if let Some(dir) = self.shots.as_ref().filter(|_| !name.is_empty()) {
                std::fs::create_dir_all(dir).expect("shots directory");
                image.save(dir.join(format!("{name}.png"))).expect("saved");
            }
        }
        (cpu, changed)
    }
}

/// Placement on the fastest sweeps in the window: every update's frame.
fn placement(app: &mut App, window: &mut Window, unit: f64) {
    press(app, KeyCode::Char('p'));
    press(app, KeyCode::Char('m'));
    press(app, KeyCode::Char('m'));
    let count = |a: &App| a.placement.as_ref().map_or(0, |p| p.count);
    tick_until(app, "fastest sweeps", Duration::from_secs(20), |a| {
        count(a) >= 3
    });
    let mut slowest = 0f64;
    for k in 0..10 {
        let seen = count(app);
        tick_until(app, "an update", Duration::from_secs(10), |a| {
            count(a) > seen
        });
        let (took, changed) = window.frame(app, if k == 9 { "placement" } else { "" });
        assert!(changed, "an update reaches the window");
        slowest = slowest.max(took);
    }
    within(
        "placement: slowest frame of ten updates",
        slowest,
        PLACEMENT_FRAMES * unit,
    );
    press(app, KeyCode::Esc);
}

#[test]
fn the_window_draws_at_once() {
    let root = std::env::temp_dir().join(format!("correctsound-window-{}", std::process::id()));
    correctsound::paths::set_root(root);
    let Some(mut window) = Window::new() else {
        return;
    };
    let unit = yardstick();
    eprintln!(
        "{:<44} {unit:7.1} ms",
        "yardstick (a fixed piece of drawing)"
    );
    let changed = CHANGED_FRAMES * unit;
    let session = measured_session(|app| placement(app, &mut window, unit));

    let mut app = App::new(Options {
        review: Some(session),
        ..Options::default()
    });
    assert_eq!(app.step, Step::Result);
    settle(&mut app);
    let (took, fresh) = window.frame(&app, "result");
    assert!(fresh);
    within("first frame of the result screen", took, changed);
    let (took, fresh) = window.frame(&app, "");
    within("the same frame again", took, STEADY_FRAME_MS);
    assert!(!fresh, "an unchanged frame is not drawn again");

    for key in ['h', 'h', 't', 'T', 'h', 'h', 'h', 'h'] {
        press(&mut app, KeyCode::Char(key));
    }
    settle(&mut app);
    let (took, fresh) = window.frame(&app, "result-300hz");
    assert!(fresh, "the new design reaches the window");
    within("frame with the new design", took, changed);

    press(&mut app, KeyCode::Char('r'));
    assert_eq!(app.overlay, Some(Overlay::Room));
    app.tick();
    let (took, _) = window.frame(&app, "room");
    within("first frame of the room screen", took, changed);
    for (name, shot, key) in [
        ("move a number (→)", "", KeyCode::Right),
        ("the next standing wave (])", "", KeyCode::Char(']')),
        ("where to sit (v)", "room-seats", KeyCode::Char('v')),
        ("the next room (g)", "", KeyCode::Char('g')),
    ] {
        let t = Instant::now();
        press(&mut app, key);
        app.tick();
        let work = ms(t);
        let (frame, _) = window.frame(&app, shot);
        // The key and the room model's work are the app's, timed in tests/performance.rs;
        // here only the window's frame is judged.
        eprintln!("{:<44} {work:7.1} ms", format!("{name}: key and work"));
        within("  its frame", frame, changed);
    }
}
