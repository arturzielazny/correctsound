//! The room's lowest resonances (modes) and how long each really rings.
//!
//! The waterfall ([`super::decay`]) looks 400 ms ahead, which is plenty for most of the
//! bass but not for the lowest modes of a lightly damped room: they can take well over a
//! second to die away. Each mode is measured here two independent ways, which agree on
//! real rooms within about 10 %:
//!
//! - **Energy decay**: the impulse response is shifted so the mode sits at 0 Hz (complex
//!   demodulation) and low-passed steeply (8th order, about ±2 Hz), which isolates it from
//!   a neighbouring mode far better than a band-pass of the same width could (a 4th-order
//!   sixth-octave band-pass lets a stronger, slower mode half an octave away through, and
//!   its decay then sets the slope). The envelope is integrated backwards (Schroeder) with
//!   the noise floor removed, and the slope fitted over as much of the decay as clears the
//!   noise (T30, T20 or T15). The filter is run backwards in time, so its own ringing falls
//!   before the direct sound instead of lengthening the decay.
//! - **Peak width**: a single mode is a resonance whose decay sets the width of its peak in
//!   the frequency response: T60 = 3·ln(10) / (π·width) ≈ 2.2 s·Hz / width (−3 dB width). The
//!   first four seconds of the impulse response resolve 0.05 Hz.
//!
//! The energy decay is what is reported; the width is the cross-check.

use realfft::RealFftPlanner;

use super::biquad::Biquad;

/// Modes are looked for in this range, Hz. Above it modes overlap too much to tell apart
/// (the Schroeder frequency of a living room is about 150–250 Hz).
pub const FROM_HZ: f64 = 20.0;
pub const TO_HZ: f64 = 150.0;
/// A peak counts as a mode when it rises this far above the lowest point on each side
/// between it and the next higher one (or a sixth of an octave away), dB. Modes below the
/// speaker's roll-off sit on a slope and rise only a few dB on the upper side.
const MIN_PROMINENCE_DB: f64 = 4.0;
/// How much impulse response is used after the direct sound, s.
const SPAN_S: f64 = 4.0;
/// Its last second is the noise floor for the energy decay; the second after it, the
/// noise floor of the spectrum.
const NOISE_S: f64 = 1.0;
/// A mode's peak must stand this far above the noise floor of the spectrum, dB.
const MIN_ABOVE_NOISE_DB: f64 = 15.0;
/// The longest decay the four seconds analysed can tell, s. A peak narrower than that is a
/// steady tone in the room (a hum below the speakers' range), not a mode dying away.
const MAX_T60_S: f64 = 3.5;
/// Decay range above the noise that allows at least a T20 fit, dB; below it the fit is a
/// short T15 and the peak width is the safer of the two when they disagree.
const FULL_FIT_RANGE_DB: f64 = 35.0;
/// Cut-off of the energy-decay low-pass after demodulation, as a share of the mode's
/// frequency, and at least [`MIN_CUTOFF_HZ`]: wide enough to pass the decay itself (a
/// mode with a 1 s T60 is 0.7 Hz wide), narrow enough to shut out the next mode.
const CUTOFF_SHARE: f64 = 0.06;
const MIN_CUTOFF_HZ: f64 = 2.0;
/// Second-order sections in that low-pass: 8th order in all.
const CUTOFF_SECTIONS: usize = 4;
/// FFT length for the peak widths: 2^20, 0.046 Hz bins at 48 kHz.
const FFT_LEN: usize = 1 << 20;
/// Most modes reported.
const MOST: usize = 6;
/// T60 from the −3 dB width of a single resonance: 3·ln(10)/π, s·Hz.
const WIDTH_TO_T60: f64 = 3.0 * std::f64::consts::LN_10 / std::f64::consts::PI;

/// One room mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    /// Its frequency, Hz.
    pub hz: f64,
    /// Time to decay 60 dB, s, from the energy decay (or from the peak width when the decay
    /// does not clear the noise far enough to fit).
    pub t60_s: f64,
    /// The same from the width of its peak, s.
    pub t60_from_width_s: f64,
    /// How far it stands above its surroundings, dB.
    pub prominence_db: f64,
}

impl Mode {
    /// Time to fade 20 dB, ms: what the waterfall measures.
    #[must_use]
    pub fn fade_20_ms(&self) -> f64 {
        self.t60_s / 3.0 * 1000.0
    }

