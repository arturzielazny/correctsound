//! Signal processing: sweeps, noise, spectra, levels and filters.

pub mod biquad;
pub mod decay;
pub mod level;
pub mod noise;
pub mod periodic;
pub mod room;
pub mod spectrum;
pub mod sweep;

/// The only sample rate the UMIK-1 supports; used for every stream.
pub const SAMPLE_RATE: u32 = 48_000;

/// Converts a power ratio to decibels, flooring at -300 dB.
#[must_use]
pub fn power_to_db(power: f64) -> f64 {
    10.0 * power.max(1e-30).log10()
}

/// Converts decibels to a power ratio.
#[must_use]
pub fn db_to_power(db: f64) -> f64 {
    10f64.powf(db / 10.0)
}

/// Converts decibels to an amplitude ratio.
#[must_use]
pub fn db_to_amplitude(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Converts an amplitude ratio to decibels, flooring at -300 dB.
#[must_use]
pub fn amplitude_to_db(amplitude: f64) -> f64 {
    20.0 * amplitude.abs().max(1e-15).log10()
}

/// Smallest power of two that is at least `n`.
#[must_use]
pub fn next_pow2(n: usize) -> usize {
    n.max(1).next_power_of_two()
}

/// Deterministic xorshift generator, so tests and the simulator are reproducible.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Creates a generator; a zero seed is replaced by a fixed non-zero one.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// Next value uniformly distributed in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Next value from a standard normal distribution (Box-Muller).
    pub fn next_gaussian(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-300);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}
