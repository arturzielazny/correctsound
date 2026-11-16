//! Exponential sine sweeps, deconvolution and impulse-response windowing.

use std::f64::consts::{PI, TAU};

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use super::{amplitude_to_db, next_pow2};

/// Parameters of an exponential (Farina) sweep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepParams {
    pub rate: u32,
    pub start_hz: f64,
    pub end_hz: f64,
    pub len: usize,
    /// Peak level in dBFS.
    pub peak_dbfs: f64,
}

impl Default for SweepParams {
    fn default() -> Self {
        Self {
            rate: super::SAMPLE_RATE,
            start_hz: 10.0,
            end_hz: 22_000.0,
            len: 1 << 18,
            peak_dbfs: -12.0,
        }
    }
}

/// A generated sweep and its regularised inverse spectrum cache.
#[derive(Debug, Clone)]
pub struct Sweep {
    params: SweepParams,
    samples: Vec<f64>,
}

impl Sweep {
    /// Generates the sweep with raised-cosine fades (50 ms in, 5 ms out).
    #[must_use]
    pub fn new(params: SweepParams) -> Self {
        let rate = f64::from(params.rate);
        let duration = params.len as f64 / rate;
        let l = duration / (params.end_hz / params.start_hz).ln();
        let amp = super::db_to_amplitude(params.peak_dbfs);
        let fade_in = (0.05 * rate) as usize;
        let fade_out = (0.005 * rate) as usize;
        let samples = (0..params.len)
            .map(|i| {
                let t = i as f64 / rate;
                let mut v = amp * (TAU * params.start_hz * l * ((t / l).exp() - 1.0)).sin();
                if i < fade_in {
                    v *= 0.5 - 0.5 * (PI * i as f64 / fade_in as f64).cos();
                }
                let from_end = params.len - 1 - i;
                if from_end < fade_out {
                    v *= 0.5 - 0.5 * (PI * from_end as f64 / fade_out as f64).cos();
                }
                v
            })
            .collect();
        Self { params, samples }
    }

    #[must_use]
    pub fn params(&self) -> &SweepParams {
        &self.params
    }

    /// The sweep as playback samples.
    #[must_use]
    pub fn samples_f32(&self) -> Vec<f32> {
        self.samples.iter().map(|&v| v as f32).collect()
    }

    /// Deconvolves a recording into an impulse response of length `next_pow2(rec + sweep)`.
    ///
    /// Frequencies outside the swept band are suppressed by Kirkeby regularisation.
    #[must_use]
    pub fn deconvolve(&self, recording: &[f32]) -> Vec<f64> {
        let n = next_pow2(recording.len() + self.samples.len());
        let mut planner = RealFftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);

        let mut x = vec![0.0; n];
        x[..self.samples.len()].copy_from_slice(&self.samples);
        let mut xs = fft.make_output_vec();
        fft.process(&mut x, &mut xs)
            .expect("buffer sizes match the plan");

        let mut y = vec![0.0; n];
        for (d, &s) in y.iter_mut().zip(recording) {
            *d = f64::from(s);
        }
        let mut ys = fft.make_output_vec();
        fft.process(&mut y, &mut ys)
            .expect("buffer sizes match the plan");

        let rate = f64::from(self.params.rate);
        let bin_hz = rate / n as f64;
        let max_pow = xs.iter().map(Complex::norm_sqr).fold(0.0, f64::max);
        let lo = self.params.start_hz * 1.2;
        let hi = self.params.end_hz.min(rate / 2.0) / 1.05;
        let last = xs.len() - 1;
        for (k, (yk, xk)) in ys.iter_mut().zip(&xs).enumerate() {
            if k == 0 || k == last {
                *yk = Complex::new(0.0, 0.0);
                continue;
            }
            let f = k as f64 * bin_hz;
            // 0 inside the band, 1 one third-octave outside it.
            let out = (((lo / f).log2().max((f / hi).log2())) * 3.0).clamp(0.0, 1.0);
            let eps = max_pow * (1e-7 + out * 1.0);
            *yk = *yk * xk.conj() / (xk.norm_sqr() + eps);
        }
        let mut ir = ifft.make_output_vec();
        ifft.process(&mut ys, &mut ir)
            .expect("buffer sizes match the plan");
        let scale = 1.0 / n as f64;
        ir.iter_mut().for_each(|v| *v *= scale);
        ir
    }
}

