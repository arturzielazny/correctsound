//! `correctsound`: measure speakers with a measurement mic and generate room-correction EQ.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use crossterm::event::{self, Event, KeyEventKind};

use correctsound::app::{App, Options};
use correctsound::audio::Channel;
use correctsound::dsp::level::SplScale;
use correctsound::dsp::sweep::{IrWindow, Sweep, SweepAnalysis, SweepParams};
use correctsound::eq::export;
use correctsound::eq::fit::design_with_positions;
use correctsound::eq::target::DesignOptions;
use correctsound::insights;
use correctsound::session::{Session, read_wav};
use correctsound::ui::graphics::Graphics;
use correctsound::{analysis, cal, platform, restore};

/// Room correction with a miniDSP UMIK-1: PipeWire + EasyEffects on Linux.
///
/// Run without arguments for the step-by-step wizard.
#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Run against a simulated room instead of real hardware.
    #[arg(long)]
    simulate: bool,
    /// Use this calibration file instead of downloading one.
    #[arg(long, value_name = "FILE")]
    cal_file: Option<PathBuf>,
    /// Use this input (its name, e.g. a PipeWire node name) instead of detecting a UMIK-1.
    #[arg(long, value_name = "NODE")]
    source: Option<String>,
    /// Preselect this output sink (node name).
    #[arg(long, value_name = "NODE")]
    sink: Option<String>,
    /// dB added to mic dBFS readings to get dB SPL (default: 124 − sensitivity factor).
    #[arg(long, value_name = "DB", allow_hyphen_values = true)]
    spl_offset: Option<f64>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show the mic, outputs and EasyEffects state that preflight sees.
    Devices,
    /// Print the room's sound level from the mic for a few seconds (plays nothing).
    Meter {
        /// How long to measure.
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// List saved sessions, newest first, numbered for `review`.
    Sessions,
    /// Open a saved session in the app: result, curiosities, apply. Nothing is measured.
    Review {
        /// Session directory or its number in `sessions` (default: the newest).
        session: Option<String>,
    },
    /// Redo the EQ design from a saved session with different options.
    Fit {
        /// Session directory (printed when the wizard exits).
        session: PathBuf,
        /// Target slope in dB/octave (0 = flat).
        #[arg(long, default_value_t = -0.5, allow_hyphen_values = true)]
        tilt: f64,
        /// Highest corrected frequency in Hz (default: each speaker's treble limit).
        #[arg(long)]
        high_limit: Option<f64>,
        /// Filters per channel, 1–20 (default: chosen per channel).
        #[arg(long)]
        filters: Option<usize>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Devices) => devices(),
        Some(Command::Sessions) => {
            sessions();
            Ok(())
        }
        Some(Command::Review { session }) => {
            let dir = pick_session(session.as_deref())?;
            wizard(Options {
                cal_file: cli.cal_file,
                review: Some(dir),
                ..Options::default()
            })
        }
        Some(Command::Meter { seconds }) => meter(seconds, cli.cal_file.as_deref(), cli.spl_offset),
        Some(Command::Fit {
            session,
            tilt,
            high_limit,
            filters,
        }) => fit(&session, tilt, high_limit, filters, cli.cal_file.as_deref()),
        None => wizard(Options {
            simulate: cli.simulate,
            cal_file: cli.cal_file,
            source: cli.source,
            spl_offset: cli.spl_offset,
            sink: cli.sink,
            review: None,
            ..Options::default()
        }),
    }
}

/// Prints the saved sessions with a number for `review`.
fn sessions() {
    let list = Session::list();
    if list.is_empty() {
        println!("No saved sessions yet.");
        return;
    }
    for (i, (dir, info)) in list.iter().enumerate() {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!(
            "{:>3}  {name}  {} position{} per channel{}{}",
            i + 1,
            info.positions[0].max(info.positions[1]),
            if info.positions[0].max(info.positions[1]) == 1 {
                ""
            } else {
                "s"
            },
            if dir.join("CorrectSound.json").exists() {
                ", EQ saved"
            } else {
                ""
            },
            if info.simulated { ", simulated" } else { "" },
        );
    }
    println!("\ncorrectsound review N   opens one (no number: the newest)");
}

/// A session directory from a path, a number from `sessions`, or the newest.
fn pick_session(arg: Option<&str>) -> Result<PathBuf> {
    if let Some(a) = arg {
        let path = PathBuf::from(a);
        if path.is_dir() {
            return Ok(path);
        }
        if let Ok(n) = a.parse::<usize>() {
            return Session::list()
                .into_iter()
                .nth(n.saturating_sub(1))
                .map(|(dir, _)| dir)
                .with_context(|| format!("no session number {n}; see correctsound sessions"));
        }
        bail!("{a} is neither a session directory nor a number from correctsound sessions");
    }
    Session::list()
        .into_iter()
        .next()
        .map(|(dir, _)| dir)
        .context("no saved sessions yet")
}

