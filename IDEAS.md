# Ideas for later

Nothing here is planned yet. Each entry says what it is, why it would help, and what it
would take.

## Subwoofer connected to the amplifier

The setup: the subwoofer is fed by the amplifier (sub-out or speaker-level inputs) and has its
own crossover, so the computer still sends plain stereo.

**What works today without changes.** Each channel's sweep plays the main speaker plus that
channel's share of the subwoofer, so the app measures one deeper-reaching "speaker" per
channel. The bass limit moves down to the subwoofer's roll-off (about 25–35 Hz), so boosts are
allowed from twice that and the deep room resonances (29, 33 Hz here) get real cuts. The sweep
starts at 10 Hz, the analysis resolves 0.7 Hz, and the level step listens only to
500–2000 Hz, so none of that needs touching.

**What doesn't work well:**

1. **Crossover cancellation.** Around the crossover (typically 60–100 Hz), the subwoofer and
   the main speakers must arrive in step or they partly cancel. EQ cannot fill that dip; the
   subwoofer's phase knob, polarity switch or position fixes it. Today it would only show up
   as one of the "deep dips" in the curiosities.
2. **Subwoofer level.** A subwoofer set too loud shows as bass above target, and the fitter
   spends several bell filters cutting it, wasting amplifier headroom. Turning the subwoofer's
   own gain knob down is cleaner.
3. **One box, two corrections.** The subwoofer is one source shared by both channels, but left
   and right are fitted separately, so their bass filters come out slightly different for the
   same box.
4. **Limits tuned for small speakers.** The cut floor is 25 Hz and the per-octave SNR checks
   start at 63 Hz.

**What to build, in order of value:**

1. **Subwoofer alignment step.** Play band-limited noise or a repeating short sweep around the
   crossover through both channels, and show the level at the seat live (a bar plus the
   crossover-region curve) while the user turns the phase knob or flips polarity. The setting
   with the highest level at the crossover is the one where subwoofer and speakers add up.
   Optionally the same loop for subwoofer distance or placement. Reuse the level meter's
   machinery with a 40–150 Hz band.
2. **Subwoofer level hint.** After measuring, compare the 30–80 Hz average with the target and
   say how far to turn the subwoofer's gain knob ("bass is +4 dB above target from 30–80 Hz:
   turn the subwoofer down about 4 dB and measure again"), instead of letting filters do it.
3. **Shared bass correction.** Below the crossover, fit one filter set to the power average
   of left and right and apply it to both channels; fit separately above. Needs a crossover
   frequency: ask for it, or detect it from where the two channels' bass starts to agree.
4. **Lower limits when a subwoofer is present.** Cut floor 20 Hz, SNR checks from the 31 Hz
   octave, a 63 Hz reverberation band.
5. **Crossover-dip insight.** In the curiosities, flag a dip within half an octave of the
   crossover and point to the phase knob and polarity switch rather than EQ.

**Practical notes for measuring with a subwoofer:** turn off its own room correction or
"auto" EQ, set auto-standby to "on" so it doesn't miss the start of a sweep, and measure
several mic positions, since deep bass varies more from seat to seat.

**Not covered here:** a subwoofer on its own output channel with the crossover done in
software. EasyEffects only processes stereo, so that would mean driving a PipeWire
filter-chain directly (crossover, subwoofer delay, three corrections): a different project.

## Other operating systems

The plan for running on more than Linux, in order. AGENTS.md has the rules; the short version
is that every port adds a module under `src/platform/` and leaves the rest alone.

1. **Make the core platform-neutral (done).** All OS code moved behind `platform::System`
   and `audio::AudioBackend`, with neutral types: a `Device` instead of a PipeWire node, an
   `InputGain` made of labelled settings instead of PipeWire and ALSA fields, an `EqHost`
   instead of EasyEffects flags, `Route::ThroughEq`, a routing check the platform answers,
   undo steps as closures. `platform::native()` picks the platform at compile time;
   elsewhere `Unsupported` explains that only `--simulate` works. CI builds and tests on
   Linux, macOS and Windows.
2. **macOS.** A `src/platform/macos/` module:
   - Audio: `cpal` (CoreAudio) at 48 kHz, playing to the chosen output and recording the
     UMIK-1 (it shows up as a standard USB audio input). Check that macOS does not resample:
     set the device's nominal rate to 48 kHz while measuring and restore it.
   - Devices and volumes: CoreAudio properties (device list, names, channel count, input
     volume scalar, output volume), through `objc2-core-audio` or `coreaudio-sys`.
   - EQ host: macOS has no EasyEffects. The open option that can be scripted is CamillaDSP
     with the BlackHole loopback driver (system output → BlackHole → CamillaDSP → speakers),
     whose YAML config can be generated from the `EqSet`. Without it, files only
     (left.txt/right.txt, which eqMac and others can import by hand).
   - Terminal: charts are images in Ghostty, kitty, WezTerm and iTerm2 (kitty or sixel
     protocol) and braille in Terminal.app; nothing to change.