/// Window applied around the impulse-response peak before taking its spectrum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IrWindow {
    /// Rising half-Hann before the peak; long enough to keep the pre-ringing of the
    /// regularised low band edge (≈10 Hz), as REW does with its 125 ms default.
    pub pre_ms: f64,
    pub flat_ms: f64,
    pub total_ms: f64,
}

impl Default for IrWindow {
    fn default() -> Self {
        Self {
            pre_ms: 125.0,
            flat_ms: 250.0,
            total_ms: 500.0,
        }
    }
}

/// FFT length used for the windowed impulse response (0.73 Hz bins at 48 kHz).
pub const SPECTRUM_FFT_LEN: usize = 1 << 16;

/// Where the noise in the bass octaves is measured in the impulse response, ms after the
/// direct sound: late enough that the room has stopped ringing, so the margin is the
/// background noise's and not the room's own decay.
const NOISE_AFTER_MS: f64 = 2000.0;
/// Octaves centred below this use that late noise window, Hz. For the default sweep its
/// stretch of recording ends 2 s before the recording does (250 Hz octave: at 5.6 s of
/// 7.5 s).
const LATE_NOISE_BELOW_HZ: f64 = 300.0;

/// Result of analysing one sweep recording.
#[derive(Debug, Clone)]
pub struct SweepAnalysis {
    /// Power spectrum `|H(f)|²` of the windowed IR, one value per FFT bin.
    pub power: Vec<f64>,
    pub bin_hz: f64,
    /// Sample index in the recording where the sweep arrives.
    pub delay_samples: usize,
    pub capture_peak_dbfs: f64,
    /// IR peak to IR noise ratio, dB.
    pub ir_snr_db: f64,
    /// Signal-to-noise ratio of the response per octave band: `(centre Hz, dB)`.
    pub octave_snr_db: Vec<(f64, f64)>,
    /// The strongest reflection shortly after the direct sound (informational).
    pub reflection: Option<super::room::Reflection>,
    /// Reverberation time per octave band, `(centre Hz, s)` (informational).
    pub decay: Vec<(f64, f64)>,
    /// How the bass fades after the direct sound (informational).
    pub waterfall: super::decay::Waterfall,
    /// The room's lowest resonances and how long each rings (informational).
    pub modes: Vec<super::modes::Mode>,
    /// The bass at high resolution, complex, for comparing speakers and fitting a model of
    /// the room (none for a recording too short).
    pub bass: Option<super::modes::BassSpectrum>,
}

/// Octave bands whose signal-to-noise ratio gates a measurement.
pub const SNR_BANDS_HZ: [f64; 8] = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];
/// Lowest acceptable per-octave SNR, dB (noise then moves the response by under ±1 dB).
pub const MIN_OCTAVE_SNR_DB: f64 = 20.0;

/// Problems that make a sweep unusable.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SweepIssue {
    #[error(
        "the mic signal clipped (peak {peak_dbfs:.1} dBFS); turn the amplifier down a little and measure again"
    )]
    Clipped { peak_dbfs: f64 },
    #[error(
        "around {freq_hz:.0} Hz the sweep was only {margin_db:.0} dB above the background noise; turn the amplifier up, or make the room quieter"
    )]
    TooQuiet { margin_db: f64, freq_hz: f64 },
    #[error(
        "no sweep was detected in the recording; check the selected output, amplifier power and mute, and that the mic points at the speakers"
    )]
    NoSignal,
}

