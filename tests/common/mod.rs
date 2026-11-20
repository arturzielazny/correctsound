//! What the tests that time the app share: a yardstick for this machine, a measured session
//! of the simulated living room, and keys.

#![allow(dead_code, reason = "each test uses some of these")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use correctsound::app::keys::{KeyCode, KeyEvent, KeyModifiers};
use correctsound::app::{App, LevelPhase, Options, Step};
use correctsound::audio::sim::{SimConfig, SimState};
use correctsound::ui::raster::{Canvas, Pen};
use correctsound::ui::theme::Rgb;

/// A fixed piece of drawing, timed on this machine, ms (about 40 on a current desktop in the
/// test build): a chart-sized canvas filled, gridded and drawn with long curves, the kind of
/// work a changed frame does. The fastest of five runs, so a stall does not inflate it.
pub fn yardstick() -> f64 {
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

pub fn press(app: &mut App, code: KeyCode) -> f64 {
    let t = Instant::now();
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    ms(t)
}

pub fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

pub fn tick_until(app: &mut App, what: &str, timeout: Duration, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        app.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The background work a key started, timed until it is done.
pub fn settle(app: &mut App) -> f64 {
    let t = Instant::now();
    tick_until(app, "background work", Duration::from_secs(60), |a| {
        !a.working()
    });
    ms(t)
}

/// A measurement of the simulated living room from two mic positions, saved as a session.
///
/// `during_measure` runs on the measure step, before the first sweep.
pub fn measured_session(during_measure: impl FnOnce(&mut App)) -> PathBuf {
    let mut app = App::new(Options {
        simulate: true,
        sim_config: Some(SimConfig::living_room()),
        sim_state: Some(SimState::default()),
        ..Options::default()
    });
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Level, "{:?}", app.message);
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
    during_measure(&mut app);
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
pub fn within(what: &str, took: f64, budget: f64) {
    eprintln!("{what:<44} {took:7.1} ms   (budget {budget:.0})");
    assert!(took <= budget, "{what} took {took:.0} ms, over {budget:.0}");
}