3. **Windows.** A `src/platform/windows/` module: WASAPI through `cpal` (exclusive mode if
   shared mode resamples), endpoint volumes through the Core Audio API, and Equalizer APO
   as the EQ host (an `Include:` line in its config.txt, backed up first). Windows Terminal
   shows sixel images.
4. **Other Linux audio stacks.** PulseAudio-only or JACK systems could get their own
   `System` beside the PipeWire one, picked at run time by what is installed.

## Next for the room screen

- Left/right phase at each mode (both speakers in the same half of a standing wave or not)
  as a second clue to where the speakers are, independent of the mic position.
- Fit the dimensions themselves (not only candidates from the resonances) and the speakers'
  acoustic centres, which sit a little in front of where a tape measure ends.
- Close pairs of standing waves read as ringing too long: in the simulated living room the
  waves at 69.0 and 72.2 Hz, both set to ring 0.5 s, measure about 2.2 s, as their beating
  slows the decay curve. Fit two decays (or the pair's envelope) where two waves lie within
  their own bandwidths of each other. The real room's 62/69 Hz readings may be the same.
- Fold the room model into the EQ: the 0.5 s analysis window understates a mode that rings
  1.7 s by 2–3 dB (measured on the simulator), which the model can correct.

## Faster feedback in placement, keeping the quality

The idea: overlap the sweeps. Start the next sweep two seconds after the previous one
instead of waiting for it to end, so a full-length sweep finishes every two seconds.

It works in principle. Exponential sweeps can overlap (the multiple exponential sweep method,
Majdak, Balazs and Laback, JAES 2007): deconvolving one sweep turns the others into copies of
the room's impulse response shifted by the offset, with their distortion products just before
them, so they stay apart as long as the room has died away within the offset.

Two catches make it a weaker deal than it looks for one speaker:

- **The level.** A 5.5 s sweep restarted every 2 s means about three sweeps sounding at once;
  their sum peaks up to three times higher, so each must play about 9 dB quieter to avoid
  clipping. That gives back most of what the long sweep gains in signal-to-noise (5.5 s
  against 2 s is +4.4 dB).
- **The latency.** Each result still describes the last 5.5 s, so after moving a trap the curve
  shows the change fully only a whole sweep later. The updates come faster; the answer
  doesn't.

**What to build instead: a 2-second looped sweep.** The fast sweeps already loop a 1 s sweep
and recompute five times a second; a 2 s period gives a full-quality result from the latest
two seconds every 0.2 s, 0.5 Hz bins, a change shown within two seconds, and room for a
decay of about 1.7 s, so the waterfall would finally show the 33 and 45 Hz modes that ring
1.5–1.7 s in the room this was tested in (the 1 s loop cuts them off at 0.7 s). It could
replace the sweeps mode, whose 1.4 s sweeps with pauses give the same detail less often.

**Where overlapping does pay: both speakers at once.** The method was made for several
loudspeakers: on the measure step, the right speaker's sweep starts while the left one's is
still playing, and one deconvolution gives both impulse responses, the right one `Δ` later.
Each speaker plays at full level (they are different sources: nothing has to be turned down).

Tried on the simulated living room (5.64 × 5.24 m, the 33 and 45 Hz waves ringing 1.5–1.6 s)
with speaker distortion added (2nd harmonic −40 dB, 3rd −50 dB), each speaker's result from
the overlapped recording against measuring it alone:

| Δ | time per mic position | EQ curve (0.5 s window) | decay of the room's waves |
|---|---|---|---|
| 0.5 s | 8.0 s | up to 0.5 dB off | lost (see below) |
| 1.0 s | 8.5 s | 0.1 dB | lost |
| 1.5–4 s | 9.0–11.5 s | identical | lost |
| 5.0 s | 12.5 s | identical | as alone |
| one after the other | 14.9 s | | |

- **The EQ curves** need `Δ` ≥ 1.5 s: below that the right speaker's distortion products,
  which arrive 0.5 s (2nd) and 0.8 s (3rd) before its impulse response, fall inside the left
  speaker's 0.5 s window.
- **The decay of the room's waves** is lost until `Δ` = 5 s only because `modes::find` takes
  its noise floor 4–5 s after the impulse, where the right speaker's sound now is. Taken from
  a clean stretch instead (before the right speaker's distortion arrives, or after its tail),
  the real limit is that distortion landing on the left speaker's still-ringing tail: the
  33 Hz wave needs about 1 s of clean tail for its 35 dB fit, so `Δ` ≈ 2.5 s.

At `Δ` = 2.5 s a mic position takes 10.0 s instead of 14.9 s (a third less; three positions
in 30 s instead of 45). To build it:

1. Play both channels in one stereo stream, so the offset is sample-exact: today a `Source`
   plays one channel, so `AudioBackend` would take a source per channel (every platform).
2. Split the analysis at the impulse response: `SweepAnalysis::analyze` on a recording
   becomes deconvolve-then-analyse, with the second speaker's peak searched near `Δ`.
3. Give `modes::find` a noise floor from a clean stretch, and stop each speaker's analysis
   windows before the other speaker's distortion arrives (`Δ` − 1 s).
4. The clipping check covers both speakers sounding together (up to 6 dB more at the mic).
5. Keep one-after-the-other as a fallback where the clean stretch is too short (a room
   ringing longer than about 1.5 s at its lowest wave).

## The window (the default; `--tty` for the terminal)

The same screens and keys in a window of its own, drawn on the GPU, all in Rust: ratatui lays
the screen out in cells as in the terminal, then vello (on wgpu: Vulkan, Metal, DirectX 12)
draws the text and the charts straight from the app. Charts draw through the same `Canvas`,
which records a vello scene there instead of pixels, so every chart works in both. It is the
default way to show the app; `--tty`, a login over SSH, or no display or GPU keep it in the
terminal. It is a cargo feature, on by default (`--no-default-features` leaves it out).

Measured on a 5120 × 2160 screen, release build, laptop with a Radeon 890M:

| | terminal (kitty images) | window |
|---|---|---|
| result screen, a frame whose charts changed | 40 ms CPU, then the terminal decodes 1 MB | 3 ms CPU, 6 ms on the GPU |
| a placement update | 40 ms CPU | 2 ms CPU, 5 ms on the GPU |
| room screen | 22 ms | 2.4 ms |

`tests/window.rs` keeps it so, and checks that each frame is a picture where a GPU is found.

Next for it:

- The room screen's keys now wait for the room model, not for drawing (45–80 ms in the test
  build after →, `v` or `g`): move that work off the key too.
- Tick faster than 50 ms while something moves (the level meter, placement), so meters run
  at the screen's rate; the GPU makes that cheap.
- What only a window can do: the exact Hz and dB under the mouse, dragging a speaker on the
  room plan, zooming a chart.
- Release it: a `.desktop` file, then packaging on macOS and Windows; decide whether the
  feature becomes the default.
- Some time, just to see: the result screen in egui (`egui_plot` has zoom and readouts under
  the cursor built in). Its immediate mode fits how the UI already draws from the app's
  state each frame; it would look less like a terminal.

## The web version

Stage 1 is done: <https://arturzielazny.github.io/correctsound/try/> runs the wizard against
the simulated room in the browser, with the terminal's screens and keys (web/README.md has how).
Next:

- Work off the page's only thread. Entering the result screen holds the page for about 0.9 s
  (the design and the room guess, which use every core in the installed app and run one after
  the other here). A Web Worker running the same Rust would keep the page answering;
  `src/background.rs` is the one place to change.
- Download the session's files from the save step (they are kept in memory now), and keep
  sessions in the browser's storage between visits.
- A smoke test in CI: open the page in headless Chrome, measure, and fail on a panic.
- Stage 2, measuring for real: the mic through `getUserMedia` with echo cancellation, noise
  suppression and automatic gain turned off; the sweep played on one channel through Web Audio
  (an AudioWorklet feeding and recording the app's `Stream`); the output chosen with
  `setSinkId` where the browser has it; the calibration file dropped on the page (miniDSP's
  server does not let other sites download it). The page cannot set the mic's gain or the
  system volume, so the level step tells the user what to change, and it cannot see an EQ
  already running, so it asks for it to be off. The EQ is a download (EasyEffects preset,
  Equalizer APO file).
- Stage 3, checking the EQ without installing it: Web Audio's peaking `BiquadFilterNode` is the
  same cookbook filter the fitter models, so the page can play the sweep through the designed
  EQ and measure the result.

## Done

- **Platform layer** (`src/platform/`): the first step towards other operating systems,
  described above.

- **Live placement mode** (`p` on the measure step): short bass sweeps in a loop, an
  evenness score, and a pinned curve to compare against. In the simulator, ←/→ move the
  speakers relative to a wall.
- **Continuous live mode** (placement): periodic bass noise instead of sweeps, the
  curve recomputed every 0.1 s and following a change in about 0.6 s; and a waterfall with
  ringing times beside each sweep in sweeps mode.
- **Measurement level from the room** (level step): listens to the background per
  octave and suggests the quietest level that keeps every octave 30 dB above it (60–80 dB
  SPL instead of a fixed 75), from a margin model checked on 30 real captures. The
  per-octave check now measures the bass noise after the room has stopped ringing.
- **Fast sweeps** (placement, `m` cycles the modes): a 1 s sweep looped without pauses and
  deconvolved circularly, curve and waterfall every 0.2 s. A big evenness number for
  reading across the room.
- **Fastest sweeps** replace the live noise mode: a 0.34 s bass sweep looped, the curve
  every 0.1 s (it matches a single sweep within 0.2 dB in the simulator), with a rough
  waterfall of the first 120 ms (a 120 ms window, ending before the sweep's distortion wraps
  around). Sweeps have a
  lower crest factor than noise, so more signal at the same peak level. The most even
  curve so far (faded green) is gone: it cluttered the chart; the pinned curve is drawn
  solid and dimmer, behind the latest. (Later removed: fast sweeps follow a change within a
  second at full detail and with a real waterfall, which is fast enough.)
- **Instant on large screens**: on a 5K monitor a change on the result screen sent 48 MB
  of uncompressed chart pixels to the terminal; charts are now compressed (about 1 MB) and
  encoded in parallel bands. The EQ fit placed its filters once per count (16 times over);
  it now places them once and refines each count, in parallel with the cross-validation
  folds (650 → 120–160 ms, the same filters to the bit), and runs in the background, a run
  of keys designed once. The room is guessed in the background from the result screen on;
  the "where to sit" map sorted the whole map for every pixel. Keys waiting in the
  terminal are all taken before the next frame, a screen under an overlay is not drawn,
  and a review analyses its recordings in parallel (1.7 → 0.3 s). `tests/performance.rs`
  keeps it so. Next: draw a screen's charts in parallel (a placement frame is two of them).
- **A richer simulator**: `SimConfig::living_room` puts the speakers in a room of known
  size; the bass comes from the room model's standing waves (one band-pass filter each, so
  they ring as long as the model says) and the treble from the direct sound and the six
  walls' first reflections. Each added mic position moves the mic 25 cm. `SimState` can
  unplug the mic, add outputs and change the default. `tests/scenarios.rs` uses it: a mic
  plugged in late, the wrong output (now named on the level step), and the room screen
  finding a 5.64 × 5.24 m room and its right-hand corner from the measurement alone, which
  first needed the guess to infer a length heard only through a wave around four walls.
- **Ultrawide terminals**: charts wider than 256 cells are sent as tiles, because one
  kitty-protocol image (Unicode placeholders) spans at most 297 columns.
- **Decay (waterfall) view** (`w`): cumulative spectral decay of the bass, and the notes
  that ring longest, also listed in the curiosities and `report.txt`.
- **Room screen** (`r`): a rectangular-room modal model (Kuttruff) with the measured decay
  times, compared with the measurement below 60 Hz (2–3 dB RMS on a real room with the true
  layout, 7–8 with a typical one). It guesses the room and the layout from the measurement
  (floor plan within 2 % on three real sessions), names the resonances, draws the plan with
  each standing wave and where traps work, a seat map, and seat or setup moves predicted to
  help, only when the model matches.
- **Room modes' true decay**: peaks from 20 to 150 Hz measured past the waterfall's
  400 ms, by energy decay (demodulated, steep backwards low-pass, Schroeder) and by peak
  width, merged over mic positions. On a real room both gave 1.7 s for a 33 Hz mode.

## Earlier ideas

- **Blind listening test.** EasyEffects bypass toggled while Spotify plays. Must be
  loudness-matched (the corrected path is quieter by the preamp), compare the same excerpt
  (`playerctl` to jump back), and run 16–20 rounds with the odds of guessing reported. More
  useful comparing two corrections (tilt, filter count, two presets) than on vs off.
- **Taste layer.** An optional bass shelf and treble shelf after the correction filters, each
  with its own amount (bass shelf capped for small speakers), adjustable in EasyEffects
  without re-measuring. Keeps the correction neutral and verifiable.
- **Replace `ureq` with `curl`.** `ureq` and its TLS stack are about a fifth of the
  dependencies, for one calibration download per serial; `curl` is always installed on Arch.
  `pgrep` could also become a `/proc` read.
- **Left/right time alignment.** Not needed for a centred desktop seat; would need both sweeps
  in one continuous recording to compare arrival times.
- **FIR phase correction.** Little audible return here; skip unless something changes.
