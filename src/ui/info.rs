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
            "1 Devices: finds the mic and the output EasyEffects plays to. EasyEffects is paused \
           while measuring, so the bare speakers are measured.",
        ),
        p(
            "2 Calibration: every UMIK-1 is slightly different; its own correction file, from \
           miniDSP, removes the mic's colouring.",
        ),
        p("3 Mic gain: the calibration assumes the mic signal is not amplified; this checks it."),
        p("4 Level: pink noise sets a repeatable loudness (75 dB), well above the room's noise."),
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
            "7 Save: writes the files, loads them into EasyEffects as a preset (it also loads \
           automatically from then on), and can play the sweeps again through EasyEffects to \
           verify the result.",
        ),
        gap(),
        h("Placement (p on the measure step)"),
        p(
            "Short bass sweeps repeat every couple of seconds while you move the speakers, your \
           seat or the mic. Each one is drawn as it arrives, with a number for how uneven the \
           bass is (lower is better). Pin a placement with Space, try another, keep the \
           better one, then measure. It is the only cure for the deep dips EQ cannot fill.",
        ),
        gap(),
        h("Decay (w)"),
        p(
            "A waterfall of the bass: how each note fades after the sound stops. Notes that \
           ring much longer than the rest sound boomy; the list beside it names them. EQ cuts \
           lower them; bass traps or moving things treat the cause.",
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
            "Playback and capture are pw-cat subprocesses at 48 kHz, 32-bit float. Both streams \
           set node.dont-move so EasyEffects cannot reroute them; the playback stream's links \
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
           exactly the noise that ends up in the curve: under 20 dB asks for a retake. The \
           level meter counts only the test noise's band (500–2000 Hz, 4th-order band-pass), \
           so infrasonic rumble does not inflate it; ambient noise is the median of 100 ms \
           blocks.",
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
            "The sweeps play again into the EasyEffects sink. The result is compared with the \
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
           against that trend.",
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
        gap(),
        p("Press m for the plain version, Esc to close."),
    ]
}
