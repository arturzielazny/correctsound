//! Situations from real use, played out on the simulator: a mic plugged in late, the wrong
//! output chosen, and a room of known size and layout that the room screen has to find.

use std::time::{Duration, Instant};

use correctsound::app::keys::{KeyCode, KeyEvent, KeyModifiers};
use correctsound::app::{App, LevelPhase, Options, Overlay, Step};
use correctsound::audio::sim::{SimConfig, SimState};
use correctsound::platform::sim::SINK;
use correctsound::ui::graphics::Graphics;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn tick_until(app: &mut App, what: &str, timeout: Duration, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        assert!(
            start.elapsed() < timeout,
            "timed out waiting for {what}: {app:?} {:?}",
            app.message
        );
        app.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Lets the app run for `time`, ticking as the event loop does.
fn run_for(app: &mut App, time: Duration) {
    let start = Instant::now();
    while start.elapsed() < time {
        app.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The screen drawn on a terminal `cols` × `rows` cells, as text.
fn render(app: &App, cols: u16, rows: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("test backend");
    terminal
        .draw(|f| correctsound::ui::draw(f, app, &mut Graphics::text_only()))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for (i, c) in buf.content().iter().enumerate() {
        out.push_str(c.symbol());
        if (i + 1) % usize::from(cols) == 0 {
            out.push('\n');
        }
    }
    out
}

fn app_with(config: SimConfig, state: SimState) -> App {
    // All tests in this binary share one process-wide test root (set_root uses OnceLock).
    let root = std::env::temp_dir().join(format!("correctsound-scenarios-{}", std::process::id()));
    correctsound::paths::set_root(root);
    App::new(Options {
        simulate: true,
        sim_config: Some(config),
        sim_state: Some(state),
        ..Options::default()
    })
}

/// From the devices step to the level step's noise playing from the left speaker, on
/// whatever output is selected.
fn to_level_noise(app: &mut App) {
    press(app, KeyCode::Enter);
    assert_eq!(app.step, Step::Level, "{:?}", app.message);
    tick_until(app, "a suggested level", Duration::from_secs(10), |a| {
        a.level.suggestion.is_some()
    });
    press(app, KeyCode::Enter);
    tick_until(
        app,
        "the left speaker heard",
        Duration::from_secs(10),
        |a| a.level.phase == LevelPhase::Playing && a.level.heard[0].is_some(),
    );
}

/// From the devices step through the level step to the measure step.
fn to_measure_step(app: &mut App) {
    to_level_noise(app);
    finish_level_step(app);
}

/// The right speaker's noise, then on to the measure step.
fn finish_level_step(app: &mut App) {
    press(app, KeyCode::Tab);
    tick_until(
        app,
        "the right speaker heard",
        Duration::from_secs(10),
        |a| a.level.heard[1].is_some(),
    );
    press(app, KeyCode::Enter);
    assert_eq!(app.step, Step::Measure);
}

/// Measures `positions` mic positions; the simulated mic moves 25 cm for each added one.
fn measure_positions(app: &mut App, positions: usize) {
    press(app, KeyCode::Enter);
    for n in 1..=positions {
        if n > 1 {
            press(app, KeyCode::Char('a'));
        }
        tick_until(app, "a mic position", Duration::from_secs(30), |a| {
            a.measure.positions.iter().all(|p| p.len() == n) && !a.measure.running()
        });
        assert!(app.measure.issue.is_none(), "{:?}", app.measure.issue);
    }
}

#[test]
fn the_chosen_output_stays_when_the_mic_is_plugged_in_late() {
    // As it happened: the laptop's own speakers are the system's default, the mic is not
    // plugged in yet, and the person picks the speakers while the app waits for it.
    let laptop = "alsa_output.laptop.speaker".to_owned();
    let state = SimState {
        mic_plugged: false,
        other_outputs: vec![(laptop.clone(), "Laptop speakers".to_owned())],
        default_output: Some(laptop.clone()),
        ..SimState::default()
    };
    let mut app = app_with(SimConfig::default(), state);
    assert_eq!(app.sink().map(|s| s.name.clone()), Some(laptop));
    assert!(render(&app, 150, 44).contains("Plug in the UMIK-1"));
    press(&mut app, KeyCode::Down);
    assert_eq!(app.sink().map(|s| s.name.as_str()), Some(SINK));
    // The app lists the devices every second while it waits: the choice must hold.
    run_for(&mut app, Duration::from_millis(2500));
    assert_eq!(app.sink().map(|s| s.name.as_str()), Some(SINK));
    app.sim_state()
        .expect("simulating")
        .lock()
        .expect("sim state")
        .mic_plugged = true;
    tick_until(&mut app, "the mic", Duration::from_secs(5), |a| {
        a.mic().is_some()
    });
    assert_eq!(
        app.sink().map(|s| s.name.as_str()),
        Some(SINK),
        "plugging the mic in moved the output"
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Level, "{:?}", app.message);
}

#[test]
fn measuring_the_wrong_output_is_caught() {
    // The laptop's speakers chosen by mistake: the mic hardly hears them, and the sweep
    // says so instead of making an EQ from the room's noise.
    let laptop = "alsa_output.laptop.speaker".to_owned();
    let state = SimState {
        other_outputs: vec![(laptop.clone(), "Laptop speakers".to_owned())],
        default_output: Some(laptop.clone()),
        ..SimState::default()
    };
    let mut app = app_with(SimConfig::default(), state);
    assert_eq!(app.sink().map(|s| s.name.clone()), Some(laptop));
    to_level_noise(&mut app);
    // The meter barely moved: far under the level the room calls for, and the level step
    // says why.
    let heard = app.level.heard[0].expect("a reading");
    assert!(heard < app.level.target_spl - 30.0, "heard {heard} dB");
    assert!(app.level.noise_unheard());
    let screen = render(&app, 150, 44);
    assert!(
        screen.contains("hardly hears the noise") && screen.contains("Laptop speakers"),
        "{screen}"
    );
    finish_level_step(&mut app);
    press(&mut app, KeyCode::Enter);
    tick_until(
        &mut app,
        "the sweep's verdict",
        Duration::from_secs(30),
        |a| a.measure.issue.is_some() || a.measure.positions.iter().all(|p| !p.is_empty()),
    );
    let issue = app.measure.issue.clone().expect("the sweep is rejected");
    eprintln!("wrong output: {issue}");
    assert!(app.measure.positions.iter().all(Vec::is_empty));
}

#[test]
fn the_room_screen_finds_the_simulated_room() {
    // A room of known size and layout: 5.64 × 5.24 m, the speakers in its right-hand corner.
    let config = SimConfig::living_room();
    let truth = config.scene.clone().expect("a room").setup;
    let mut app = app_with(config, SimState::default());
    to_measure_step(&mut app);
    measure_positions(&mut app, 3);
    // Each position heard the room differently: the mic really moved.
    let positions = &app.measure.responses[1]
        .as_ref()
        .expect("measured")
        .positions;
    assert_eq!(positions.len(), 3);

    // The longest-ringing standing wave, side to side at 32.7 Hz, rings 1.6 s in the
    // simulated room: the decay measurement finds it.
    let modes = app.modes(1);
    let side = modes
        .iter()
        .find(|m| (m.hz - 32.7).abs() < 1.5)
        .unwrap_or_else(|| panic!("the 33 Hz wave among {modes:?}"));
    eprintln!("33 Hz rings {:.2} s (simulated 1.6 s)", side.t60_s);
    assert!((1.1..=2.2).contains(&side.t60_s), "{side:?}");

    // The room screen guesses the room from the measurement alone.
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Result);
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(app.overlay, Some(Overlay::Room));
    let best = app.room.guesses.first().expect("a guess").setup;
    eprintln!(
        "guessed {:.2} × {:.2} × {:.2} m, speakers {:.2} and {:.2} m from the left wall",
        best.room.length, best.room.width, best.room.height, best.speakers[0].y, best.speakers[1].y
    );
    let near = |got: f64, want: f64| (got / want - 1.0).abs() < 0.04;
    assert!(
        near(best.room.length, truth.room.length) && near(best.room.width, truth.room.width),
        "guessed {:?}",
        best.room
    );
    let middle =
        |s: &correctsound::dsp::roommodel::Setup| (s.speakers[0].y + s.speakers[1].y) / 2.0;
    assert!(
        middle(&best) > best.room.width / 2.0,
        "the speakers are in the right-hand corner, not the left: {best:?}"
    );

    // Every screen draws at a small, a usual and an ultrawide terminal size.
    for (cols, rows) in [(80, 24), (150, 44), (320, 62)] {
        let screen = render(&app, cols, rows);
        assert!(screen.contains("Rooms"), "{cols}×{rows}:\n{screen}");
    }
    press(&mut app, KeyCode::Esc);
    for (cols, rows) in [(80, 24), (150, 44), (320, 62)] {
        let screen = render(&app, cols, rows);
        assert!(screen.contains("Tilt"), "{cols}×{rows}:\n{screen}");
    }
}
