//! Drives the whole wizard against the simulator, without a terminal.

use std::time::{Duration, Instant};

use correctsound::app::{App, LevelPhase, Options, Step};
use correctsound::eq::fit::AutoMethod;
use correctsound::ui::graphics::Graphics;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(140, 45)).expect("test backend");
    terminal
        .draw(|f| correctsound::ui::draw(f, app, &mut Graphics::text_only()))
        .expect("draw");
    let buf = terminal.backend().buffer();
    let width = buf.area.width as usize;
    let mut out = String::new();
    for (i, c) in buf.content().iter().enumerate() {
        out.push_str(c.symbol());
        if (i + 1) % width == 0 {
            out.push('\n');
        }
    }
    if std::env::var_os("SHOW_SCREENS").is_some() {
        println!("{out}");
    }
    out
}

#[test]
fn full_wizard_on_simulator() {
    let root = std::env::temp_dir().join(format!("correctsound-test-{}", std::process::id()));
    correctsound::paths::set_root(root.clone());
    let mut app = App::new(Options {
        simulate: true,
        ..Options::default()
    });
    assert_eq!(app.step, Step::Preflight);
    assert!(render(&app).contains("Simulated speakers"));

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::MicGain, "{:?}", app.message);
    assert!(!app.mic_gain.expect("gain read").is_unity());
    assert!(render(&app).contains("set these to unity"));
    press(&mut app, KeyCode::Enter);
    assert!(app.mic_gain.expect("gain read").is_unity());
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Level);

    tick_until(&mut app, "ambient + noise", Duration::from_secs(10), |a| {
        a.level.phase == LevelPhase::Playing && a.level.heard[0].is_some()
    });
    let ambient = app.level.ambient_spl.expect("ambient measured");
    assert!(ambient < 40.0, "ambient {ambient}");
    let before = app.level.spl.expect("spl");
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    std::thread::sleep(Duration::from_millis(1500));
    app.tick();
    let after = app.level.spl.expect("spl");
    assert!(
        after > before + 0.5,
        "volume up raised SPL: {before} → {after}"
    );
    assert!(render(&app).contains("dB SPL"));
    press(&mut app, KeyCode::Tab);
    tick_until(
        &mut app,
        "right channel heard",
        Duration::from_secs(10),
        |a| a.level.heard[1].is_some(),
    );

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Measure);
    // Live placement, before measuring so every measurement has the moved speakers: sweeps
    // repeat, and moving the (simulated) speakers changes the bass.
    press(&mut app, KeyCode::Char('p'));
    assert!(app.placement.is_some(), "{:?}", app.measure.issue);
    tick_until(
        &mut app,
        "a placement reading",
        Duration::from_secs(20),
        |a| a.placement.as_ref().is_some_and(|p| p.count >= 1),
    );
    press(&mut app, KeyCode::Char(' '));
    let pinned = app
        .placement
        .as_ref()
        .and_then(|p| p.reference.clone())
        .expect("pinned");
    for _ in 0..8 {
        press(&mut app, KeyCode::Right);
    }
    assert!((app.sim_wall_m().expect("simulated wall") - 1.0).abs() < 1e-9);
    // The sweep under way started before the move; wait for one after it.
    let seen = app.placement.as_ref().map_or(0, |p| p.count);
    tick_until(
        &mut app,
        "a reading after moving",
        Duration::from_secs(20),
        |a| a.placement.as_ref().is_some_and(|p| p.count >= seen + 2),
    );
    let now = app
        .placement
        .as_ref()
        .and_then(|p| p.latest.clone())
        .expect("reading");
    // A wall 1 m behind cancels near 343 / 4 = 86 Hz; at 0.6 m it was near 143 Hz.
    assert!(
        (now.score.deepest.0 - pinned.score.deepest.0).abs() > 20.0,
        "dip moved: {:?} → {:?}",
        pinned.score,
        now.score
    );
    let screen = render(&app);
    assert!(
        screen.contains("Placement") && screen.contains("pinned"),
        "{screen}"
    );
    press(&mut app, KeyCode::Esc);
    assert!(app.placement.is_none());
    assert_eq!(app.step, Step::Measure);

    press(&mut app, KeyCode::Enter);
    assert!(app.measure.running());
    tick_until(&mut app, "first position", Duration::from_secs(20), |a| {
        a.measure.positions.iter().all(|p| p.len() == 1) && !a.measure.running()
    });
    assert!(app.measure.issue.is_none(), "{:?}", app.measure.issue);

    press(&mut app, KeyCode::Char('a'));
    tick_until(&mut app, "second position", Duration::from_secs(20), |a| {
        a.measure.positions.iter().all(|p| p.len() == 2) && !a.measure.running()
    });
    assert!(render(&app).contains("2 mic positions"));

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Result);
    let design = app.design.clone().expect("designed");
    assert!(!design.channels[0].filters.is_empty());
    // Two positions: the filter count is cross-validated between them.
    assert_eq!(
        design.channels[0].auto.as_ref().map(|a| a.method),
        Some(AutoMethod::CrossValidated { positions: 2 })
    );
    press(&mut app, KeyCode::Char('h'));
    assert_eq!(
        app.design.as_ref().expect("designed").options.high_limit_hz,
        Some(300.0)
    );
    // f/F step from what the automatic choice picked for the current settings.
    let most = app
        .design
        .as_ref()
        .expect("designed")
        .channels
        .iter()
        .map(|c| c.filters.len())
        .max()
        .expect("two");
    press(&mut app, KeyCode::Char('F'));
    assert_eq!(
        app.design.as_ref().expect("designed").options.max_filters,
        Some(most - 1)
    );
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.view_channel, 1);
    assert!(render(&app).contains("Right channel"));

    // The decay view opens on the shown channel and switches with Tab.
    press(&mut app, KeyCode::Char('w'));
    let screen = render(&app);
    assert!(screen.contains("how the bass fades"), "{screen}");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.view_channel, 0);
    assert!(render(&app).contains("Left speaker"));
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none());
    assert!(
        app.curiosities
            .iter()
            .flat_map(|s| &s.lines)
            .any(|l| l.contains("ring")),
        "{:?}",
        app.curiosities
    );

    // Back and forth keeps the measurements.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.step, Step::Measure);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Result);

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Save);
    assert_eq!(app.save.written.len(), 4, "{:?}", app.save.error);
    for p in &app.save.written {
        assert!(p.exists(), "{}", p.display());
    }
    let left = std::fs::read_to_string(&app.save.written[0]).expect("left.txt");
    assert!(left.starts_with("Preamp: "));
    let session = app.session_dir().expect("session");
    assert!(session.join("left-1-capture.wav").exists());
    assert!(session.join("session.toml").exists());

    press(&mut app, KeyCode::Char('a'));
    assert!(app.save.applied.is_some(), "{:?}", app.save.error);
    press(&mut app, KeyCode::Char('v'));
    tick_until(&mut app, "verification", Duration::from_secs(20), |a| {
        a.save.verify.is_some()
    });
    let v = app.save.verify.expect("verified");
    assert!(
        v.iter().all(|r| r.rms_db < 0.5 && r.worst_db.abs() < 1.5),
        "verification {v:?}"
    );
    assert!(render(&app).contains("Verification"));

    press(&mut app, KeyCode::Enter);
    assert!(app.quit);

    // The saved session can be refitted from the command line.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_correctsound"))
        .args([
            "fit",
            &session.to_string_lossy(),
            "--tilt",
            "-1",
            "--filters",
            "6",
        ])
        .output()
        .expect("run correctsound fit");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(session.join("left-tilt-1.00_hiauto_n6.txt").exists());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Target tilt -1.0 dB/oct"));

    // The saved session can be reviewed: re-analysed from its recordings, nothing played.
    let mut review = App::new(Options {
        simulate: true,
        review: Some(session.clone()),
        ..Options::default()
    });
    assert_eq!(review.step, Step::Result, "{:?}", review.message);
    assert!(review.reviewing());
    let reviewed = review
        .design
        .as_ref()
        .expect("designed from the recordings");
    assert_eq!(
        reviewed.channels[0].measured.db.len(),
        design.channels[0].measured.db.len()
    );
    assert!(!review.curiosities.is_empty());
    let screen = render(&review);
    assert!(screen.contains("REVIEW"), "{screen}");
    // No way back into measuring.
    press(&mut review, KeyCode::Esc);
    assert_eq!(review.step, Step::Result);
    // The info and curiosities screens open and close.
    press(&mut review, KeyCode::Char('c'));
    assert!(render(&review).contains("Curiosities"));
    press(&mut review, KeyCode::Char('i'));
    press(&mut review, KeyCode::Char('m'));
    assert!(render(&review).contains("technical"));
    press(&mut review, KeyCode::Esc);
    assert!(review.overlay.is_none());
    let _ = std::fs::remove_dir_all(root);
}