fn wizard(options: Options) -> Result<()> {
    // Restore the system on SIGTERM / SIGHUP / SIGINT (raw mode turns Ctrl-C into a key).
    let signalled = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    let signals = [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ];
    #[cfg(not(unix))]
    let signals = [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT];
    for sig in signals {
        signal_hook::flag::register(sig, Arc::clone(&signalled))
            .context("installing signal handler")?;
    }
    spawn_watchdog(Arc::clone(&signalled));
    let mut terminal = ratatui::init();
    // Charts as images where the terminal can show them (asks the terminal, in raw mode,
    // before the loop below starts reading keys).
    let mut gfx = Graphics::detect();
    // ratatui's hook restores the terminal; ours also restores the system first.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore::run_all();
        previous(info);
    }));

    let mut app = App::new(options);
    let result = (|| -> Result<()> {
        while !app.quit && !signalled.load(Ordering::Relaxed) {
            gfx.refresh_cell_size();
            gfx.begin_frame();
            terminal.draw(|f| correctsound::ui::draw(f, &app, &mut gfx))?;
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                app.handle_key(key);
            }
            app.tick();
        }
        Ok(())
    })();
    let session = app.session_dir();
    let undo = restore::pending();
    drop(app); // stops any audio
    restore::run_all();
    ratatui::restore();
    if !undo.is_empty() {
        println!("Restored: {}", undo.join(", "));
    }
    if let Some(dir) = session {
        println!("Session saved in {}", dir.display());
    }
    result
}

/// How often the watchdog looks at the terminal and the signal flag.
const WATCHDOG_PERIOD: Duration = Duration::from_millis(200);
/// After a signal, how long the main loop gets to exit on its own (it prints the session).
const SIGNAL_GRACE: Duration = Duration::from_secs(1);

/// Ends the program, restoring the system first, when its terminal goes away or a signal
/// is not acted on. Without it a closed terminal left the wizard spinning at full CPU with
/// the system unrestored: crossterm's `event::read` retries the dead terminal's
/// end-of-input forever, so the main loop never got to check the signal flag.
fn spawn_watchdog(signalled: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut signal_seen: Option<std::time::Instant> = None;
        loop {
            std::thread::sleep(WATCHDOG_PERIOD);
            let terminal_gone = terminal_gone();
            if !terminal_gone {
                if !signalled.load(Ordering::Relaxed) {
                    continue;
                }
                let since = *signal_seen.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() < SIGNAL_GRACE {
                    continue;
                }
            }
            restore::run_all();
            if !terminal_gone {
                ratatui::restore();
            }
            // Children (pw-cat) end on their own: playback at the end of its input, capture
            // on its next write to the closed pipe.
            std::process::exit(1);
        }
    });
}

/// True once the controlling terminal has gone away (its window was closed).
#[cfg(unix)]
fn terminal_gone() -> bool {
    // Opening the controlling terminal fails once it has hung up.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .is_err()
}

/// True once the controlling terminal has gone away. Without a cheap check on this
/// platform, a closed console ends the process through the signal handlers instead.
#[cfg(not(unix))]
fn terminal_gone() -> bool {
    false
}

fn devices() -> Result<()> {
    let mut sys = platform::native();
    println!("Platform: {}", sys.name());
    let missing = sys.missing();
    if !missing.is_empty() {
        println!("Missing: {}", missing.join(", "));
    }
    let d = sys.devices()?;
    match &d.mic {
        Some(m) => {
            let gain = sys.input_gain(m)?;
            println!("Mic:      {} ({})", m.description, m.name);
            for s in &gain.settings {
                println!(
                    "          {} {}{}",
                    s.label,
                    s.value,
                    if s.at_unity { "" } else { "  (not unity)" }
                );
            }
        }
        None => println!("Mic:      no UMIK-1 found"),
    }
    println!("Outputs:");
    for s in &d.sinks {
        let mark = if d.preferred_sink.as_deref() == Some(s.name.as_str()) {
            "*"
        } else {
            " "
        };
        let vol = sys
            .output_volume(s)
            .map_or("?".to_owned(), |v| format!("{v:.2}"));
        println!(
            "  {mark} {} [{}] volume {vol}\n      {}",
            s.description,
            if s.stereo { "stereo" } else { "mono" },
            s.name
        );
    }
    match (&d.eq_host, sys.eq_bypass()?) {
        (Some(h), Some(b)) => println!(
            "{}: running, global bypass {}",
            h.name,
            if b { "ON" } else { "off" }
        ),
        (Some(h), None) => println!("{}: not running", h.name),
        (None, _) => println!("EQ host: none supported on this platform"),
    }
    if !d.other_streams.is_empty() {
        println!("Other output streams: {}", d.other_streams.join(", "));
    }
    Ok(())
}

