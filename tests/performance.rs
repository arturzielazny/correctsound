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

mod common;

use std::time::{Duration, Instant};

use common::{
    measured_session, median, ms, press, settle, tick_until, within, yardstick, yardstick_once,
};
use correctsound::app::keys::KeyCode;
use correctsound::app::{App, Options, Overlay, Step};
use correctsound::ui::graphics::Graphics;
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
/// A placement frame on fast sweeps, after the first: its curve and its waterfall drawn anew
/// for every update, five times a second, like a frame whose charts all changed (the median
/// of ten is about 4.5 yardsticks timed beside them; the fastest sweeps, gone in 0.4.0, drew
/// no waterfall and took 2.6).
const PLACEMENT_FRAMES: f64 = CHANGED_FRAMES;
/// Background work after a key: the new design (100–200 ms).
const DESIGN_MS: f64 = 4000.0;
/// Opening a review: every recording analysed again (about 0.5 s).
const REVIEW_MS: f64 = 8000.0;
/// What one frame may send to the terminal at most (about 1 MB; 48 MB uncompressed).
const FRAME_BYTES: usize = 6_000_000;

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

/// Placement on fast sweeps: every update's frame, drawn while the sweep loops.
///
/// These frames are drawn while the simulated room plays and every update is analysed, on
/// whatever else a shared CI machine is doing; one of ten can stall for reasons that are not
/// the app's. So each frame is followed by a run of the yardstick under the same conditions,
/// and the median frame is held to the median yardstick: a busy spell slows both alike, a
/// stall in one frame does not decide, and a real slowdown, which slows every frame, still
/// fails.
fn placement(app: &mut App) {
    let mut screen = Screen::new();
    press(app, KeyCode::Char('p'));
    press(app, KeyCode::Char('m'));
    let count = |a: &App| a.placement.as_ref().map_or(0, |p| p.count);
    tick_until(app, "fast sweeps", Duration::from_secs(20), |a| {
        count(a) >= 3
    });
    // The first frame after opening lays the screen out and sets its charts up; what
    // matters while watching is every frame after it.
    let _ = screen.frame(app);
    let (mut frames, mut units, mut most) = (Vec::new(), Vec::new(), 0usize);
    while frames.len() < 10 {
        let seen = count(app);
        tick_until(app, "an update", Duration::from_secs(10), |a| {
            count(a) > seen
        });
        let (took, sent) = screen.frame(app);
        frames.push(took);
        most = most.max(sent);
        units.push(yardstick_once());
    }
    let slowest = frames.iter().copied().fold(0.0, f64::max);
    let unit = median(units);
    eprintln!(
        "{:<44} {unit:7.1} ms",
        "  yardstick beside them (median of ten)"
    );
    eprintln!("{:<44} {slowest:7.1} ms", "  slowest of them (not judged)");
    within(
        "placement: median frame of ten updates",
        median(frames),
        PLACEMENT_FRAMES * unit,
    );
    eprintln!("{:<44} {:7.2} MB", "  most sent in one", most as f64 / 1e6);
    assert!(most <= FRAME_BYTES, "{most} bytes in one placement frame");
    press(app, KeyCode::Esc);
}

struct TempRoot(std::path::PathBuf);

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn reviewing_answers_at_once() {
    let root = std::env::temp_dir().join(format!("correctsound-perf-{}", std::process::id()));
    let _cleanup = TempRoot(root.clone());
    correctsound::paths::set_root(root);
    let unit = yardstick();
    eprintln!(
        "{:<44} {unit:7.1} ms",
        "yardstick (a fixed piece of drawing)"
    );
    let changed = CHANGED_FRAMES * unit;
    let session = measured_session(placement);

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
