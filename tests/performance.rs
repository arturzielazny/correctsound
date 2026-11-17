//! How fast the app answers, so a change that makes it sluggish is caught.
//!
//! A measurement made on the simulator's living room is reviewed on a terminal the size of
//! a 5120 × 2160 monitor's (360 × 68 cells of 15 × 32 pixels), with charts sent as
//! compressed kitty images as in Ghostty. Two kinds of checks:
//!
//! - Facts that do not depend on the machine: how many bytes a frame sends to the terminal,
//!   that a frame with nothing new sends nothing, that a run of key presses is designed once
//!   rather than once per press, that a screen under an overlay is not drawn.
//! - Times. Drawing budgets are multiples of a yardstick, a fixed piece of drawing timed on
//!   the machine running the test, so a slower machine gets more time and the same slowdown
//!   is caught on any machine; the others have budgets many times what a current desktop
//!   takes. A real slowdown (a computation back on the key path, a sort per pixel) fails
//!   either way. `cargo test --test performance -- --nocapture` prints them.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use correctsound::app::{App, LevelPhase, Options, Overlay, Step};
use correctsound::audio::sim::{SimConfig, SimState};
use correctsound::ui::graphics::Graphics;
use correctsound::ui::raster::{Canvas, Pen};
use correctsound::ui::theme::Rgb;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui_image::picker::ProtocolType;

/// The terminal of a 5120 × 2160 monitor, and its cell in pixels.
const COLS: u16 = 360;
const ROWS: u16 = 68;
const CELL: (u16, u16) = (15, 32);

/// Budgets, ms, or yardsticks (see [`yardstick`]). What a 24-core desktop takes is in
/// brackets, in the test build (a release build is two to three times faster).
/// A key on any screen returns (under 1 ms; the work it starts runs in the background or
/// once before the next frame).
const KEY_MS: f64 = 50.0;
/// A frame with nothing new (2–5 ms).
const STEADY_FRAME_MS: f64 = 40.0;
/// A frame whose charts changed: drawn, compressed and encoded (2–3.5 yardsticks; a sort
/// per pixel of the room's floor, as once happened, made it 13).
const CHANGED_FRAMES: f64 = 8.0;
/// A placement frame: its curve and waterfall drawn anew for every update, ten times a
/// second on the fastest sweeps (about 2.6 yardsticks).
const PLACEMENT_FRAMES: f64 = 6.0;
/// Background work after a key: the new design (100–200 ms).
const DESIGN_MS: f64 = 4000.0;
/// Opening a review: every recording analysed again (about 0.5 s).
const REVIEW_MS: f64 = 8000.0;
/// What one frame may send to the terminal at most (about 1 MB; 48 MB uncompressed).
const FRAME_BYTES: usize = 6_000_000;

/// A fixed piece of drawing, timed on this machine, ms (about 40 on a current desktop in the
/// test build): a chart-sized canvas filled, gridded and drawn with long curves, the kind of
/// work a changed frame does. The fastest of five runs, so a stall does not inflate it.
fn yardstick() -> f64 {
    (0..5)
        .map(|_| {
            let t = Instant::now();
            let mut c = Canvas::new(2400, 1200, Rgb(26, 27, 38), 15.0);
            for k in 0..40 {
                let y = 30.0 * k as f32;
                c.line((0.0, y), (2400.0, y), Pen::solid(Rgb(60, 60, 80), 0.5, 1.0));
            }
            for k in 0..6 {
                let points: Vec<Option<(f32, f32)>> = (0..2400)
                    .map(|x| {
                        let x = x as f32;
                        Some((x, 600.0 + 300.0 * ((x / (40.0 + 9.0 * k as f32)).sin())))
                    })
                    .collect();
                c.polyline(&points, Pen::solid(Rgb(255, 158, 100), 0.9, 3.0));
            }
            let area: Vec<(f32, f32)> = (0..=240)
                .map(|x| (x as f32 * 10.0, 900.0 + 80.0 * (x as f32 / 7.0).sin()))
                .chain([(2400.0, 1200.0), (0.0, 1200.0)])
                .collect();
            c.fill(&area, Rgb(122, 162, 247), 0.3);
            let image = c.into_image();
            std::hint::black_box(&image);
            ms(t)
        })
        .fold(f64::INFINITY, f64::min)
}