impl SweepAnalysis {
    /// Analyses a recording of `sweep`.
    ///
    /// # Errors
    ///
    /// Returns a [`SweepIssue`] when the recording clipped, was too quiet, or held no sweep.
    pub fn analyze(sweep: &Sweep, recording: &[f32], window: IrWindow) -> Result<Self, SweepIssue> {
        let capture_peak_dbfs = super::level::peak_dbfs(recording);
        if capture_peak_dbfs > -1.0 {
            return Err(SweepIssue::Clipped {
                peak_dbfs: capture_peak_dbfs,
            });
        }
        let rate = f64::from(sweep.params.rate);
        let ir = sweep.deconvolve(recording);
        let n = ir.len();
        let peak = ir
            .iter()
            .enumerate()
            .fold(
                (0, 0.0),
                |(bi, bv), (i, &v)| if v.abs() > bv { (i, v.abs()) } else { (bi, bv) },
            )
            .0;

        let ms = |m: f64| (m * rate / 1000.0) as usize;
        // IR noise: RMS of a stretch well after the window, before any wrap-around.
        let noise_start = peak + ms(window.total_ms + 100.0);
        let noise_len = ms(400.0);
        let ir_noise = (0..noise_len)
            .map(|i| ir[(noise_start + i) % n].powi(2))
            .sum::<f64>()
            / noise_len as f64;
        let ir_snr_db = amplitude_to_db(ir[peak]) - 10.0 * ir_noise.max(1e-30).log10();
        if ir_snr_db < 25.0 || ir[peak].abs() < 1e-7 {
            return Err(SweepIssue::NoSignal);
        }

        let power = windowed_power(&ir, peak, window, rate);
        // The same window placed on a stretch of IR that holds only noise: this is exactly
        // the noise that ends up in `power`. Just after the window for the treble, where
        // the room is quiet again within a few hundred ms; for the bass, NOISE_AFTER_MS
        // after the direct sound, when even a boomy room has stopped ringing (in real rooms
        // a 50 Hz mode is still 40 dB up at 0.6 s). Both are well before the distortion
        // products, which wrap around to the last 1.5 s of the IR. The late window cannot
        // serve the treble: the IR at a given time is made from the recording later on by
        // the time the sweep took to reach that frequency, and for the treble that is past
        // the end of the recording, where there is no noise to measure.
        let early = peak + ms(window.pre_ms + window.total_ms + 100.0);
        let late = peak + ms(NOISE_AFTER_MS);
        let noise_early = windowed_power(&ir, early % n, window, rate);
        let noise_late = windowed_power(&ir, late % n, window, rate);
        let bin_hz = rate / SPECTRUM_FFT_LEN as f64;
        let octave_snr_db: Vec<(f64, f64)> = SNR_BANDS_HZ
            .iter()
            .map(|&c| {
                let noise = if c < LATE_NOISE_BELOW_HZ {
                    &noise_late
                } else {
                    &noise_early
                };
                (c, band_snr_db(&power, noise, bin_hz, c))
            })
            .collect();
        if let Some(&(freq_hz, margin_db)) = octave_snr_db
            .iter()
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|(_, snr)| *snr < MIN_OCTAVE_SNR_DB)
        {
            return Err(SweepIssue::TooQuiet { margin_db, freq_hz });
        }

        let delay = if peak < recording.len() { peak } else { 0 };
        let (modes, bass) = super::modes::analyse(&ir, peak, rate);
        Ok(Self {
            power,
            bin_hz,
            delay_samples: delay,
            capture_peak_dbfs,
            ir_snr_db,
            octave_snr_db,
            reflection: super::room::early_reflection(&ir, peak, rate),
            decay: super::room::decay_times(&ir, peak, rate),
            waterfall: super::decay::Waterfall::new(&ir, peak, rate),
            modes,
            bass,
        })
    }
}

