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

## Done

- **Platform layer** (`src/platform/`): the first step towards other operating systems,
  described above.

- **Live placement mode** (`p` on the measure step): short bass sweeps in a loop, an
  evenness score, and a pinned curve to compare against. In the simulator, ←/→ move the
  speakers relative to a wall.
- **Continuous live mode** (placement): periodic bass noise instead of sweeps, the
  curve recomputed every 0.1 s and following a change in about 0.6 s; and a waterfall with
  ringing times beside each sweep in sweeps mode.
- **Fast sweeps** (placement, `m` cycles the modes): a 1 s sweep looped without pauses and
  deconvolved circularly, curve and waterfall every 0.2 s. A big evenness number for
  reading across the room, and the most even curve so far kept in faded green.
- **Decay (waterfall) view** (`w`): cumulative spectral decay of the bass, and the notes
  that ring longest, also listed in the curiosities and `report.txt`.

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
