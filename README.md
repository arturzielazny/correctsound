# CorrectSound

**Room correction for Linux, in a terminal.** Plug in a miniDSP UMIK-1, answer a few prompts,
and a measured, verified equalizer for your speakers and room is loaded into EasyEffects.
It replaces the REW → export → import routine with one wizard.

**Website, with the full story, diagrams and screenshots:
<https://arturzielazny.github.io/correctsound/>**

![The result screen: measured, target and predicted response above the equalizer's curve](docs/img/6-result.png)

> Written end to end by an AI coding agent (Claude) with one person testing it on real speakers.
> It is meant to be taken apart: clone it, point your own AI agent at it, make it do what
> your setup needs, and send a pull request. See [Make it yours](#make-it-yours).

## Install

```sh
cargo install --path .
```

On Linux it needs `pipewire` (for `pw-cat`, `pw-dump`), `wireplumber` (`wpctl`) and
`alsa-utils` (`amixer`). EasyEffects is optional: without it you still get the EQ files. On
macOS and Windows only the simulator and the saved-session commands work for now (see
[Platforms](#platforms)). Try it without any hardware, on any OS:

```sh
correctsound --simulate
```

## Use

```sh
correctsound
```

| Step | What happens |
|------|--------------|
| 1 Devices | Finds the UMIK-1 and your outputs (the one EasyEffects plays to is preselected). EasyEffects is bypassed while measuring. |
| 2 Calibration | Type the mic's serial; its calibration file is downloaded from miniDSP and cached. Choose 0° (mic points at the speakers) or 90° (points at the ceiling). |
| 3 Mic gain | Sets the mic's PipeWire volume and ALSA capture control to unity, which the calibration assumes. |
| 4 Level | Pink noise plays from one speaker; turn the amplifier until the meter shows 75 dB (or use ↑/↓). Tab switches speakers. |
| 5 Measure | A 5.5 s sweep from each speaker. Optionally move the mic 20–30 cm and press `a` to add positions; they are averaged. `p` opens live placement. |
| 6 Result | Measured, target and predicted response, the equalizer's curve (each filter and the whole), and the filter list. High limit and filter count are chosen automatically; `t/T` tilt, `h` high limit, `f/F` filter count, `d` back to automatic. |
| 7 Save | Writes the files; `a` loads them into EasyEffects (after backing up its settings), `v` re-measures through EasyEffects to verify. |

More screens:

- `p` **Placement and room treatment** (measure step): the bass at your seat, measured over
  and over with an evenness score. **Walk around with a bass trap and see what it does right
  away**: pin the room as it is with Space, put the trap in a corner, and the curve shows the
  difference. The same works for moving the speakers or your seat. Two modes, switched with
  `l`: *sweeps* (a short sweep every couple of seconds, fine detail, and a waterfall showing
  how long each note rings, compared with the pinned sweep) and *live* (bass-only periodic
  noise without a break; the curve follows a change within about half a second). Below about
  200 Hz the room decides what you hear, and this is the only cure for the dips no EQ can
  fill.
- `w` **Decay**: a waterfall of the bass, how each note fades after the sound stops, and the
  notes that ring longest.
- `c` **Curiosities**: the room's resonances and the room dimensions they suggest,
  reverberation time per octave, the strongest early reflection, ringing notes,
  cancellations and broad dips with their likely cause, the speakers' bass and treble
  extension, tonal balance and left/right match. Also written to `report.txt`.
- `i` **How it works**, with `m` for the technical version.

Charts are drawn as images in the current Omarchy theme's colours where the terminal shows
images (Ghostty and kitty through the kitty graphics protocol, foot through sixel), and with
braille characters elsewhere.

`Esc` goes back a step, `q` quits. Everything the tool changes (EasyEffects bypass, mic gain,
output volume) is put back when it exits, including on errors and signals.

Other commands:

- `correctsound sessions`: saved measurements, newest first, numbered.
- `correctsound review [N | SESSION_DIR]`: open a saved measurement in the app (default: the
  newest). Its recordings are analysed again with the current version; you can change the
  design, save and apply it. Nothing is played or recorded.
- `correctsound devices`: what step 1 sees.
- `correctsound --cal-file FILE meter`: the room's sound level from the mic for a few seconds. Plays nothing.
- `correctsound fit SESSION_DIR [--tilt -1] [--high-limit 2000] [--filters 8]`: redo the EQ from a saved session (omitted options are automatic).
- `correctsound --simulate`: the whole wizard against a simulated room, no hardware.
- `--cal-file`, `--source NODE`, `--sink NODE`, `--spl-offset DB`: use a calibration file you already have, another mic, a specific output, or a custom dBFS → SPL offset.

## Files

Each run creates `~/.local/share/correctsound/sessions/<date_time>/` with:

- `left.txt`, `right.txt`: Equalizer APO filters (EasyEffects' equalizer can import these per channel)
- `CorrectSound.json`: EasyEffects output preset (one split-channel equalizer)
- `report.txt`: settings, filters, before/after deviation, curiosities
- `*-capture.wav`, `*-response.csv`, `session.toml`: raw recordings and responses, for `fit` and `review`
- `*-predicted.csv`, `verify-*.csv`, `verify-*-capture.wav`: the predicted result and the
  verification sweep through EasyEffects, at the same smoothing, for comparison
- `easyeffects-backup/`: a copy of `~/.config/easyeffects/db` taken before the preset was loaded

Applying installs the preset as `~/.local/share/easyeffects/output/CoSo-NNN-DDMMYYYY.json`
(`NNN` continues from the highest `CoSo-` number there, e.g. `CoSo-002-14112026`) and loads it
with `easyeffects -l`. It also becomes the preset EasyEffects autoloads for that output
(`~/.local/share/easyeffects/autoload/output/<sink>:<route>.json`), so it survives EasyEffects
restarting without saving its state; an autoload file it replaces is backed up in the session.

## How the EQ is designed

- Response: log sweep 10 Hz – 22 kHz, deconvolved; the impulse response is windowed (125 ms
  before the peak, 500 ms after); the mic calibration is removed.
- Target: a −0.5 dB/octave tilt (`t/T` to change; 0 is flat), from the speaker's bass limit
  to its treble limit. The bass limit is where the response, averaged in dB over an octave,
  falls 6 dB under target; octave averaging stops a room-mode spike below the roll-off from
  passing for bass extension. The treble limit is where it falls 6 dB under and stays there
  (20 kHz if it never does). Each channel is matched to its own level; a left/right difference
  is corrected with the equalizer's balance and output gain.
- Below the bass limit (down to 25 Hz) the target is the speaker's own roll-off, and room
  resonances standing above it are cut. Cuts there tame boom without more cone excursion;
  nothing below the limit is boosted.
- Filters: bells; cuts down to −12 dB. Boosts must be wide (Q ≤ 2), since narrow dips are
  cancellations that EQ cannot fill, and start at twice the bass limit. The summed EQ may
  boost at most +3 dB below 300 Hz (room modes) and +6 dB above (broad speaker or desk dips),
  so stacked filters cannot get round the limit. Above 5 kHz filters are wide (Q ≤ 2): finer
  detail there moves with the head. A shared preamp prevents clipping.
- Filter count, per channel: with several mic positions, leave-one-out cross-validation
  (fit to the other positions, score on the held-out one, smallest count within one
  standard error of the best), so only corrections that hold across positions are kept;
  with one position, the smallest count reaching 90% of the improvement 16 filters make.
  `report.txt` lists the error by filter count.
- Measurement checks: each sweep's signal-to-noise ratio is measured per octave (63 Hz–8 kHz)
  from the noise in the deconvolved impulse response, the same noise that ends up in the
  curve; under 20 dB in any octave asks for a louder or quieter-room retake. The level meter
  only counts the test noise's band, so infrasonic rumble does not inflate it.
- Filters use EasyEffects' `APO (DR)` bell mode, the RBJ cookbook filter the fitter models,
  so the prediction matches what EasyEffects does.

## Notes on implementation choices

- Audio runs through `pw-cat` subprocesses instead of the `pipewire` crate. Both streams set
  `node.dont-move`; without it, EasyEffects re-routes any new output stream into its own sink,
  and the measurement would include the existing EQ. The playback stream's links are checked
  during every sweep, and the sweep is abandoned if it reaches anything except the chosen output.
- The 0° calibration URL is `…/umikcal/umik.php/<serial>.txt` (not `umik00.php`); an unknown
  serial returns HTTP 200 with an HTML error, which is detected.
- SPL = dBFS + 124 − sensitivity factor (matches REW on a current UMIK-1). Older 18 dB-gain
  mics are marked approximate. The EQ only depends on the response shape, not this constant.

## Platforms

| | Status |
|---|---|
| Linux (any distribution with PipeWire) | Works. EasyEffects is optional; without it you get the EQ files. |
| macOS, Windows | Builds, and `--simulate` works. Measuring real speakers needs a platform module: one folder under `src/platform/`. |

Everything that depends on the operating system lives in `src/platform/`, behind two
interfaces; the rest (measuring, EQ design, the screens) is shared. CI builds and tests every
change on Linux, macOS and Windows. A port adds a platform and never rewrites the app for one
OS: [AGENTS.md](AGENTS.md) has the layout, a checklist for adding a platform, and the rules.

## Make it yours

CorrectSound does one setup well: Linux, PipeWire, EasyEffects, a UMIK-1, two speakers. Yours
may differ: macOS, Windows, a UMIK-2 or another mic, a subwoofer, CamillaDSP, a receiver with
its own EQ. **Clone the repository and give it to your favourite AI coding agent.** The code is
small (about 11k lines of Rust), tested against a simulated room, and written to be read.
[CONTRIBUTING.md](CONTRIBUTING.md) has prompts that work as a starting point,
[AGENTS.md](AGENTS.md) the rules your agent should follow, and [IDEAS.md](IDEAS.md) what is
planned and why.

When it works for you, open a pull request, so the next person with your setup does not have
to start over.

## Development

```sh
cargo test                          # DSP, fitting, parsers, and the whole wizard on the simulator
cargo run -- --simulate             # the app, no hardware
cargo run --release --example screenshots -- docs/img   # the website's screenshots
```

## Patents

A brief, non-lawyer look found nothing this code needs a licence for. The methods it uses
are published and decades old: exponential sweeps (Farina, 2000), regularised inversion
(Kirkeby, 1998), the RBJ biquad cookbook, Schroeder backward integration (1965), cumulative
spectral decay (1970s), power averaging of positions, cross-validation. Audyssey's patent on
averaging several listening positions (US 8,005,228) is listed as expired; Sonos' calibration
patents claim networked playback devices calibrated with a moving phone, which this is not;
Dirac-style mixed-phase FIR correction is not used. This is not legal advice; if it matters
for your use, ask a patent attorney.

## License

[MIT](LICENSE). Take it, change it, ship it.