fn press(app: &mut App, code: KeyCode) -> f64 {
    let t = Instant::now();
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    ms(t)
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

fn tick_until(app: &mut App, what: &str, timeout: Duration, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        app.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The background work a key started, timed until it is done.
fn settle(app: &mut App) -> f64 {
    let t = Instant::now();
    tick_until(app, "background work", Duration::from_secs(60), |a| {
        !a.working()
    });
    ms(t)
}

/// A screen drawn as the event loop draws it: time, and bytes of images sent.
struct Screen {
    terminal: Terminal<TestBackend>,
    gfx: Graphics,
}

impl Screen {
    fn new() -> Self {
        Self {
            terminal: Terminal::new(TestBackend::new(COLS, ROWS)).expect("test backend"),
            gfx: Graphics::with_protocol(ProtocolType::Kitty, CELL, true),
        }
    }

    fn frame(&mut self, app: &App) -> (f64, usize) {
        let t = Instant::now();
        self.gfx.begin_frame();
        let gfx = &mut self.gfx;
        self.terminal
            .draw(|f| correctsound::ui::draw(f, app, gfx))
            .expect("draw");
        let took = ms(t);
        // Image transmissions sit in the first cell of each image, as long escape strings.
        let sent = self
            .terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol().len())
            .filter(|&n| n > 64)
            .sum();
        (took, sent)
    }
}

/// Placement on the fastest sweeps: every update's frame, drawn while the sweep loops.
fn placement(app: &mut App, unit: f64) {
    let mut screen = Screen::new();
    press(app, KeyCode::Char('p'));
    press(app, KeyCode::Char('m'));
    press(app, KeyCode::Char('m'));
    let count = |a: &App| a.placement.as_ref().map_or(0, |p| p.count);
    tick_until(app, "fastest sweeps", Duration::from_secs(20), |a| {
        count(a) >= 3
    });
    let (mut slowest, mut most, mut frames) = (0f64, 0usize, 0);
    while frames < 10 {
        let seen = count(app);
        tick_until(app, "an update", Duration::from_secs(10), |a| {
            count(a) > seen
        });
        let (took, sent) = screen.frame(app);
        slowest = slowest.max(took);
        most = most.max(sent);
        frames += 1;
    }
    within(
        "placement: slowest frame of ten updates",
        slowest,
        PLACEMENT_FRAMES * unit,
    );
    eprintln!("{:<44} {:7.2} MB", "  most sent in one", most as f64 / 1e6);
    assert!(most <= FRAME_BYTES, "{most} bytes in one placement frame");
    press(app, KeyCode::Esc);
}

/// A measurement of the simulated living room from two mic positions, saved as a session.
fn measured_session(unit: f64) -> PathBuf {
    let mut app = App::new(Options {
        simulate: true,
        sim_config: Some(SimConfig::living_room()),
        sim_state: Some(SimState::default()),
        ..Options::default()
    });
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Level);
    tick_until(&mut app, "a level", Duration::from_secs(10), |a| {
        a.level.suggestion.is_some()
    });
    press(&mut app, KeyCode::Enter);
    tick_until(&mut app, "the left speaker", Duration::from_secs(10), |a| {
        a.level.phase == LevelPhase::Playing && a.level.heard[0].is_some()
    });
    press(&mut app, KeyCode::Tab);
    tick_until(
        &mut app,
        "the right speaker",
        Duration::from_secs(10),
        |a| a.level.heard[1].is_some(),
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Measure);
    placement(&mut app, unit);
    press(&mut app, KeyCode::Enter);
    for n in 1..=2 {
        if n > 1 {
            press(&mut app, KeyCode::Char('a'));
        }
        tick_until(&mut app, "a mic position", Duration::from_secs(30), |a| {
            a.measure.positions.iter().all(|p| p.len() == n) && !a.measure.running()
        });
    }
    app.session_dir().expect("a session")
}

