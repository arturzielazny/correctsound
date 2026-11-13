# Contributing

CorrectSound was written by an AI coding agent, with one person testing it on real speakers.
The quickest way to adapt it is the same: clone it, open your own agent (Claude Code, Codex,
Cursor, Aider, whatever you like) in the repository, and describe your setup. This page has
prompts that work as a starting point. Change the details to match your hardware.

Then send a pull request. Someone else has your setup too.

## Before you start

- `cargo test` runs everything, including the whole wizard against a simulated room
  (`src/audio/sim.rs`). Work against the simulator first: it plays nothing, and its true
  response is known, so tests can check results exactly.
- `correctsound --simulate` is the app on the simulator.
- Start with the module docs: `src/lib.rs` lists the modules. Each file starts with a comment
  saying what it is for.
- [IDEAS.md](IDEAS.md) lists what is planned, with the reasoning.

## A preamble for every prompt

Agents do better with the ground rules up front. Paste this before any of the prompts below:

```text
You are working on CorrectSound, a Rust TUI that measures speakers with a measurement mic
and generates room-correction EQ. Read README.md, CONTRIBUTING.md and IDEAS.md, then the
module docs (src/lib.rs and the first comment of each file). Run `cargo test` before changing
anything, and keep it passing.

Rules:
- Never play sound on my real speakers without asking me first and waiting for my answer.
  Use the simulator (src/audio/sim.rs, `--simulate`) for development and tests.
- Back up any settings file of another program before overwriting it, and restore what you
  change when the app exits (see src/restore.rs).
- Keep the DSP (src/dsp, src/eq) independent of the platform; put platform code behind the
  existing traits (AudioBackend in src/audio, System in src/system).
- Add tests for new behaviour, preferably end-to-end against the simulator, like
  tests/pipeline.rs and tests/wizard.rs.
- Run `cargo fmt` and `cargo clippy --all-targets -- -D warnings` before you finish.
```

## Prompts

### macOS

```text
Port CorrectSound to macOS. Audio: add an AudioBackend that plays and records through
CoreAudio (the `cpal` crate is fine), at 48 kHz, routing playback to the device the user
picks and recording from the UMIK-1. System: replace the PipeWire/wpctl/amixer parts with
macOS equivalents (device list, input volume at unity, output volume), behind the System
trait. Output: keep left.txt/right.txt (Equalizer APO format), and add an exporter for
<the EQ app I use: eqMac / SoundSource / Rogue Amoeba Audio Hijack / none>. Keep Linux
working. Show me the plan before you write code.
```

### Windows

```text
Port CorrectSound to Windows. Use WASAPI (via `cpal`) for playback and capture at 48 kHz,
in exclusive mode if shared mode resamples. Replace the PipeWire/EasyEffects parts with
Equalizer APO: write the filters to an include file under Equalizer APO's config directory
and add one `Include:` line to config.txt (back up config.txt first; restore it on request).
Detect whether Equalizer APO is installed on the chosen device and explain how to install it
if not. Keep Linux working. Show me the plan before you write code.
```

### Another measurement mic

```text
Add support for a <miniDSP UMIK-2 / Dayton Audio iMM-6 / my own mic with a .cal/.frd file>.
The UMIK-2 records at 44.1–192 kHz and has its own calibration download; other mics come with
a calibration file in a different format and need a sensitivity entered by hand. Detect the
mic where possible, parse its calibration format (add a parser and tests with a made-up file;
never commit someone's real calibration file), and make the sample rate a setting rather than
the 48 kHz constant in src/dsp/mod.rs, with tests at 44.1 and 96 kHz on the simulator.
```

### A subwoofer

```text
I have a subwoofer connected to my amplifier (it has its own crossover; the computer still
sends stereo). Implement the "Subwoofer connected to the amplifier" section of IDEAS.md,
starting with the alignment step and the level hint. Extend the simulator with a subwoofer
(its own room response, crossover, phase and gain settings) so everything can be tested
without sound.
```

### CamillaDSP, or PipeWire's own filter chain, instead of EasyEffects

```text
I don't use EasyEffects. Add an output for <CamillaDSP / a PipeWire filter-chain module>:
generate its configuration from the EqSet (src/eq), install and load it the way
src/system/easyeffects.rs does for EasyEffects (back up, apply, autoload, restore), and let
the user pick the output in step 1 when both are available. Verification (the `v` key) must
measure through the new path.
```

### A headless mode for a box without a screen

```text
My speakers are driven by a Raspberry Pi without a display. Add a non-interactive mode:
`correctsound auto --sink NAME --level-check` that runs the measurement steps with prompts
on stdout and confirmations on stdin, writes the session, and prints the report. Keep the
TUI as the default. Reuse the App state machine rather than duplicating its logic.
```

### FIR correction

```text
Add an optional FIR output: from the measured response and the designed target, build a
minimum-phase FIR (or mixed-phase below 300 Hz, with a clear explanation of the trade-off)
for EasyEffects' Convolver plugin. Keep the parametric EQ as the default. Show the predicted
result for both in the result screen, and verify the FIR the same way the EQ is verified.
```

### Your own idea

```text
I want CorrectSound to <describe what you need and why>. My setup: <OS, audio stack, mic,
speakers, amplifier, room>. Read the code first and tell me which parts would change and
how you would test it on the simulator. Wait for my go-ahead before writing code.
```

## Pull requests

- One topic per pull request, with tests. Screenshots of new screens help: regenerate the
  website's with `cargo run --release --example screenshots -- docs/img`.
- Say which hardware you tested on, or that you only tested on the simulator.
- Never commit personal data: calibration files of real mics, `pw-dump` output (it contains
  host and user names), recordings of your room.
- Mentioning that an AI agent wrote the code is welcome, not a problem.

By contributing you agree that your contribution is licensed under the [MIT licence](LICENSE).
