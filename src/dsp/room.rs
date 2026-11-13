//! Room acoustics read from an impulse response: reverberation time per octave and the
//! strongest early reflection. Informational only; the EQ does not use them.

use serde::{Deserialize, Serialize};

use super::biquad::Biquad;

/// Speed of sound at about 20 °C, m/s.
pub const SPEED_OF_SOUND: f64 = 343.0;

/// Octave bands for the reverberation time.
pub const DECAY_BANDS_HZ: [f64; 6] = [125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0];

/// How much of the response after the direct sound the decay analysis uses, s.
const DECAY_SPAN_S: f64 = 0.8;
/// Where the noise floor is taken, after the decay span, s.
const NOISE_SPAN_S: f64 = 0.4;
/// The decay is fitted between these levels below the start (T20), dB.
const FIT_FROM_DB: f64 = -5.0;
const FIT_TO_DB: f64 = -25.0;

/// Reflections are searched this long after the direct sound, s; earlier than the start
/// is still the speaker's own response.
const REFLECTION_FROM_S: f64 = 0.0005;
const REFLECTION_TO_S: f64 = 0.020;

/// The strongest reflection shortly after the direct sound.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Reflection {
    /// Delay after the direct sound, ms.
    pub delay_ms: f64,
    /// Level relative to the direct sound, dB.
    pub level_db: f64,
}

impl Reflection {
    /// How much longer the reflected path is than the direct one, m.
    #[must_use]
    pub fn extra_path_m(&self) -> f64 {
        self.delay_ms / 1000.0 * SPEED_OF_SOUND
    }
}

/// Sample `i` positions after `start` in a circular impulse response.
fn at(ir: &[f64], start: usize, i: usize) -> f64 {
    ir[(start + i) % ir.len()]
}

