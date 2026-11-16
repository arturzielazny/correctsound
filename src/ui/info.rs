//! The info screen: how CorrectSound works, plainly or (m) in technical detail.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// A heading line.
fn h(text: &'static str) -> Line<'static> {
    Line::from(Span::styled(text, Style::default().bold()))
}

/// A paragraph.
fn p(text: &'static str) -> Line<'static> {
    Line::from(text)
}

fn gap() -> Line<'static> {
    Line::from("")
}

/// The screen's text.
#[must_use]
pub fn lines(technical: bool) -> Vec<Line<'static>> {
    if technical {
        technical_lines()
    } else {
        simple_lines()
    }
}

fn simple_lines() -> Vec<Line<'static>> {
    vec![
        h("How CorrectSound works"),
        gap(),
        p(
            "Your speakers do not sound at your seat the way they sound in a test lab. The room \
           adds resonances (some bass notes boom), reflections from the desk and walls cancel \
           some frequencies, and the speakers themselves are not perfectly even. CorrectSound \
           measures what actually reaches your ears and builds an equalizer that evens it out.",
        ),
        gap(),
        h("The steps"),
        p(
            "1 Devices: finds the mic and the output your EQ program (EasyEffects on Linux) plays \
           to. The EQ program is paused while measuring, so the bare speakers are measured.",
        ),
        p(
            "2 Calibration: every UMIK-1 is slightly different; its own correction file, from \
           miniDSP, removes the mic's colouring.",
        ),
        p("3 Mic gain: the calibration assumes the mic signal is not amplified; this checks it."),
        p(
            "4 Level: first CorrectSound listens to the room's background noise and works out \
           how loud the measurement must be to stay well above it, often only 60 dB. Then pink \
           noise plays while you turn the amplifier to that level.",
        ),
        p(
            "5 Measure: a sweep, a tone gliding from 10 Hz to 22 kHz, plays from each speaker. \
           From the recording the app computes how loud every frequency arrives. Measuring a \
           few mic positions around your head is worth it: the room sounds different 20 cm \
           away, and averaging keeps what is common to all of them.",
        ),
        p(
            "6 Result: the measurement is compared with a target: even, gently sloping down \
           towards the treble, the balance a good speaker has in a real room. The app finds \
           filters that bring the measurement closer to it, and chooses how many by checking \
           which ones keep helping at mic positions they were not fitted to.",
        ),
        p(
            "7 Save: writes the files, loads them into the EQ program as a preset (it also loads \
           automatically from then on), and can play the sweeps again through it to verify the \
           result.",
        ),
        gap(),
        h("Placement and room treatment (p on the measure step)"),
        p(
            "The bass at your seat, measured over and over while you move the speakers or your \
           seat, or carry a bass trap from corner to corner, with a number for how uneven it \
           is (lower is better). Pin the room as it is with Space, try a change, keep what \
           helps, then measure. Keep the mic where it is. It is the only cure for the deep \
           dips EQ cannot fill.",
        ),
        p(
            "Three ways to measure, switched with m. Sweeps: a short sweep every couple of \
           seconds, with fine detail and a waterfall of how the bass decays; bass traps show \
           best there, as notes that stop ringing sooner (keep still while a sweep plays). \
           Fast sweeps: a one-second sweep over and over without pauses, the curve and the \
           waterfall updated five times a second; a change shows within a second. Fastest \
           sweeps: a third-of-a-second sweep over and over, the curve updated ten times a \
           second; it follows any change within about half a second, so you can watch it \
           while you carry a trap. Its waterfall is only rough (the first 120 ms, coarse in \
           frequency): the room rings longer than one sweep.",
        ),
        p(
            "A big number beside the text says how much more even than the pinned reading the \
           latest one is, readable from across the room. The pinned curve stays on the chart, \
           behind the latest one.",
        ),
        gap(),
        h("Decay (w)"),
        p(
            "A waterfall of the bass: how each note fades after the sound stops. Notes that \
           ring much longer than the rest sound boomy; the list beside it names them. EQ cuts \
           lower them; bass traps or moving things treat the cause. The waterfall shows the \
           first 400 ms; the room's lowest resonances (modes) are also measured on their own, \
           however long they ring, as the time to fade 60 dB.",
        ),
        gap(),
        h("Room (r)"),
        p(
            "Your room seen from above, with the speakers and your seat. In the bass a room is a \
           set of standing waves between its walls, each loud in some places and silent in \
           others; which ones you hear depends on where the speakers and your seat are. From the \
           measurement alone CorrectSound guesses the room's size and where things are, and \
           lists its guesses under your own rooms (g/G go through the list). Measure your room \
           with a tape measure and change the numbers (↑/↓ choose, type metres and Enter, or \
           ←/→ for 5 cm steps): the speakers from the wall behind them and from the nearer side \
           wall, how far apart they are, and your seat from the speakers. Changing one keeps \
           the rest together, and m mirrors the setup to the other side wall. Changing a guess \
           makes it your room. Your rooms are saved in config.toml and with the session, so a \
           review finds them; x x deletes one.",
        ),
        p(
            "It then names every resonance (\"33 Hz: side to side, 5.24 m\"), shows where each \
           standing wave is loud (that is where bass traps work), says how well its model of \
           the room matches what was measured, and, when it does, whether another seat or \
           moving the whole setup would make the bass more even. v shows where to sit: every \
           spot coloured by how even the bass below 80 Hz would be if you sat there, with the \
           speakers where they are (green even, red boomy or thin). Check any suggestion with \
           live \
           placement before moving furniture.",
        ),
        gap(),
        h("What EQ can and cannot do"),
        p("It can tame peaks: room resonances and bumps are cut, freely and by up to 12 dB."),
        p("It can lift broad, gentle dips a little (at most 3 dB in the bass, 6 dB above)."),
        p(
            "It cannot fill narrow, deep dips. They are cancellations: a reflection arriving out \
           of step with the direct sound. More power cancels just as well, so boosting only \
           wastes amplifier power. Moving the seat or the speakers by 20 to 50 cm moves them.",
        ),
        p(
            "It does not extend the speakers: below their bass limit and above their treble \
           limit nothing is boosted (booming room resonances below the bass limit are still \
           cut).",
        ),
        gap(),
        p(
            "Press m for the technical details, c for what the measurement says about your room, \
           w for the decay view, Esc to close.",
        ),
    ]
}