/// Checks `took` against `budget`, saying what it was.
fn within(what: &str, took: f64, budget: f64) {
    eprintln!("{what:<44} {took:7.1} ms   (budget {budget:.0})");
    assert!(took <= budget, "{what} took {took:.0} ms, over {budget:.0}");
}

#[test]
fn reviewing_answers_at_once() {
    let root = std::env::temp_dir().join(format!("correctsound-perf-{}", std::process::id()));
    correctsound::paths::set_root(root);
    let unit = yardstick();
    eprintln!(
        "{:<44} {unit:7.1} ms",
        "yardstick (a fixed piece of drawing)"
    );
    let changed = CHANGED_FRAMES * unit;
    let session = measured_session(unit);

    // Opening the review: every recording analysed again, in parallel.
    let t = Instant::now();
    let mut app = App::new(Options {
        review: Some(session),
        ..Options::default()
    });
    within("open a review", ms(t), REVIEW_MS);
    assert_eq!(app.step, Step::Result);
    within(
        "its background work (room guess)",
        settle(&mut app),
        DESIGN_MS,
    );

    let mut screen = Screen::new();
    let (took, sent) = screen.frame(&app);
    within("first frame of the result screen", took, changed);
    eprintln!(
        "{:<44} {:7.2} MB",
        "  sent to the terminal",
        sent as f64 / 1e6
    );
    assert!(sent > 0, "the charts are sent");
    assert!(
        sent <= FRAME_BYTES,
        "{sent} bytes in one frame: are images compressed?"
    );
    let (took, sent) = screen.frame(&app);
    within("the same frame again", took, STEADY_FRAME_MS);
    assert_eq!(sent, 0, "an unchanged frame sends nothing");

    // The result screen's keys return at once; the design follows in the background, and
    // a quick run of presses is designed once at the end.
    let runs = app.designs_run;
    let mut slowest: f64 = 0.0;
    // Round the high limits and on to 300 Hz, with a tilt there and back on the way.
    for key in ['h', 'h', 't', 'T', 'h', 'h', 'h', 'h'] {
        slowest = slowest.max(press(&mut app, KeyCode::Char(key)));
    }
    within("slowest of eight result-screen keys", slowest, KEY_MS);
    within("the design after them", settle(&mut app), DESIGN_MS);
    assert!(
        app.designs_run - runs <= 2,
        "{} designs for eight quick presses",
        app.designs_run - runs
    );
    assert_eq!(
        app.design.as_ref().map(|d| d.options.high_limit_hz),
        Some(Some(300.0))
    );
    let (took, sent) = screen.frame(&app);
    within("frame with the new design", took, changed);
    assert!(sent > 0, "the new design reaches the screen");
    assert!(sent <= FRAME_BYTES, "{sent} bytes");

    // The room screen: guessed in the background since the review opened, so it opens at
    // once; the result screen under it is not drawn.
    within(
        "key r (room screen)",
        press(&mut app, KeyCode::Char('r')),
        KEY_MS,
    );
    assert_eq!(app.overlay, Some(Overlay::Room));
    app.tick();
    let (took, sent) = screen.frame(&app);
    within("first frame of the room screen", took, changed);
    assert!(sent <= FRAME_BYTES, "{sent} bytes");
    for (name, key) in [
        ("move a number (→)", KeyCode::Right),
        ("the next standing wave (])", KeyCode::Char(']')),
        ("where to sit (v)", KeyCode::Char('v')),
        ("the next room (g)", KeyCode::Char('g')),
    ] {
        let t = Instant::now();
        press(&mut app, key);
        app.tick(); // the room screen is worked out once per frame
        let (frame, _) = screen.frame(&app);
        within(
            &format!("{name}: key, work and frame"),
            ms(t),
            KEY_MS + changed,
        );
        assert!(frame <= changed, "{name}: frame {frame:.0} ms");
    }
    // A held key: twenty presses before one frame are worked out once.
    let t = Instant::now();
    for _ in 0..20 {
        press(&mut app, KeyCode::Right);
    }
    app.tick();
    screen.frame(&app);
    within("twenty presses of → and one frame", ms(t), KEY_MS + changed);
}
