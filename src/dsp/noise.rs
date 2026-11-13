//! Band-limited pink noise for level setting.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use super::Rng;

/// Length of the generated loop; a power of two so it is built in one inverse FFT.
const LOOP_LEN: usize = 1 << 16;

/// Generates a seamlessly looping buffer of pink noise between `low_hz` and `high_hz`.
///
/// The level is set so the RMS reads `rms_dbfs` with the sine = 0 dBFS convention.
/// Band edges roll off over 1/6 octave to avoid ringing.
#[must_use]
pub fn pink_band(rate: u32, low_hz: f64, high_hz: f64, rms_dbfs: f64, seed: u64) -> Vec<f32> {
    let mut planner = RealFftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(LOOP_LEN);
    let mut spectrum = ifft.make_input_vec();
    let mut rng = Rng::new(seed);
    let bin_hz = f64::from(rate) / LOOP_LEN as f64;
    let edge_oct = 1.0 / 6.0;
    let last = spectrum.len() - 1;
    for (k, bin) in spectrum.iter_mut().enumerate().take(last).skip(1) {
        let f = k as f64 * bin_hz;
        let lo = ((f / low_hz).log2() / edge_oct + 0.5).clamp(0.0, 1.0);
        let hi = ((high_hz / f).log2() / edge_oct + 0.5).clamp(0.0, 1.0);
        let taper = (lo * hi * std::f64::consts::FRAC_PI_2).sin().powi(2);
        let mag = taper / f.sqrt();
        let phase = std::f64::consts::TAU * rng.next_f64();
        *bin = Complex::from_polar(mag, phase);
    }
    let mut out = ifft.make_output_vec();
    ifft.process(&mut spectrum, &mut out)
        .expect("spectrum length and zero DC/Nyquist bins match the plan");
    let mean_sq = out.iter().map(|v| v * v).sum::<f64>() / out.len() as f64;
    let target = 10f64.powf(rms_dbfs / 10.0) / 2.0;
    let gain = (target / mean_sq).sqrt();
    out.iter().map(|v| (v * gain) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::level::rms_dbfs;

    fn band_power(x: &[f32], rate: f64, lo: f64, hi: f64) -> f64 {
        let mut planner = RealFftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(x.len());
        let mut input: Vec<f64> = x.iter().map(|&v| f64::from(v)).collect();
        let mut out = fft.make_output_vec();
        fft.process(&mut input, &mut out).expect("valid fft");
        let bin = rate / x.len() as f64;
        out.iter()
            .enumerate()
            .filter(|(k, _)| (*k as f64 * bin) >= lo && (*k as f64 * bin) < hi)
            .map(|(_, c)| c.norm_sqr())
            .sum()
    }

    #[test]
    fn level_and_band_limits() {
        let x = pink_band(48_000, 500.0, 2000.0, -20.0, 7);
        assert!((rms_dbfs(&x) + 20.0).abs() < 0.01);
        let total = band_power(&x, 48_000.0, 0.0, 24_000.0);
        let outside =
            band_power(&x, 48_000.0, 0.0, 300.0) + band_power(&x, 48_000.0, 3500.0, 24_000.0);
        assert!(
            outside / total < 1e-3,
            "out of band fraction {}",
            outside / total
        );
    }

    #[test]
    fn pink_slope_has_equal_power_per_octave() {
        let x = pink_band(48_000, 100.0, 10_000.0, -20.0, 3);
        let a = band_power(&x, 48_000.0, 400.0, 800.0);
        let b = band_power(&x, 48_000.0, 1600.0, 3200.0);
        let diff = 10.0 * (a / b).log10();
        assert!(diff.abs() < 0.5, "octave power difference {diff} dB");
    }
}
