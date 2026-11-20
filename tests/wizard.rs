//! Drives the whole wizard against the simulator, without a terminal.

use std::time::{Duration, Instant};

use correctsound::app::keys::{KeyCode, KeyEvent, KeyModifiers};
use correctsound::app::placement::{Mode, Reading};
use correctsound::app::{App, LevelPhase, Options, Overlay, Shown, Step, room};
use correctsound::eq::fit::AutoMethod;
use correctsound::eq::target::HIGH_LIMIT_PRESETS;
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

/// Lets background work (a design, the room guess) finish, as the event loop would.
fn settle(app: &mut App) {
    tick_until(app, "background work", Duration::from_secs(20), |a| {
        !a.working()
    });
}

/// How differently two bass readings are shaped from 40 to 250 Hz: the RMS of their
/// difference after removing the average offset, dB.
fn shape_difference(a: &Reading, b: &Reading) -> f64 {
    let diff: Vec<f64> = a
        .response
        .freqs
        .iter()
        .zip(&a.response.db)
        .filter(|(f, _)| (40.0..=250.0).contains(*f))
        .map(|(&f, &d)| d - b.response.at(f))
        .collect();
    let mean = diff.iter().sum::<f64>() / diff.len() as f64;
    (diff.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / diff.len() as f64).sqrt()
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
    assert_eq!(app.step, Step::Setup);
    // One screen: the mic, its calibration, its gain (off unity in the simulator) and the
    // outputs, and one Enter sets the gain and goes on.
    let setup = render(&app);
    assert!(setup.contains("Simulated speakers"), "{setup}");
    assert!(
        setup.contains("Calibration: the simulated mic's"),
        "{setup}"
    );
    assert!(!app.mic_gain.as_ref().expect("gain read").is_unity());
    assert!(
        setup.contains("Enter sets the mic gain and continues"),
        "{setup}"
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Level, "{:?}", app.message);
    assert!(app.mic_gain.as_ref().expect("gain read").is_unity());
    assert_eq!(app.message, None);

    // Listening to the room first, once EasyEffects is bypassed beside the screen: the
    // simulated room rumbles a little in the bass, so the suggested level is a little above
    // the floor and set by the 63 Hz octave.
    tick_until(&mut app, "listening", Duration::from_secs(2), |a| {
        a.level.phase == LevelPhase::Listening
    });
    assert!(!app.bypassing_eq());
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.level.phase, LevelPhase::Listening, "too early to play");
    tick_until(
        &mut app,
        "a suggested level",
        Duration::from_secs(10),
        |a| a.level.suggestion.is_some(),
    );
    let suggestion = app.level.suggestion.expect("suggested");
    eprintln!("suggested {suggestion:?}");
    assert!(
        (60.0..=70.0).contains(&suggestion.level_spl) && suggestion.limited_by_hz == 63.0,
        "{suggestion:?}"
    );
    let noise = app.level.room_noise.expect("room heard");
    assert!(noise[1] > noise[4] + 10.0, "rumble in the bass: {noise:?}");
    let screen = render(&app);
    assert!(
        screen.contains("Keep quiet") && screen.contains("Bass") && screen.contains("dB SPL"),
        "{screen}"
    );
    // It says it is live: the time keeps counting, a tick on each bar is the sound right now,
    // and the number says it follows the room.
    assert!(app.level.room_noise_now.is_some());
    assert!(
        screen.contains("live,") && screen.contains("┃") && screen.contains("follows the room"),
        "{screen}"
    );
    let before = app.level.listening_secs();
    tick_until(&mut app, "more listening", Duration::from_secs(5), |a| {
        a.level.listening_secs() >= before + 1.0
    });
    // Every row of the big number starts in the same column (wrapping once trimmed the
    // spaces off some rows, so a 7 looked like a T). Shown as 71, whose rows start with gaps.
    if let Some(s) = app.level.suggestion.as_mut() {
        s.level_spl = 71.0;
    }
    let screen = render(&app);
    app.level.suggestion = Some(suggestion);
    let big: Vec<String> = correctsound::ui::bignum::lines("71", ratatui::style::Style::default())
        .iter()
        .map(|l| l.spans[0].content.to_string())
        .collect();
    let rows: Vec<Vec<char>> = screen.lines().map(|l| l.chars().collect()).collect();
    let drawn_at = |row: &[char], text: &str| {
        let text: Vec<char> = text.chars().collect();
        (0..row.len().saturating_sub(text.len())).find(|&c| row[c..c + text.len()] == text[..])
    };
    let (top, col) = rows
        .iter()
        .enumerate()
        .find_map(|(i, r)| drawn_at(r, &big[0]).map(|c| (i, c)))
        .unwrap_or_else(|| panic!("big number on screen\n{screen}"));
    for (k, text) in big.iter().enumerate() {
        let row = &rows[top + k];
        let here: String = row[col..col + text.chars().count()].iter().collect();
        assert_eq!(
            &here, text,
            "row {k} of the big number is shifted\n{screen}"
        );
    }
    press(&mut app, KeyCode::Enter);
    assert!((app.level.target_spl - suggestion.level_spl).abs() < 3.0);
    tick_until(&mut app, "noise", Duration::from_secs(10), |a| {
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
    // Moving the speakers from 0.6 m to 1 m from the wall moves its cancellation dips
    // (343 / 4d: 143 → 86 Hz, with others above): the whole bass is shaped differently.
    // Compared by shape, not by where the deepest dip is, which can tip between two
    // similar dips.
    let moved = shape_difference(&now, &pinned);
    assert!(
        moved > 2.0,
        "bass changed by {moved:.2} dB: {:?} → {:?}",
        pinned.score,
        now.score
    );
    let screen = render(&app);
    assert!(
        screen.contains("Placement") && screen.contains("pinned") && screen.contains("fades"),
        "{screen}"
    );
    // Each sweep also measures how the bass decays.
    assert!(now.waterfall.is_some() && pinned.waterfall.is_some());

    // Fast sweeps: a sweep looping without pauses, a new reading every fifth of a second,
    // each with the decay.
    press(&mut app, KeyCode::Char('m'));
    let count = |a: &App| a.placement.as_ref().map_or(0, |p| p.count);
    let latest = |a: &App| {
        a.placement
            .as_ref()
            .and_then(|p| p.latest.clone())
            .expect("a reading")
    };
    let seen = count(&app);
    tick_until(&mut app, "fast sweeps", Duration::from_secs(30), |a| {
        count(a) >= seen + 5
    });
    let fast = latest(&app);
    assert_eq!(fast.mode, Mode::Fast);
    assert!(fast.waterfall.is_some());
    let (same, other) = (
        shape_difference(&fast, &now),
        shape_difference(&fast, &pinned),
    );
    eprintln!("fast at 1 m vs sweep at 1 m {same:.2} dB, vs sweep at 0.6 m {other:.2} dB");
    assert!(same < other / 4.0, "{same} vs {other}");
    // The big number.
    let screen = render(&app);
    assert!(
        screen.contains("Evenness") && screen.contains("█") && screen.contains("Fast sweeps"),
        "{screen}"
    );
    // Moving the wall while the sweep loops: the curve follows within about a second. Counted
    // in updates (0.2 s of sound each while the app keeps up, fewer when it lags behind the
    // simulator), so a slow test machine does not change the result.
    app.tick();
    for _ in 0..8 {
        press(&mut app, KeyCode::Left);
    }
    let moved_at = count(&app);
    tick_until(
        &mut app,
        "the fast curve to follow",
        Duration::from_secs(30),
        |a| {
            a.placement
                .as_ref()
                .and_then(|p| p.latest.as_ref())
                .is_some_and(|r| shape_difference(r, &pinned) < 1.0)
        },
    );
    let updates = count(&app) - moved_at;
    eprintln!("fast sweeps followed the move in {updates} updates");
    // One sweep (five updates) plus the one under way.
    assert!(updates <= 7, "{updates} updates");
    for _ in 0..8 {
        press(&mut app, KeyCode::Right);
    }

    // Two modes: m goes back to single sweeps.
    press(&mut app, KeyCode::Char('m'));
    assert_eq!(app.placement.as_ref().map(|p| p.mode), Some(Mode::Sweeps));
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
    let screen = render(&app);
    assert!(
        screen.contains("2 mic positions") && screen.contains("each one faint"),
        "{screen}"
    );
    // Each position is kept for the chart, behind the average.
    for r in app.measure.responses.iter().flatten() {
        assert_eq!(r.positions_display.len(), 2);
    }
    // Tab shows the left channel only, the right only, then both again.
    assert_eq!(app.shown, Shown::Both);
    assert!(
        screen.contains("Left") && screen.contains("Right"),
        "{screen}"
    );
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.shown, Shown::Left);
    let screen = render(&app);
    assert!(
        screen.contains("Left") && !screen.contains("Right"),
        "{screen}"
    );
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.shown, Shown::Right);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.shown, Shown::Both);

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.step, Step::Result);
    let design = app.design.clone().expect("designed");
    assert!(!design.channels[0].filters.is_empty());
    // Two positions: the filter count is cross-validated between them.
    assert_eq!(
        design.channels[0].auto.as_ref().map(|a| a.method),
        Some(AutoMethod::CrossValidated { positions: 2 })
    );
    // A key changes the options at once and the design follows in the background: the
    // screen keeps answering keys. A quick run of presses is designed once at the end, not
    // once per press.
    let runs = app.designs_run;
    press(&mut app, KeyCode::Char('h'));
    assert!(app.designing());
    for _ in 0..HIGH_LIMIT_PRESETS.len() {
        press(&mut app, KeyCode::Char('h'));
    }
    settle(&mut app);
    assert!(
        app.designs_run - runs <= 2,
        "{} designs for a run of presses",
        app.designs_run - runs
    );
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
    settle(&mut app);
    assert_eq!(
        app.design.as_ref().expect("designed").options.max_filters,
        Some(most - 1)
    );
    press(&mut app, KeyCode::Char('d'));
    // Both channels side by side, then each alone with Tab.
    let screen = render(&app);
    assert!(
        screen.contains("Left channel") && screen.contains("Right channel"),
        "{screen}"
    );
    press(&mut app, KeyCode::Tab);
    let screen = render(&app);
    assert!(
        screen.contains("Left channel") && !screen.contains("Right channel"),
        "{screen}"
    );
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.shown, Shown::Right);
    assert_eq!(app.view_channel, 1);
    let screen = render(&app);
    assert!(
        screen.contains("Right channel") && !screen.contains("Left channel"),
        "{screen}"
    );

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

    // The room screen: guessed from the measurement, then corrected with a tape measure.
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(app.overlay, Some(Overlay::Room));
    assert_eq!(app.room.selected, room::Selected::Guess(0));
    let guessed = app
        .room
        .setup()
        .expect("a room guessed from the measurement");
    assert!(guessed.room.is_valid(), "{guessed:?}");
    assert!(app.room.guesses.len() > 1);
    let screen = render(&app);
    assert!(
        screen.contains("Guess 1: change a number")
            && screen.contains("Rooms")
            && screen.contains("Standing waves")
            && screen.contains("What it means"),
        "{screen}"
    );
    // g and G go through the list.
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.room.selected, room::Selected::Guess(1));
    press(&mut app, KeyCode::Char('G'));
    assert_eq!(app.room.selected, room::Selected::Guess(0));
    // The first field is the room's length: typed in metres.
    for c in "5,64".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    // Changing a guess makes it a room of your own; the guess stays on the list.
    let entered = app.room.setup().expect("set");
    assert_eq!(entered.room.length, 5.64);
    assert_eq!(app.room.selected, room::Selected::Yours(0));
    assert_eq!(app.room.guesses[0].setup, guessed);
    let screen = render(&app);
    assert!(screen.contains("Your room 1, saved"), "{screen}");
    // ←/→ move the selected thing by 5 cm: the setup's distance from its side wall.
    for _ in 0..4 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.room.selected_field(), room::Field::SideWall);
    let before = app.room.setup().expect("set");
    press(&mut app, KeyCode::Right);
    let after = app.room.setup().expect("set");
    let apart = |s: &correctsound::dsp::roommodel::Setup| s.speakers[1].y - s.speakers[0].y;
    assert!(
        (apart(&after) - apart(&before)).abs() < 1e-9,
        "the speakers move together"
    );
    let (before, after) = (
        room::Field::SideWall.get(&before),
        room::Field::SideWall.get(&after),
    );
    assert_eq!(
        app.room.yours.len(),
        1,
        "the same room changed, not a new one"
    );
    assert!((after - before - 0.05).abs() < 0.011, "{before} → {after}");
    // Every standing wave can be shown, and where to sit instead.
    app.tick(); // the room screen is worked out once per frame
    let derived = app.room.current().expect("the model's view");
    assert!(!derived.listed.is_empty());
    assert!(derived.agreement_db.iter().all(Option::is_some));
    press(&mut app, KeyCode::Char(']'));
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.room.view, room::PlanView::Seats);
    let screen = render(&app);
    assert!(
        screen.contains("Where to sit") && screen.contains("v standing waves"),
        "{screen}"
    );
    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.room.view, room::PlanView::Mode);
    // Your rooms can be deleted, with a second press.
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(app.room.yours.len(), 1);
    press(&mut app, KeyCode::Char('x'));
    assert!(app.room.yours.is_empty());
    press(&mut app, KeyCode::Esc);
    assert!(app.overlay.is_none());

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
    // Tab here too, from the right channel alone back to both.
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.shown, Shown::Both);
    let left = std::fs::read_to_string(&app.save.written[0]).expect("left.txt");
    assert!(left.starts_with("Preamp: "));
    let session = app.session_dir().expect("session");
    assert!(session.join("left-1-capture.wav").exists());
    assert!(session.join("session.toml").exists());

    // The screen says what can be done: install, verify, then keep it or roll back.
    let screen = render(&app);
    assert!(
        screen.contains("a install")
            && screen.contains("v verify")
            && screen.contains("u roll back"),
        "{screen}"
    );
    let sim_eq = |app: &App| {
        app.sim_state()
            .expect("simulated")
            .lock()
            .expect("sim state")
            .eq
            .is_some()
    };
    assert!(!sim_eq(&app));
    press(&mut app, KeyCode::Char('a'));
    assert!(app.save.applied().is_some(), "{:?}", app.save.error);
    assert!(sim_eq(&app));
    assert!(render(&app).contains("put the mic back where the first measurement was"));
    press(&mut app, KeyCode::Char('v'));
    tick_until(&mut app, "verification", Duration::from_secs(20), |a| {
        a.save.verify().is_some()
    });
    // Verified at the first mic position (the screen asks for the mic there), against what
    // the EQ predicts for that position: they match closely.
    let v = app.save.verify().expect("verified");
    assert!(
        v.iter().all(|r| r.rms_db < 0.5 && r.worst_db.abs() < 1.5),
        "verification {v:?}"
    );
    assert!(render(&app).contains("Verification"));

    // Rolled back: the EQ host is as it was before, and the EQ can be installed again.
    press(&mut app, KeyCode::Char('u'));
    assert!(app.save.applied().is_none(), "{:?}", app.save.error);
    assert!(!sim_eq(&app));
    assert!(app.save.verify().is_none());
    let screen = render(&app);
    assert!(
        screen.contains("Rolled back") && screen.contains("installs it again"),
        "{screen}"
    );
    let summary = app.exit_summary().join("\n");
    assert!(
        summary.contains("as it was before you installed the EQ")
            && summary.contains("Your measurement is saved in")
            && summary.contains("correctsound review"),
        "{summary}"
    );
    press(&mut app, KeyCode::Char('u'));
    assert!(
        app.message
            .as_deref()
            .is_some_and(|m| m.contains("Nothing to roll back"))
    );
    press(&mut app, KeyCode::Char('a'));
    assert!(app.save.applied().is_some(), "{:?}", app.save.error);
    assert!(sim_eq(&app));
    assert!(app.save.rolled_back().is_none());
    // On the way out it says the EQ stays installed, and where the measurement is.
    let summary = app.exit_summary();
    assert!(
        summary[0].starts_with("The EQ stays installed")
            && summary[0].contains("backed up in")
            && summary[1].contains(&session.display().to_string()),
        "{summary:?}"
    );

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