    /// True when the two measurements agree within a third.
    #[must_use]
    pub fn confirmed(&self) -> bool {
        (self.t60_s / self.t60_from_width_s - 1.0).abs() < 1.0 / 3.0
    }

    /// The T60 as text: "1.7 s", or the range of the two measurements when they disagree:
    /// "1.7–3.2 s".
    #[must_use]
    pub fn t60_text(&self) -> String {
        if self.confirmed() {
            format!("{:.1} s", self.t60_s)
        } else {
            let (a, b) = (
                self.t60_s.min(self.t60_from_width_s),
                self.t60_s.max(self.t60_from_width_s),
            );
            format!("{a:.1}–{b:.1} s")
        }
    }

    /// Time to fade 20 dB as text, like [`Self::t60_text`]: "570 ms".
    #[must_use]
    pub fn fade_20_text(&self) -> String {
        let ms = |t60: f64| t60 / 3.0 * 1000.0;
        if self.confirmed() {
            format!("{:.0} ms", self.fade_20_ms())
        } else {
            let (a, b) = (
                self.t60_s.min(self.t60_from_width_s),
                self.t60_s.max(self.t60_from_width_s),
            );
            format!("{:.0}–{:.0} ms", ms(a), ms(b))
        }
    }
}

/// The mode within a twelfth of an octave of `hz`, if any.
#[must_use]
pub fn near(modes: &[Mode], hz: f64) -> Option<&Mode> {
    modes
        .iter()
        .filter(|m| (m.hz / hz).log2().abs() < 1.0 / 12.0)
        .min_by(|a, b| {
            (a.hz / hz)
                .log2()
                .abs()
                .total_cmp(&(b.hz / hz).log2().abs())
        })
}

/// The impulse response from `from` to `to` seconds around the direct sound at `peak`,
/// circularly.
fn stretch(ir: &[f64], peak: usize, from: f64, to: f64, rate: f64) -> Vec<f64> {
    let n = ir.len() as isize;
    let (a, b) = ((from * rate) as isize, (to * rate) as isize);
    (a..b)
        .map(|i| ir[((peak as isize + i).rem_euclid(n)) as usize])
        .collect()
}

/// Finds the prominent resonances between [`FROM_HZ`] and [`TO_HZ`] in an impulse response
/// whose direct sound is at `peak`, the most prominent first. The response must reach at
/// least six and a half seconds past the direct sound before wrapping around (a full
/// sweep's does: its distortion products sit in the last 1.5 s); a shorter one gives none.
#[must_use]
pub fn find(ir: &[f64], peak: usize, rate: f64) -> Vec<Mode> {
    if (ir.len() as f64) < (SPAN_S + NOISE_S + 1.5) * rate {
        return Vec::new();
    }
    // High-resolution magnitude, the end tapered over half a second.
    let lead = 0.05;
    let x = stretch(ir, peak, -lead, SPAN_S, rate);
    let taper = 0.5 * rate;
    let weights: Vec<f64> = (0..x.len())
        .map(|i| ((x.len() - i) as f64 / taper).min(1.0))
        .collect();
    let db = spectrum_db(x.iter().zip(&weights).map(|(v, w)| v * w));
    // The noise floor: a second of response after the room has died away, scaled to the
    // same window energy, and averaged over ±1 Hz so it is a floor and not noise itself.
    let late = stretch(ir, peak, SPAN_S, SPAN_S + NOISE_S, rate);
    let scale = weights.iter().map(|w| w * w).sum::<f64>() / late.len() as f64;
    let noise_bins = spectrum_db(late.into_iter());
    let bin = rate / FFT_LEN as f64;
    let half = (1.0 / bin) as usize;
    let floor = |k: usize| {
        let (a, b) = (k.saturating_sub(half), (k + half + 1).min(noise_bins.len()));
        let mean = noise_bins[a..b]
            .iter()
            .map(|&d| super::db_to_power(d))
            .sum::<f64>()
            / (b - a) as f64;
        super::power_to_db(mean * scale)
    };

    let mut modes: Vec<Mode> = Vec::new();
    let (k0, k1) = ((FROM_HZ / bin) as usize, (TO_HZ / bin) as usize);
    let near = (0.5 / bin).max(1.0) as usize;
    for k in k0..k1 {
        if !(k - near..=k + near).all(|j| j == k || db[j] < db[k]) {
            continue;
        }
        let f = k as f64 * bin;
        let noise = floor(k);
        if db[k] < noise + MIN_ABOVE_NOISE_DB {
            continue;
        }
        let prominence = prominence_db(&db, k, bin, noise);
        if prominence < MIN_PROMINENCE_DB {
            continue;
        }
        let Some(width) = half_power_width(&db, k, bin) else {
            continue;
        };
        let t60_from_width_s = WIDTH_TO_T60 / width;
        if t60_from_width_s > MAX_T60_S {
            continue;
        }
        // The energy decay, unless it had too little room above the noise for a full fit
        // and disagrees with the width (low modes in a noisy room).
        let t60_s = match energy_decay(ir, peak, f, rate) {
            Some((t, range))
                if t <= MAX_T60_S
                    && (range >= FULL_FIT_RANGE_DB
                        || (t / t60_from_width_s - 1.0).abs() < 1.0 / 3.0) =>
            {
                t
            }
            _ => t60_from_width_s,
        };
        modes.push(Mode {
            hz: f,
            t60_s,
            t60_from_width_s,
            prominence_db: prominence,
        });
    }
    modes.sort_by(|a, b| b.prominence_db.total_cmp(&a.prominence_db));
    modes.truncate(MOST);
    modes
}