fn technical_lines() -> Vec<Line<'static>> {
    vec![
        h("Technical details"),
        gap(),
        h("Signal path"),
        p(
            "On Linux, playback and capture are pw-cat subprocesses at 48 kHz, 32-bit float. Both \
           streams set node.dont-move so EasyEffects cannot reroute them; the playback stream's links \
           are checked during every sweep. EasyEffects is bypassed for measuring and the \
           original state is restored on exit, on signals, and if the terminal closes.",
        ),
        gap(),
        h("Sweep and impulse response"),
        p(
            "Exponential (Farina) sweep 10 Hz – 22 kHz, 2^18 samples (5.46 s), −12 dBFS, with \
           0.5 s silence before and 1.5 s after. Deconvolution by FFT division with Kirkeby \
           regularisation outside the sweep band gives the impulse response; harmonic \
           distortion lands before the main peak and is windowed out.",
        ),
        p(
            "Window: 125 ms before the peak (half Hann), 250 ms flat after it, Hann taper to 500 \
           ms. Spectrum: 65536-point FFT (0.73 Hz bins). The long window keeps bass resolution; \
           the room's reverberant field is part of what is measured, as it is part of what you \
           hear.",
        ),
        p(
            "Calibration: the mic's file is interpolated in log frequency and subtracted. SPL = \
           dBFS + 124 − sensitivity factor (dBFS: a full-scale sine is 0 dBFS).",
        ),
        gap(),
        h("Quality checks"),
        p(
            "Clipping above −1 dBFS fails the sweep. The impulse response's peak-to-noise ratio \
           must exceed 25 dB. Per octave from 63 Hz to 8 kHz, the response is compared with \
           the noise in a later stretch of the same impulse response (same window), which is \
           exactly the noise that ends up in the curve: under 20 dB asks for a retake. For the \
           octaves below 300 Hz that stretch starts 2 s after the direct sound, when the room \
           has stopped ringing (a 50 Hz mode can still be 40 dB up at 0.6 s), so the margin is \
           the background's and not the room's own decay; above, it starts at 0.7 s, because \
           for the treble the IR there is made from the end of the recording. The level meter \
           counts only the test noise's band (500–2000 Hz, 4th-order band-pass), so \
           infrasonic rumble does not inflate it.",
        ),
        p(
            "Measurement level: while listening, the background is analysed per octave (0.5 s \
           Hann blocks every 0.25 s, the median of the last 3 s). For the log sweep, each \
           octave's margin is the meter level + the speaker's level in that octave relative \
           to the meter's band − the background in the octave + 6 dB (6 ± 2 dB on 30 real \
           captures, and within 1 dB on a synthetic room in the tests). The suggestion is the \
           quietest level giving every octave from 63 Hz to 8 kHz 30 dB, assuming the speaker \
           6 dB weak at 63 Hz and 3 dB elsewhere, kept within 60–80 dB SPL. It goes up at \
           once and down only by 2 dB or more, so it does not flicker.",
        ),
        gap(),
        h("Averaging and smoothing"),
        p(
            "Positions are averaged in power. Display: 1/48 octave. Fitting: 1/48 octave below \
           100 Hz widening to 1/3 octave at 1 kHz and above (resolution where the room has \
           narrow modes, broad where narrow detail is position-specific). Grid: 96 points per \
           octave, 20 Hz – 20 kHz.",
        ),
        gap(),
        h("Target and range"),
        p(
            "Reference: the channel's mean level from 300 Hz to 3 kHz; tilt −0.5 dB/octave \
           pivoting at 1 kHz (t/T). Each channel has its own reference; the left/right \
           difference is corrected by the equalizer's balance and output gain.",
        ),
        p(
            "Bass limit: going down from 200 Hz, the first frequency where the response averaged \
           in dB over one octave is 6 dB below target (dB averaging keeps a single resonance \
           from passing for bass). Treble limit: where that average falls 6 dB below and \
           stays below to 20 kHz. Between 25 Hz and the bass limit the target is that octave \
           average itself, the speaker's roll-off: only peaks above it are corrected, with \
           cuts.",
        ),
        gap(),
        h("Fitter"),
        p(
            "RBJ peaking biquads (EasyEffects' APO (DR) mode, which uses the same formulas, so \
           the prediction is what EasyEffects does). Cost: squared error over the range, peaks \
           weighted 2×, dips clamped to −6 dB (a deeper dip costs no more), effect outside the \
           range weighted 0.25. Greedy seeding on the largest error region (narrow dips \
           skipped), each followed by a short joint refinement; then coordinate descent on all \
           filters' log frequency, gain and log Q.",
        ),
        p(
            "Limits: gain −12…+6 dB. Q 0.5–10 below 300 Hz, 0.5–3 to 5 kHz, 0.5–2 above. Boosts \
           only with Q ≤ 2, centred at least at twice the bass limit. The summed equalizer may \
           boost at most +3 dB below 300 Hz, +6 dB above, and +1 dB below the bass limit \
           (a penalty in the cost, so stacked filters and wide skirts cannot get round it). \
           Preamp: −(largest boost + 0.5 dB).",
        ),
        gap(),
        h("Number of filters"),
        p(
            "Counts 0–16 are all fitted (in parallel). With several mic positions: leave-one-out \
           cross-validation; for each position, fit to the power average of the others and \
           score the correctable error left on the held-out one. The count chosen is the \
           smallest within one standard error of the best mean (one-standard-error rule). \
           With one position: the smallest count reaching 90% of the error reduction 16 \
           filters achieve. report.txt lists the error by count.",
        ),
        gap(),
        h("Verification"),
        p(
            "The sweeps play again through the EQ program (EasyEffects' sink on Linux). The result is compared with the \
           prediction in the corrected range after removing the broadband offset (preamp, sink \
           volume): RMS of the difference, and the largest single deviation.",
        ),
        gap(),
        h("Curiosities"),
        p(
            "Resonances and cancellations: peaks over +4 dB and dips under −8 dB relative to the \
           one-octave average, below 300 Hz. Room dimensions: lengths L of 1.8–8 m whose axial \
           modes 343·n / 2L (n = 1–6) fall within 2.5% of at least two resonances including \
           the first, scored by closeness and by the share of predicted modes found. \
           Reverberation: per octave, band-pass the impulse response, Schroeder backward \
           integration with the noise floor subtracted, slope fitted from −5 to −25 dB (T20), \
           medians over all sweeps. Early reflection: the highest local peak 0.5–20 ms after \
           the direct sound. Broad dips: the 1/3-octave average 5 dB under the two-octave \
           average; distance = quarter wavelength.",
        ),
        gap(),
        h("Placement"),
        p(
            "A 2^16-sample (1.37 s) exponential sweep, 10 Hz – 1 kHz, at the measurement level, \
           with 0.25 s of silence before and 0.6 s after, repeated on one channel. Same \
           deconvolution and window as a full sweep; only clipping and IR peak-to-noise are \
           checked. Shown at 1/24 octave from 20 to 500 Hz, calibrated. Score: RMS of the \
           response minus its two-octave dB average, 40–250 Hz (a slope such as the speaker's \
           roll-off scores zero, peaks and dips do not), with the deepest dip and highest peak \
           against that trend. Each sweep's impulse response also gives a waterfall (as in \
           Decay below), and the notes that ring longest are compared with the pinned sweep.",
        ),
        p(
            "Fastest sweeps: a logarithmic sweep, 15–600 Hz, peaking at the sweep level, \
           filling one period of 2^14 samples (0.34 s) played in a loop. Every frequency in \
           it is a multiple of the \
           loop rate (2.9 Hz), so once the room has settled any one period of the recording \
           gives |H|² = |Y|²/|X|² exactly at those frequencies, whatever its start: no \
           alignment with playback, and powers rather than phases, so the mic's own USB clock \
           does not matter. Recomputed from the latest period every 4800 recorded samples \
           (0.1 s), averaged half and half with the previous result, shown at 1/12 octave. \
           A change shows in about 0.6 s: one period plus the room's decay plus the average. \
           The rough waterfall comes from the circular impulse response of the same period: a \
           120 ms window, slices to 120 ms, ending before the sweep's distortion wraps around \
           to the end of the loop; a tail longer than the loop adds onto it. \
           The score is computed at 1/24 octave in every mode.",
        ),
        p(
            "Fast sweeps: a logarithmic sweep, 10 Hz to 1 kHz, filling a period of 49152 \
           samples (1.02 s) and looped, the jump at the loop point faded over 5 ms. The same \
           periodic trick gives the impulse response by circular deconvolution, \
           IFFT(Y·X*/|X|²) with a third-octave fade outside the band, from the latest period \
           every 9600 recorded samples (0.2 s). It goes through the same window and waterfall \
           as a single sweep. The period is as short as the waterfall allows: it needs 0.7 s \
           after the direct sound, and the sweep's harmonic distortion wraps around to \
           0.15 s before it. In the simulator it matches single sweeps exactly and shows a \
           change in 1.3 s.",
        ),
        gap(),
        h("Decay"),
        p(
            "Cumulative spectral decay from each sweep's impulse response: a 300 ms window \
           (30 ms half-Hann rise, Hann fade over its second half) starting every 20 ms from \
           0 to 400 ms after the direct sound; 32768-point FFT, 1/24-octave smoothing, power \
           averaged over mic positions. Fade time: when a frequency falls 20 dB below the \
           loudest start within a third of an octave (so a louder note's skirt does not count \
           as ringing). Ringing: local peaks of the fade time from 30 to 300 Hz over 1.5 times \
           the median and 100 ms, which halfway through their fade are also peaks in their \
           slice.",
        ),
        p(
            "Room modes, for decays past the waterfall's 400 ms: peaks from 20 to 150 Hz in the \
           spectrum of the first 4 s of the impulse response (2^20-point FFT, 0.05 Hz) that \
           stand 15 dB above its noise floor (from 4–5 s) and 4 dB above their valleys. Each \
           is measured twice. Energy decay: demodulated to 0 Hz, low-passed at 6 % of its \
           frequency (at least 2 Hz, 8th order) backwards in time, Schroeder-integrated with \
           the noise removed, T30, T20 or T15 by how far it clears the noise. Peak width: \
           T60 = 3·ln10/(π·Δf₋₃dB). The energy decay is shown unless it only managed a T15 and \
           disagrees with the width by a third; when the two disagree both are shown. Peaks \
           narrower than 4 s can resolve (T60 over 3.5 s) are steady hums, not modes. Merged \
           over mic positions by median. On synthetic modes both are within 1–2 % alone and \
           15 % for modes a fifth of an octave apart.",
        ),
        gap(),
        h("Room model"),
        p(
            "A rectangular room with reflecting walls: modes up to 200 Hz, each a product of \
           cosines with f = 343/2·√((l/L)²+(m/W)²+(n/H)²); the pressure at the seat is the modal \
           sum Σ εψ(source)ψ(seat)·f/(fₙ²−f²+j·f·fₙ/Q) plus the room's compliance term, for a \
           constant volume-velocity source (Kuttruff, Room Acoustics, ch. 3). Q from each \
           mode's measured T60 where it was measured, from the room's bass reverberation \
           elsewhere. The seat is averaged over 9 points ±20 cm (the measurement averages mic \
           positions).",
        ),
        p(
            "Agreement: model and measurement smoothed to 1/12 octave, each minus its one-octave \
           trend (which removes the speaker's own roll-off), RMS of the difference from 25 to \
           60 Hz. On a real living room the true layout gave 2–3 dB, a typical layout 7–8 dB; \
           above about 70 Hz the box model stops matching (modes crowd, furniture and soft walls \
           decide). Suggestions are offered only when it is under 5 dB, and only for changes \
           predicted to make the bass below 80 Hz at least 1 dB more even (seat within 60 cm) \
           or 1.5 dB (the whole setup moved along or away from the wall).",
        ),
        p(
            "Guess: rooms whose modes explain the resonances heard (weighted by how many \
           measurements heard each; a penalty for rooms whose modes are so dense that anything \
           fits), the eight most likely; in each, the layout (speakers side by side, the seat an \
           equilateral triangle) that best explains both speakers' measured bass (agreement) \
           and their left/right balance at the modes below 55 Hz (the median over mic \
           positions of |L/R| at each mode's peak, which does not depend on the mic position and \
           tells the mirror images apart), ranked by agreement + 0.5 × balance error. On three \
           real sessions it found the floor plan within 1–2 % and the speakers in the right \
           corner; the height and the distance from the wall are the least certain.",
        ),
        gap(),
        p("Press m for the plain version, Esc to close."),
    ]
}