fn meter(seconds: u64, cal_file: Option<&std::path::Path>, spl_offset: Option<f64>) -> Result<()> {
    use correctsound::audio::{Program, Route, Source};
    use correctsound::dsp::level::{LevelMeter, rms_dbfs};
    let mut sys = platform::native();
    let d = sys.devices()?;
    let mic = d.mic.context("no UMIK-1 found")?;
    let sens = match cal_file {
        Some(p) => cal::load(p)?.sens_factor,
        None => {
            println!("(no --cal-file: assuming sensitivity factor 0 dB)");
            0.0
        }
    };
    let scale = spl_offset.map_or_else(
        || SplScale::from_sens_factor(sens),
        |o| SplScale { offset_db: o },
    );
    let sink = d.sinks.first().cloned().context("no output device found")?;
    let mut audio = sys.audio(&mic, &sink);
    let stream = audio.start(
        Source::new(Program::Silence, Channel::Left),
        Route::Hardware,
    )?;
    let mut meter = LevelMeter::standard(correctsound::dsp::SAMPLE_RATE);
    let mut total = 0usize;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_millis(500));
        let block = stream.drain();
        total += block.len();
        meter.push(&block);
        if let Some(db) = meter.dbfs() {
            println!(
                "{:5.1} s  {:6.1} dB SPL  (last 0.5 s: {:6.1} dBFS)",
                start.elapsed().as_secs_f64(),
                scale.spl(db),
                rms_dbfs(&block)
            );
        }
    }
    let rate = total as f64 / start.elapsed().as_secs_f64();
    println!("Captured {total} samples ≈ {rate:.0} samples/s");
    Ok(())
}

fn fit(
    dir: &std::path::Path,
    tilt: f64,
    high_limit: Option<f64>,
    filters: Option<usize>,
    cal_override: Option<&std::path::Path>,
) -> Result<()> {
    if filters.is_some_and(|n| !(1..=20).contains(&n)) {
        bail!("--filters must be between 1 and 20");
    }
    let session = Session::open(dir);
    let info = session.load_info()?;
    let cal = match (cal_override, &info.calibration_file) {
        (Some(p), _) => cal::load(p)?,
        (None, Some(p)) => cal::load(p)?,
        (None, None) if info.simulated => {
            correctsound::audio::sim::SimConfig::default().calibration()
        }
        (None, None) => bail!("the session has no calibration file; pass --cal-file"),
    };
    let scale = SplScale {
        offset_db: info.spl_offset_db,
    };
    let sweep = Sweep::new(SweepParams::default());
    let measured = Channel::BOTH.map(|c| -> Result<(analysis::Responses, Vec<SweepAnalysis>)> {
        let mut positions = Vec::new();
        for p in 0..info.positions[c.index()] {
            let rec = read_wav(&session.capture_path(c, p))?;
            positions.push(
                SweepAnalysis::analyze(&sweep, &rec, IrWindow::default())
                    .with_context(|| format!("{} position {}", c.name(), p + 1))?,
            );
        }
        if positions.is_empty() {
            bail!("no {} measurements in the session", c.name());
        }
        let r = analysis::responses(&positions, &cal, scale, info.sweep_dbfs);
        Ok((r, positions))
    });
    let [l, r] = measured;
    let ((l, l_runs), (r, r_runs)) = (l?, r?);
    let options = DesignOptions {
        tilt_db_per_oct: tilt,
        high_limit_hz: high_limit,
        max_filters: filters,
    };
    let d = design_with_positions([&l.fit, &r.fit], [&l.positions, &r.positions], &options);
    let suffix = format!(
        "tilt{tilt:+.2}_hi{}_n{}",
        high_limit.map_or_else(|| "auto".to_owned(), |h| format!("{h:.0}")),
        filters.map_or_else(|| "auto".to_owned(), |n| n.to_string())
    );
    session.write(&format!("left-{suffix}.txt"), &export::apo_text(&d.eq, 0))?;
    session.write(&format!("right-{suffix}.txt"), &export::apo_text(&d.eq, 1))?;
    session.write(
        &format!("CorrectSound-{suffix}.json"),
        &serde_json::to_string_pretty(&export::easyeffects_preset(&d.eq))?,
    )?;
    let curiosities = insights::insights(&insights::Inputs {
        responses: [&l, &r],
        analyses: [&l_runs, &r_runs],
        design: &d,
        ambient_spl: info.ambient_spl,
        level_spl: info.room_noise_spl.as_ref().and(info.level_spl),
        needed_spl: info
            .room_noise_spl
            .as_ref()
            .and(info.needed_spl.or(info.level_spl)),
        bass_noise_spl: info.room_noise_spl.as_ref().and_then(|n| n.get(1).copied()),
    });
    let report = format!(
        "{}\n{}",
        export::report(&d, &format!("CorrectSound refit of {}", dir.display())),
        insights::text(&curiosities)
    );
    session.write(&format!("report-{suffix}.txt"), &report)?;
    println!("{report}");
    println!(
        "Wrote left-/right-{suffix}.txt and CorrectSound-{suffix}.json in {}",
        dir.display()
    );
    Ok(())
}