/// Power spectrum of `samples`, zero-padded to [`FFT_LEN`], dB.
fn spectrum_db(samples: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut buf = vec![0.0; FFT_LEN];
    for (b, v) in buf.iter_mut().zip(samples) {
        *b = v;
    }
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(FFT_LEN);
    let mut out = fft.make_output_vec();
    fft.process(&mut buf, &mut out)
        .expect("buffer sizes match the plan");
    out.iter()
        .map(|c| super::power_to_db(c.norm_sqr()))
        .collect()
}

/// How far the peak at bin `k` rises above the higher of its two valleys: the lowest
/// point on each side before the response climbs above the peak, or a sixth of an
/// octave away. A mode on the skirt of a louder one still has its own valley between them.
/// Valleys count no deeper than the noise floor `noise`, whose random dips would otherwise
/// make a ripple of noise look like a peak.
fn prominence_db(db: &[f64], k: usize, bin: f64, noise: f64) -> f64 {
    let reach = ((k as f64 * bin * (2f64.powf(1.0 / 6.0) - 1.0)) / bin) as usize;
    let valley = |range: &mut dyn Iterator<Item = usize>| {
        range
            .take_while(|&j| db[j] <= db[k])
            .map(|j| db[j])
            .fold(db[k], f64::min)
            .max(noise)
    };
    let left = valley(&mut (k.saturating_sub(reach)..k).rev());
    let right = valley(&mut (k + 1..(k + reach + 1).min(db.len())));
    db[k] - left.max(right)
}

/// The −3 dB width of the peak at bin `k`, Hz, with the crossings interpolated.
fn half_power_width(db: &[f64], k: usize, bin: f64) -> Option<f64> {
    let half = db[k] - 3.0;
    let mut lo = k;
    while db[lo] > half {
        lo = lo.checked_sub(1)?;
    }
    let mut hi = k;
    while db[hi] > half {
        hi += 1;
        if hi >= db.len() {
            return None;
        }
    }
    let f_lo = lo as f64 + (half - db[lo]) / (db[lo + 1] - db[lo]);
    let f_hi = (hi - 1) as f64 + (db[hi - 1] - half) / (db[hi - 1] - db[hi]);
    Some((f_hi - f_lo) * bin).filter(|w| *w > 0.0)
}