/// A quick look at one recording, for the live placement loop.
#[derive(Debug, Clone)]
pub struct QuickAnalysis {
    /// Windowed power spectrum per FFT bin ([`SPECTRUM_FFT_LEN`] long).
    pub power: Vec<f64>,
    /// Width of one bin, Hz.
    pub bin_hz: f64,
    /// How the bass decays after the sweep.
    pub waterfall: super::decay::Waterfall,
}

/// Analyses one recording for the live placement loop: the windowed power spectrum and the
/// bass waterfall. Only clipping and a missing sweep are rejected; the per-octave checks of
/// a full measurement are skipped.
///
/// # Errors
///
/// Returns [`SweepIssue::Clipped`] or [`SweepIssue::NoSignal`].
pub fn quick_analysis(sweep: &Sweep, recording: &[f32]) -> Result<QuickAnalysis, SweepIssue> {
    check_clipping(recording)?;
    quick_ir_analysis(&sweep.deconvolve(recording), f64::from(sweep.params.rate))
}

/// Rejects a recording that clipped.
///
/// # Errors
///
/// Returns [`SweepIssue::Clipped`].
pub fn check_clipping(recording: &[f32]) -> Result<(), SweepIssue> {
    let capture_peak_dbfs = super::level::peak_dbfs(recording);
    if capture_peak_dbfs > -1.0 {
        return Err(SweepIssue::Clipped {
            peak_dbfs: capture_peak_dbfs,
        });
    }
    Ok(())
}

/// [`quick_analysis`] of an impulse response that is already known, such as the circular
/// one of a looped sweep ([`crate::dsp::periodic::Periodic::impulse_response`]). It must be
/// at least a second long; its direct sound is its largest sample.
///
/// # Errors
///
/// Returns [`SweepIssue::NoSignal`] when the direct sound does not stand out.
pub fn quick_ir_analysis(ir: &[f64], rate: f64) -> Result<QuickAnalysis, SweepIssue> {
    let peak = ir
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .map_or(0, |(i, _)| i);
    let window = IrWindow::default();
    let ms = |m: f64| (m * rate / 1000.0) as usize;
    let noise_start = peak + ms(window.total_ms + 100.0);
    let noise_len = ms(200.0);
    let n = ir.len();
    let noise = (0..noise_len)
        .map(|i| ir[(noise_start + i) % n].powi(2))
        .sum::<f64>()
        / noise_len as f64;
    if amplitude_to_db(ir[peak]) - 10.0 * noise.max(1e-30).log10() < 25.0 {
        return Err(SweepIssue::NoSignal);
    }
    Ok(QuickAnalysis {
        power: windowed_power(ir, peak, window, rate),
        bin_hz: rate / SPECTRUM_FFT_LEN as f64,
        waterfall: super::decay::Waterfall::new(ir, peak, rate),
    })
}

/// Ratio of signal to noise power in the octave band centred on `centre`, dB.
fn band_snr_db(signal: &[f64], noise: &[f64], bin_hz: f64, centre: f64) -> f64 {
    let lo = (centre / std::f64::consts::SQRT_2 / bin_hz).ceil() as usize;
    let hi = ((centre * std::f64::consts::SQRT_2 / bin_hz).floor() as usize).min(signal.len() - 1);
    let sum = |v: &[f64]| v[lo..=hi].iter().sum::<f64>();
    10.0 * (sum(signal) / sum(noise).max(1e-30)).log10()
}