/// The strongest reflection 0.5–20 ms after the direct sound at `peak`: the highest
/// local peak there (the speaker's own decay just after the direct sound falls steadily
/// and has none).
#[must_use]
pub fn early_reflection(ir: &[f64], peak: usize, rate: f64) -> Option<Reflection> {
    let direct = ir.get(peak)?.abs();
    if direct <= 0.0 {
        return None;
    }
    let from = (REFLECTION_FROM_S * rate) as usize;
    let to = (REFLECTION_TO_S * rate) as usize;
    let around = (0.0001 * rate).max(1.0) as usize;
    let (i, v) = (from..to)
        .map(|i| (i, at(ir, peak, i).abs()))
        .filter(|&(i, v)| {
            (i - around..=i + around)
                .filter(|&j| j != i)
                .all(|j| at(ir, peak, j).abs() < v)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    Some(Reflection {
        delay_ms: i as f64 / rate * 1000.0,
        level_db: 20.0 * (v / direct).log10(),
    })
}

/// Reverberation time (T20, extrapolated to 60 dB) per octave band, `(centre Hz, s)`,
/// by Schroeder backward integration with the noise floor subtracted. Bands whose decay
/// does not clear the noise by enough are left out.
#[must_use]
pub fn decay_times(ir: &[f64], peak: usize, rate: f64) -> Vec<(f64, f64)> {
    let span = (DECAY_SPAN_S * rate) as usize;
    let noise = (NOISE_SPAN_S * rate) as usize;
    // A little before the direct sound, so the band filters settle on silence.
    let lead = (0.005 * rate) as usize;
    let start = (peak + ir.len() - lead) % ir.len();
    let total = lead + span + noise;
    if total >= ir.len() {
        return Vec::new();
    }
    let raw: Vec<f64> = (0..total).map(|i| at(ir, start, i)).collect();
    DECAY_BANDS_HZ
        .iter()
        .filter_map(|&centre| {
            let mut band = raw.clone();
            let q = std::f64::consts::FRAC_1_SQRT_2;
            let hp = Biquad::high_pass(rate, centre / std::f64::consts::SQRT_2, q);
            let lp = Biquad::low_pass(rate, centre * std::f64::consts::SQRT_2, q);
            for f in [hp, hp, lp, lp] {
                f.process(&mut band);
            }
            band_decay(&band, lead, span, rate).map(|t| (centre, t))
        })
        .collect()
}

/// T20 of one band-filtered response: direct sound around `lead`, decay over `span`
/// samples, noise after it.
fn band_decay(band: &[f64], lead: usize, span: usize, rate: f64) -> Option<f64> {
    let energy: Vec<f64> = band.iter().map(|v| v * v).collect();
    let noise_floor =
        energy[lead + span..].iter().sum::<f64>() / (energy.len() - lead - span) as f64;
    // Start at the band's own peak (filters delay it a little).
    let start = (0..lead * 3)
        .max_by(|&a, &b| energy[a].total_cmp(&energy[b]))
        .unwrap_or(lead);
    let end = lead + span;
    // Schroeder integral from the end backwards, noise removed.
    let mut schroeder = vec![0.0; end - start];
    let mut acc = 0.0;
    for i in (start..end).rev() {
        acc += energy[i] - noise_floor;
        schroeder[i - start] = acc;
    }
    let first = schroeder[0];
    if first <= 0.0 {
        return None;
    }
    // Points between −5 and −25 dB, every millisecond.
    let step = (rate / 1000.0).max(1.0) as usize;
    let mut points = Vec::new();
    let mut reached = false;
    for (i, &s) in schroeder.iter().enumerate().step_by(step) {
        if s <= 0.0 {
            break;
        }
        let db = 10.0 * (s / first).log10();
        if db <= FIT_TO_DB {
            reached = true;
            break;
        }
        if db <= FIT_FROM_DB {
            points.push((i as f64 / rate, db));
        }
    }
    if !reached || points.len() < 5 {
        return None;
    }
    // Least-squares slope, dB per second.
    let n = points.len() as f64;
    let (st, sd) = points
        .iter()
        .fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    let (mt, md) = (st / n, sd / n);
    let (cov, var) = points.iter().fold((0.0, 0.0), |a, p| {
        (a.0 + (p.0 - mt) * (p.1 - md), a.1 + (p.0 - mt).powi(2))
    });
    let slope = cov / var;
    (slope < 0.0).then(|| -60.0 / slope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::Rng;

    const RATE: f64 = 48_000.0;

    #[test]
    fn measures_a_known_reverberation_time() {
        // Exponentially decaying noise with a 0.4 s reverberation time, then a noise floor.
        let rt = 0.4;
        let mut rng = Rng::new(3);
        let n = 1 << 17;
        let peak = 1000;
        let ir: Vec<f64> = (0..n)
            .map(|i| {
                let floor = rng.next_gaussian() * 1e-6;
                if i < peak {
                    return floor;
                }
                let t = (i - peak) as f64 / RATE;
                rng.next_gaussian() * 10f64.powf(-3.0 * t / rt) + floor
            })
            .collect();
        let decay = decay_times(&ir, peak, RATE);
        assert_eq!(decay.len(), DECAY_BANDS_HZ.len());
        for (f, t) in decay {
            assert!((t - rt).abs() < 0.04, "{f} Hz: {t:.3} s");
        }
    }

    #[test]
    fn finds_a_desk_reflection() {
        let mut ir = vec![0.0; 1 << 15];
        let peak = 500;
        ir[peak] = 1.0;
        // 1.25 ms later, half as strong: 43 cm of extra path.
        ir[peak + 60] = 0.5;
        ir[peak + 400] = 0.2;
        let r = early_reflection(&ir, peak, RATE).expect("reflection");
        assert!((r.delay_ms - 1.25).abs() < 1e-9);
        assert!((r.level_db + 6.02).abs() < 0.01);
        assert!((r.extra_path_m() - 0.429).abs() < 0.001);
    }
}