/// T60 of the energy around `hz`, s, from the demodulated, backwards-filtered,
/// backwards-integrated decay, and how far the decay started above the noise, dB; `None`
/// when that is less than 28 dB.
fn energy_decay(ir: &[f64], peak: usize, hz: f64, rate: f64) -> Option<(f64, f64)> {
    let lead = 0.2;
    let x = stretch(ir, peak, -lead, SPAN_S, rate);
    let w = std::f64::consts::TAU * hz / rate;
    let lp = Biquad::low_pass(
        rate,
        (hz * CUTOFF_SHARE).max(MIN_CUTOFF_HZ),
        std::f64::consts::FRAC_1_SQRT_2,
    );
    // The in-phase and quadrature parts, each low-passed backwards in time.
    let part = |phase: f64| {
        let mut v: Vec<f64> = x
            .iter()
            .enumerate()
            .map(|(i, s)| s * (w * i as f64 + phase).cos())
            .rev()
            .collect();
        for _ in 0..CUTOFF_SECTIONS {
            lp.process(&mut v);
        }
        v.reverse();
        v
    };
    let (re, im) = (part(0.0), part(-std::f64::consts::FRAC_PI_2));
    let energy: Vec<f64> = re.iter().zip(&im).map(|(a, b)| a * a + b * b).collect();
    let noise_len = (NOISE_S * rate) as usize;
    let end = energy.len() - noise_len;
    let noise = energy[end..].iter().sum::<f64>() / noise_len as f64;
    // From the band's loudest moment just after the direct sound.
    let l = (lead * rate) as usize;
    let start = (l..l + (0.05 * rate) as usize).max_by(|&a, &b| energy[a].total_cmp(&energy[b]))?;
    let mut schroeder = vec![0.0; end - start];
    let mut acc = 0.0;
    for i in (start..end).rev() {
        acc += energy[i] - noise;
        schroeder[i - start] = acc;
    }
    let first = schroeder[0];
    if first <= 0.0 || noise <= 0.0 {
        return None;
    }
    // How far the decay clears the noise: decide how much of it to fit.
    // How far the decay starts above the noise (its first 20 ms against the floor), as
    // for a reverberation time: a fit needs 10 dB of room below its end.
    let head = (0.02 * rate) as usize;
    let range =
        super::power_to_db(energy[start..start + head].iter().sum::<f64>() / head as f64 / noise);
    let to = if range >= 45.0 {
        -35.0
    } else if range >= FULL_FIT_RANGE_DB {
        -25.0
    } else if range >= 28.0 {
        -20.0
    } else {
        return None;
    };
    let step = (rate / 1000.0).max(1.0) as usize;
    let points: Vec<(f64, f64)> = schroeder
        .iter()
        .enumerate()
        .step_by(step)
        .take_while(|(_, s)| **s > 0.0)
        .map(|(i, &s)| (i as f64 / rate, super::power_to_db(s / first)))
        .filter(|&(_, d)| d <= -5.0 && d >= to)
        .collect();
    if points.len() < 10 {
        return None;
    }
    let n = points.len() as f64;
    let (mt, md) = points
        .iter()
        .fold((0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n));
    let (cov, var) = points.iter().fold((0.0, 0.0), |a, p| {
        (a.0 + (p.0 - mt) * (p.1 - md), a.1 + (p.0 - mt).powi(2))
    });
    let slope = cov / var;
    (slope < 0.0).then(|| (-60.0 / slope, range))
}