/// Windows the circular IR around `peak` and returns its power spectrum.
fn windowed_power(ir: &[f64], peak: usize, window: IrWindow, rate: f64) -> Vec<f64> {
    let n = ir.len();
    let ms = |m: f64| ((m * rate / 1000.0) as usize).max(1);
    let pre = ms(window.pre_ms);
    let flat = ms(window.flat_ms);
    let total = ms(window.total_ms).min(SPECTRUM_FFT_LEN - pre);
    let mut buf = vec![0.0; SPECTRUM_FFT_LEN];
    for i in 0..pre {
        let w = 0.5 - 0.5 * (PI * i as f64 / pre as f64).cos();
        buf[i] = ir[(peak + n - pre + i) % n] * w;
    }
    for i in 0..total {
        let w = if i < flat {
            1.0
        } else {
            0.5 + 0.5 * (PI * (i - flat) as f64 / (total - flat) as f64).cos()
        };
        buf[pre + i] = ir[(peak + i) % n] * w;
    }
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(SPECTRUM_FFT_LEN);
    let mut out = fft.make_output_vec();
    fft.process(&mut buf, &mut out)
        .expect("buffer sizes match the plan");
    out.iter().map(Complex::norm_sqr).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_system_is_flat() {
        let sweep = Sweep::new(SweepParams::default());
        let delay = 30_000;
        let mut rec = vec![0f32; delay];
        rec.extend(sweep.samples_f32());
        rec.extend(vec![0f32; 72_000]);
        // A tiny noise floor so the SNR checks have something finite to compare against.
        let mut rng = crate::dsp::Rng::new(1);
        for v in &mut rec {
            *v += (rng.next_gaussian() * 1e-6) as f32;
        }
        let a = SweepAnalysis::analyze(&sweep, &rec, IrWindow::default()).expect("clean sweep");
        assert_eq!(a.delay_samples, delay);
        for (k, &p) in a.power.iter().enumerate() {
            let f = k as f64 * a.bin_hz;
            if (20.0..=20_000.0).contains(&f) {
                let db = crate::dsp::power_to_db(p);
                assert!(db.abs() < 0.05, "{f:.1} Hz: {db:.3} dB");
            }
        }
    }

    #[test]
    fn silence_is_rejected() {
        let sweep = Sweep::new(SweepParams::default());
        let mut rng = crate::dsp::Rng::new(2);
        let rec: Vec<f32> = (0..400_000)
            .map(|_| (rng.next_gaussian() * 1e-3) as f32)
            .collect();
        assert!(SweepAnalysis::analyze(&sweep, &rec, IrWindow::default()).is_err());
    }

    #[test]
    fn clipping_is_rejected() {
        let sweep = Sweep::new(SweepParams::default());
        let rec: Vec<f32> = sweep
            .samples_f32()
            .iter()
            .map(|v| (v * 8.0).clamp(-1.0, 1.0))
            .collect();
        assert!(matches!(
            SweepAnalysis::analyze(&sweep, &rec, IrWindow::default()),
            Err(SweepIssue::Clipped { .. })
        ));
    }

    /// Recording of `sweep` through a unity system with Gaussian noise of `noise_rms` added.
    fn noisy_recording(sweep: &Sweep, noise_rms: f64, rumble: f64) -> Vec<f32> {
        let mut rec = vec![0f32; 24_000];
        rec.extend(sweep.samples_f32());
        rec.extend(vec![0f32; 72_000]);
        let mut rng = crate::dsp::Rng::new(7);
        for (i, v) in rec.iter_mut().enumerate() {
            let t = i as f64 / 48_000.0;
            *v += (rng.next_gaussian() * noise_rms + rumble * (TAU * 4.0 * t).sin()) as f32;
        }
        rec
    }

    #[test]
    fn infrasonic_rumble_is_not_noise() {
        // Rumble louder than the sweep itself, but below the measured band.
        let sweep = Sweep::new(SweepParams::default());
        let rec = noisy_recording(&sweep, 1e-5, 0.3);
        let a = SweepAnalysis::analyze(&sweep, &rec, IrWindow::default()).expect("usable");
        assert!(
            a.octave_snr_db.iter().all(|(_, snr)| *snr > 40.0),
            "{:?}",
            a.octave_snr_db
        );
    }

    #[test]
    fn broadband_noise_is_too_quiet() {
        let sweep = Sweep::new(SweepParams::default());
        let rec = noisy_recording(&sweep, 0.05, 0.0);
        assert!(matches!(
            SweepAnalysis::analyze(&sweep, &rec, IrWindow::default()),
            Err(SweepIssue::TooQuiet { .. })
        ));
    }
}
