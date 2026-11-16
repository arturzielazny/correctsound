# Notes for AI coding agents (and the people driving them)

CorrectSound measures speakers with a measurement mic and designs room-correction EQ. It
was written by an AI agent and is meant to be extended by others. Read this file before
changing anything, then README.md, CONTRIBUTING.md and IDEAS.md.

## The one rule for ports: extend, don't rewrite

There is **one** CorrectSound for every operating system and distribution. A port adds a
platform; it never turns the app into a version that only works on yours.

- Do not delete, stub out or "simplify away" another platform's code, and do not change
  how Linux behaves to make your platform easier.
- Do not copy the app into an OS-specific fork inside the repository, and do not replace
  shared code with OS-specific code. If shared code needs something new from the OS, add
  a neutral method to the platform interface (below) and implement it everywhere.
- CI builds, lints and tests every change on Linux, macOS and Windows. A pull request that
  breaks any of them, or the simulator, cannot be merged.

## How the code is laid out

```text
src/platform/          the only place that knows about the operating system
  mod.rs               System trait, neutral types (Device, Devices, InputGain, EqHost,
                       Routing, Applied), native() which picks the platform, Unsupported
  linux/               PipeWire (pw-cat, pw-dump, wpctl), ALSA (amixer), EasyEffects
  sim.rs               the simulated platform: tests and --simulate, on every OS
src/audio/             AudioBackend trait (play a Source, record the mic), the room simulator
src/dsp, src/eq        signal processing and EQ design: pure Rust, no I/O
src/analysis.rs, src/insights.rs, src/cal.rs, src/session.rs
src/app/               the wizard's state machine
src/ui/                drawing (ratatui; charts as images or braille)
src/restore.rs         undo steps run on exit, errors, panics and signals
src/paths.rs           where files go (XDG layout under the home directory, every OS)
```

Everything outside `src/platform/` must stay OS-neutral: no `#[cfg(target_os)]`, no names
of OS tools or settings, no fields that only mean something on one OS. The only
exceptions are small, documented ones in `src/main.rs` (signals, detecting a closed
terminal) and `src/ui/raster.rs` (font locations); keep them that small.

The two interfaces a platform implements:

- `platform::System`: list the mic and outputs, read and set the mic's input gain and the
  output volume, find and control the EQ host (the program that applies the EQ to all
  playback: EasyEffects on Linux), check where playback is going, and install the EQ.
- `audio::AudioBackend`: play a `Source` on one channel of the chosen output at 48 kHz
  while recording the mic, directly (`Route::Hardware`) or through the EQ host
  (`Route::ThroughEq`, for verification).

Words the user sees about the OS come from the platform too: `GainSetting` labels,
`EqHost::name`, `System::input_gain_help`, `Routing::Wrong` messages.

## Implementation notes worth knowing

- On Linux, audio runs through `pw-cat` subprocesses instead of the `pipewire` crate. Both
  streams set `node.dont-move`; without it, EasyEffects re-routes any new output stream into
  its own sink, and the measurement would include the existing EQ. The playback stream's links
  are checked during every sweep, and the sweep is abandoned if it reaches anything except the
  chosen output.
- The UMIK-1's 0° calibration URL is `…/umikcal/umik.php/<serial>.txt` (not `umik00.php`); an
  unknown serial returns HTTP 200 with an HTML error, which is detected.
- SPL = dBFS + 124 − sensitivity factor (matches REW on a current UMIK-1). Older 18 dB-gain
  mics are marked approximate. The EQ only depends on the response's shape, not this constant.
- Filters use EasyEffects' `APO (DR)` bell mode, the RBJ cookbook filter the fitter models, so
  the prediction matches what EasyEffects does.

## Adding a platform

1. Read `src/platform/mod.rs`, `src/platform/linux/` and `src/platform/sim.rs`.
2. Add `src/platform/<os>/mod.rs` with a type implementing `System`, and an
   `AudioBackend`. Select it in `platform::native()` with `#[cfg(target_os = "<os>")]`,
   and gate the module the same way. Put OS-only dependencies under
   `[target.'cfg(target_os = "<os>")'.dependencies]` in Cargo.toml.
3. Register an undo step with `restore::register` (a closure) or
   `restore::register_command` before every change you make to the system.
4. Where your OS lacks something, degrade instead of failing: no EQ host means
   `eq_host: None` (the user still gets the EQ files); no way to reroute playback means
   `check_routing` answers `Routing::Ok`.
5. Test what can be tested without hardware: parse captured output of your OS's tools
   from files in `tests/data/` (scrubbed of host names, user names and serials), like
   `platform/linux/pipewire.rs` does with `pw-dump.json`.
6. Document it: a section in README.md (requirements, what works, what doesn't), and the
   install notes on the website (`docs/index.html`).
7. One platform per pull request. Say which hardware you tested on, or that you only ran
   the simulator.

## Write for many systems, not just yours

- Use the OS's standard interfaces (PipeWire, CoreAudio, WASAPI) rather than one
  desktop's or one distribution's tools. Detect what is installed (`System::missing`)
  instead of assuming it, and name the missing package in plain words.
- Linux is many distributions. Do not hard-code Arch package names, Omarchy paths or a
  particular desktop; anything like that must be optional with a fallback, as the
  Omarchy theme colours are (`src/ui/theme.rs`).
- Terminals differ: charts are images where the terminal supports the kitty or sixel
  protocol and braille elsewhere. Do not make the app depend on one terminal.
- Keep files where `src/paths.rs` puts them, so sessions look the same everywhere.

## Safety rules (for agents with a terminal)

- Never play sound on real speakers without asking the person first and waiting for the
  answer. Develop and test against the simulator (`--simulate`, `src/audio/sim.rs`).
- Back up another program's settings before overwriting them, and restore whatever you
  change when the app exits.
- Never commit personal data: real calibration files (they carry the mic's serial),
  unscrubbed device dumps (host and user names), recordings of a room, anything in
  `private/`.

## Before you finish

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Add tests for new behaviour, preferably end to end against the simulator like
`tests/pipeline.rs` and `tests/wizard.rs`. If a screen changed, regenerate the website's
screenshots: `cargo run --release --example screenshots -- docs/img` (needs fontconfig).

Code style: each file starts with a comment saying what it is for; messages to the user
are plain sentences; errors in user input are reported, never `panic!`ed.