/// The modes of several measurements of one speaker (mic positions), merged: a mode's
/// frequency and decay belong to the room, so each is the median over the positions that
/// found it within a fortieth of an octave. A real mode is heard from anywhere, so one found
/// by fewer than half the positions is left out. Sorted by frequency.
#[must_use]
pub fn merge(all: &[&[Mode]]) -> Vec<Mode> {
    let mut groups: Vec<Vec<Mode>> = Vec::new();
    for m in all.iter().flat_map(|ms| ms.iter()) {
        match groups
            .iter_mut()
            .find(|g| (g[0].hz / m.hz).log2().abs() < 1.0 / 40.0)
        {
            Some(g) => g.push(*m),
            None => groups.push(vec![*m]),
        }
    }
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let needed = all.len().div_ceil(2);
    let mut merged: Vec<Mode> = groups
        .into_iter()
        .filter(|g| g.len() >= needed)
        .map(|g| Mode {
            hz: median(g.iter().map(|m| m.hz).collect()),
            t60_s: median(g.iter().map(|m| m.t60_s).collect()),
            t60_from_width_s: median(g.iter().map(|m| m.t60_from_width_s).collect()),
            prominence_db: median(g.iter().map(|m| m.prominence_db).collect()),
        })
        .collect();
    merged.sort_by(|a, b| a.hz.total_cmp(&b.hz));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Rng;

    const RATE: f64 = 48_000.0;

    /// An impulse response: the direct sound, two room modes ringing with known decay times,
    /// a broad low-frequency hump that is not a mode, and noise.
    fn room(modes: &[(f64, f64, f64)], noise: f64) -> (Vec<f64>, usize) {
        let n = 1 << 19;
        let peak = 2400;
        let mut rng = Rng::new(9);
        let mut ir: Vec<f64> = (0..n).map(|_| noise * rng.next_gaussian()).collect();
        ir[peak] += 1.0;
        for &(hz, t60, amp) in modes {
            let decay = 3.0 * std::f64::consts::LN_10 / t60;
            for (i, v) in ir[peak..].iter_mut().enumerate() {
                let t = i as f64 / RATE;
                *v += amp * (-decay * t).exp() * (std::f64::consts::TAU * hz * t).sin();
            }
        }
        // A heavily damped "resonance" at 70 Hz (T60 0.1 s): broad, not a mode.
        let decay = 3.0 * std::f64::consts::LN_10 / 0.1;
        for (i, v) in ir[peak..].iter_mut().enumerate() {
            let t = i as f64 / RATE;
            *v += 0.01 * (-decay * t).exp() * (std::f64::consts::TAU * 70.0 * t).sin();
        }
        (ir, peak)
    }

    #[test]
    fn measures_known_mode_decays_both_ways() {
        let (ir, peak) = room(&[(32.7, 1.6, 0.01), (45.0, 1.0, 0.01)], 1e-7);
        let found = merge(&[&find(&ir, peak, RATE)]);
        assert_eq!(found.len(), 2, "{found:?}");
        for (m, (hz, t60)) in found.iter().zip([(32.7, 1.6), (45.0, 1.0)]) {
            eprintln!("{hz} Hz, T60 {t60} s: {m:?}");
            assert!((m.hz - hz).abs() < 0.1, "{m:?}");
            assert!((m.t60_s / t60 - 1.0).abs() < 0.08, "energy decay {m:?}");
            assert!((m.t60_from_width_s / t60 - 1.0).abs() < 0.08, "width {m:?}");
            assert!(m.confirmed());
            assert!((m.fade_20_ms() - m.t60_s / 3.0 * 1000.0).abs() < 1e-9);
        }
    }

    #[test]
    fn tells_apart_close_modes_with_different_decays() {
        // A fifth of an octave apart: a long, quiet one next to a short, loud one.
        let (ir, peak) = room(&[(40.0, 1.8, 0.006), (46.0, 0.6, 0.02)], 1e-7);
        let found = merge(&[&find(&ir, peak, RATE)]);
        for (hz, t60) in [(40.0, 1.8), (46.0, 0.6)] {
            let m = found
                .iter()
                .find(|m| (m.hz - hz).abs() < 0.3)
                .unwrap_or_else(|| panic!("{hz} Hz in {found:?}"));
            eprintln!("{hz} Hz, T60 {t60} s: {m:?}");
            assert!((m.t60_s / t60 - 1.0).abs() < 0.15, "energy decay {m:?}");
            assert!((m.t60_from_width_s / t60 - 1.0).abs() < 0.15, "width {m:?}");
        }
    }

    #[test]
    fn a_noisy_decay_falls_back_on_the_peak_width() {
        // The noise is so high the decay clears it by less than 28 dB, even in the narrow
        // band around the mode.
        let (ir, peak) = room(&[(32.7, 1.6, 0.01)], 1e-2);
        let m = find(&ir, peak, RATE);
        let mode = m
            .iter()
            .find(|m| (m.hz - 32.7).abs() < 0.2)
            .unwrap_or_else(|| panic!("{m:?}"));
        assert_eq!(mode.t60_s, mode.t60_from_width_s, "{mode:?}");
        assert!((mode.t60_s / 1.6 - 1.0).abs() < 0.2, "{mode:?}");
        // Ripples of the noise are not taken for modes.
        assert_eq!(m.len(), 1, "{m:?}");
    }

    #[test]
    fn a_mode_heard_from_one_position_of_three_is_left_out() {
        let mode = |hz: f64| Mode {
            hz,
            t60_s: 1.0,
            t60_from_width_s: 1.0,
            prominence_db: 10.0,
        };
        let (a, b, c) = (
            vec![mode(32.7), mode(45.0)],
            vec![mode(32.8), mode(30.4)],
            vec![mode(32.75)],
        );
        let merged = merge(&[&a, &b, &c]);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert!((merged[0].hz - 32.75).abs() < 1e-9);
        // With one position, everything it found counts.
        assert_eq!(merge(&[&a]).len(), 2);
    }

    #[test]
    fn a_short_impulse_response_has_no_modes() {
        let (ir, peak) = room(&[(32.7, 1.6, 0.01)], 1e-7);
        assert!(find(&ir[..48_000], peak, RATE).is_empty());
    }
}
