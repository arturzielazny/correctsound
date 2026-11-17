//! Fits peaking filters that bring a measured response towards its target.

use serde::{Deserialize, Serialize};

use super::target::{DesignOptions, Target};
use super::{EqSet, Filter, response_db};
use crate::dsp::SAMPLE_RATE;
use crate::dsp::spectrum::{POINTS_PER_OCTAVE, Response};

/// Deepest cut a filter may make, dB.
pub const GAIN_MIN: f64 = -12.0;
/// Largest boost a filter may make, dB (above [`Q_SPLIT_HZ`]; see [`LOW_BOOST_MAX`]).
pub const GAIN_MAX: f64 = 6.0;
/// Largest boost below [`Q_SPLIT_HZ`], dB: there, dips are mostly room cancellations
/// that a boost only fills at one spot, at the cost of headroom.
pub const LOW_BOOST_MAX: f64 = 3.0;
/// Largest boost of all filters together, dB: [`LOW_BOOST_MAX`] below [`Q_SPLIT_HZ`],
/// [`GAIN_MAX`] above. Applied to the summed curve, so neither stacked filters nor the
/// skirt of a wide boost can get round it.
#[must_use]
pub fn boost_cap_db(freq: f64) -> f64 {
    if freq < Q_SPLIT_HZ {
        LOW_BOOST_MAX
    } else {
        GAIN_MAX
    }
}
/// Boosts are centred at least this many times above the correction range's low end,
/// so a small speaker is not pushed where it is running out of excursion.
pub const BOOST_LOW_FACTOR: f64 = 2.0;
/// Largest combined boost below the speaker's bass limit, dB: only the skirt of a wide
/// boost centred higher up, which adds no meaningful excursion.
pub const BELOW_LIMIT_BOOST_DB: f64 = 1.0;
/// Cost weight of combined boost beyond [`boost_cap_db`]; large, so it acts as a limit.
const OVER_BOOST_WEIGHT: f64 = 1000.0;
pub const Q_MIN: f64 = 0.5;
/// Q limit below [`Q_SPLIT_HZ`], where room modes are narrow.
pub const Q_MAX_LOW: f64 = 10.0;
/// Q limit at and above [`Q_SPLIT_HZ`].
pub const Q_MAX_HIGH: f64 = 3.0;
pub const Q_SPLIT_HZ: f64 = 300.0;
/// Q limit at and above [`TREBLE_HZ`]: features narrower than about half an octave up
/// there change with every centimetre of head movement.
pub const Q_MAX_TREBLE: f64 = 2.0;
pub const TREBLE_HZ: f64 = 5_000.0;
/// Boosts wider than this are allowed; narrower dips are cancellations EQ cannot fill.
pub const BOOST_Q_MAX: f64 = 2.0;
/// Filters smaller than this are dropped, dB.
pub const MIN_ABS_GAIN: f64 = 0.5;
/// Dips deeper than this below target are treated as this deep, dB.
pub const DIP_LIMIT: f64 = 6.0;
/// Peaks above target weigh this much more than dips below it.
pub const PEAK_WEIGHT: f64 = 2.0;
/// Weight of EQ effect outside the correction range (where the ideal is no change).
pub const OUTSIDE_WEIGHT: f64 = 0.25;
/// Greedy seeding stops when the remaining error is within this, dB.
pub const DONE_DB: f64 = 1.0;

/// Precomputed `cos(ω)` and `cos(2ω)` for fast biquad magnitude evaluation.
struct Grid {
    cos1: Vec<f64>,
    cos2: Vec<f64>,
}

impl Grid {
    fn new(freqs: &[f64]) -> Self {
        let rate = f64::from(SAMPLE_RATE);
        let w: Vec<f64> = freqs
            .iter()
            .map(|f| std::f64::consts::TAU * f / rate)
            .collect();
        Self {
            cos1: w.iter().map(|w| w.cos()).collect(),
            cos2: w.iter().map(|w| (2.0 * w).cos()).collect(),
        }
    }

    /// RBJ peaking magnitude in dB on every grid point.
    fn curve(&self, f: &Filter, out: &mut [f64]) {
        let rate = f64::from(SAMPLE_RATE);
        let a = 10f64.powf(f.gain / 40.0);
        let w0 = std::f64::consts::TAU * f.freq / rate;
        let alpha = w0.sin() / (2.0 * f.q);
        let c = w0.cos();
        let (b0, b1, b2) = (1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a);
        let (a0, a1, a2) = (1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a);
        let (b0, b1, b2, a1, a2) = (b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0);
        let (nb0, nb1, nb2) = (
            b0 * b0 + b1 * b1 + b2 * b2,
            2.0 * (b0 * b1 + b1 * b2),
            2.0 * b0 * b2,
        );
        let (na0, na1, na2) = (1.0 + a1 * a1 + a2 * a2, 2.0 * (a1 + a1 * a2), 2.0 * a2);
        for ((o, &c1), &c2) in out.iter_mut().zip(&self.cos1).zip(&self.cos2) {
            let num = nb0 + nb1 * c1 + nb2 * c2;
            let den = na0 + na1 * c1 + na2 * c2;
            *o = 10.0 * (num / den).max(1e-30).log10();
        }
    }
}

/// Clamps a filter into the allowed parameter space.
fn constrain(f: &mut Filter, target: &Target) {
    f.freq = f.freq.clamp(target.cut_floor_hz(), target.high_hz);
    f.gain = f.gain.clamp(GAIN_MIN, GAIN_MAX);
    let mut q_max = if f.freq < Q_SPLIT_HZ {
        Q_MAX_LOW
    } else if f.freq < TREBLE_HZ {
        Q_MAX_HIGH
    } else {
        Q_MAX_TREBLE
    };
    if f.gain > 0.0 {
        q_max = q_max.min(BOOST_Q_MAX);
        f.freq = f
            .freq
            .max(BOOST_LOW_FACTOR * target.low_hz)
            .min(target.high_hz);
        if f.freq < Q_SPLIT_HZ {
            f.gain = f.gain.min(LOW_BOOST_MAX);
        }
    }
    f.q = f.q.clamp(Q_MIN, q_max);
}

/// The quantity the fitter minimises, per grid point.
struct Problem<'a> {
    freqs: &'a [f64],
    grid: Grid,
    /// Measured minus target with dips clamped, dB; only meaningful in range.
    error: Vec<f64>,
    /// Measured minus target without clamping, used to judge how narrow a dip is.
    raw_error: Vec<f64>,
    in_range: Vec<bool>,
    /// [`boost_cap_db`] on every grid point.
    boost_cap: Vec<f64>,
    target: &'a Target,
}

impl Problem<'_> {
    /// The cost as an RMS over the points it covers, dB.
    fn rms(&self, eq: &[f64]) -> f64 {
        let n = self.in_range.iter().filter(|&&b| b).count().max(1);
        (self.cost(eq) / n as f64).sqrt()
    }

    fn cost(&self, eq: &[f64]) -> f64 {
        self.error
            .iter()
            .zip(eq)
            .zip(&self.in_range)
            .zip(&self.boost_cap)
            .map(|(((&e, &q), &inside), &cap)| {
                let over = (q - cap).max(0.0);
                let fit = if inside {
                    let r = e + q;
                    if r > 0.0 { PEAK_WEIGHT * r * r } else { r * r }
                } else {
                    OUTSIDE_WEIGHT * q * q
                };
                fit + OVER_BOOST_WEIGHT * over * over
            })
            .sum()
    }
}

/// Fits up to `max_filters` peaking filters to `measured` (fit-smoothed) against `target`.
#[must_use]
pub fn fit(measured: &Response, target: &Target, max_filters: usize) -> Vec<Filter> {
    fit_problem(&problem(measured, target), max_filters)
}

/// Largest filter count the automatic choice tries.
pub const AUTO_MAX_FILTERS: usize = 16;
/// With one mic position: the smallest count reaching this share of the improvement
/// [`AUTO_MAX_FILTERS`] filters would make.
const KNEE_SHARE: f64 = 0.9;
/// Smallest standard error assumed for the one-standard-error rule, dB.
const MIN_STANDARD_ERROR_DB: f64 = 0.05;

/// How an automatic filter count was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutoMethod {
    /// Leave-one-out over this many mic positions.
    CrossValidated { positions: usize },
    /// One position: [`KNEE_SHARE`] of the achievable improvement.
    Knee,
}

/// How the automatic filter count was chosen, for display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoChoice {
    /// Filters used.
    pub chosen: usize,
    /// The filter cap that won (`chosen` can be lower: near-duplicates merge).
    pub limit: usize,
    pub method: AutoMethod,
    /// By filter cap, the remaining correctable error (dB RMS as the fitter weighs it):
    /// on held-out positions when cross-validated, else on the measurement itself.
    pub error_by_count: Vec<(usize, f64)>,
}

/// Fits all counts 1..=[`AUTO_MAX_FILTERS`]; index 0 is no filters.
///
/// The filters are placed one at a time, and placing the first `n` does not depend on how
/// many come after, so one run of [`seeds`] gives every count its starting point; only the
/// final refinement differs, and those run in parallel. The result is exactly what fitting
/// each count on its own gives, at a sixth of the work.
fn fit_all(p: &Problem<'_>) -> Vec<Vec<Filter>> {
    let seeded = seeds(p, AUTO_MAX_FILTERS);
    let mut fits: Vec<Vec<Filter>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (1..=AUTO_MAX_FILTERS)
            .map(|n| {
                // Placing stops early when nothing is left worth correcting: every larger
                // count starts from the last placement.
                let start = seeded
                    .get(n - 1)
                    .or_else(|| seeded.last())
                    .cloned()
                    .unwrap_or_default();
                scope.spawn(move || finish(p, start))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("fit threads do not panic"))
            .collect()
    });
    fits.insert(0, Vec::new());
    fits
}

/// Power average of responses in dB on a shared grid.
fn power_mean(responses: &[&Response]) -> Response {
    let freqs = responses[0].freqs.clone();
    let db = (0..freqs.len())
        .map(|i| {
            let p = responses
                .iter()
                .map(|r| 10f64.powf(r.db[i] / 10.0))
                .sum::<f64>()
                / responses.len() as f64;
            10.0 * p.log10()
        })
        .collect();
    Response { freqs, db }
}

/// Fits with the number of filters the measurement supports.
///
/// With several mic positions, by leave-one-out cross-validation: for each position, fit
/// every count to the average of the others and measure the correctable error left on
/// the held-out position. Filters that correct the room help everywhere; filters that
/// fit one spot's details stop helping (or hurt) where they were not fitted. The count
/// chosen is the smallest whose held-out error is within one standard error of the best
/// (the usual one-standard-error rule, favouring the simpler correction).
///
/// With one position there is nothing to validate against; the count is the smallest
/// that reaches [`KNEE_SHARE`] of the improvement [`AUTO_MAX_FILTERS`] filters make.
#[must_use]
pub fn fit_auto(
    measured: &Response,
    target: &Target,
    positions: &[Response],
) -> (Vec<Filter>, AutoChoice) {
    let p = problem(measured, target);
    // The fit itself and every cross-validation fold are independent: all at once.
    let folds = if positions.len() >= 2 {
        positions.len()
    } else {
        0
    };
    let (mut fits, fold_errors): (Vec<Vec<Filter>>, Vec<Vec<f64>>) = std::thread::scope(|scope| {
        let fold_handles: Vec<_> = (0..folds)
            .map(|held| {
                scope.spawn(move || {
                    let others: Vec<&Response> = positions
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != held)
                        .map(|(_, r)| r)
                        .collect();
                    let training = power_mean(&others);
                    let fold_fits = fit_all(&problem(&training, target));
                    let test = problem(&positions[held], target);
                    fold_fits
                        .iter()
                        .map(|f| test.rms(&response_db(f, test.freqs)))
                        .collect::<Vec<f64>>()
                })
            })
            .collect();
        let fits = fit_all(&p);
        let errors = fold_handles
            .into_iter()
            .map(|h| h.join().expect("fit threads do not panic"))
            .collect();
        (fits, errors)
    });
    let training_error: Vec<f64> = fits
        .iter()
        .map(|f| p.rms(&response_db(f, p.freqs)))
        .collect();

    let (chosen, method, curve) = if positions.len() >= 2 {
        // errors[fold][count]
        let errors = fold_errors;
        let folds = errors.len() as f64;
        let mean: Vec<f64> = (0..=AUTO_MAX_FILTERS)
            .map(|k| errors.iter().map(|e| e[k]).sum::<f64>() / folds)
            .collect();
        let best = (0..mean.len())
            .min_by(|&a, &b| mean[a].total_cmp(&mean[b]))
            .unwrap_or(0);
        let spread = (errors
            .iter()
            .map(|e| (e[best] - mean[best]).powi(2))
            .sum::<f64>()
            / (folds - 1.0).max(1.0))
        .sqrt();
        let se = (spread / folds.sqrt()).max(MIN_STANDARD_ERROR_DB);
        let chosen = (0..=best)
            .find(|&k| mean[k] <= mean[best] + se)
            .unwrap_or(best);
        let method = AutoMethod::CrossValidated {
            positions: positions.len(),
        };
        (chosen, method, mean)
    } else {
        let (e0, emin) = (training_error[0], training_error[AUTO_MAX_FILTERS]);
        let goal = e0 - KNEE_SHARE * (e0 - emin);
        let chosen = training_error
            .iter()
            .position(|&e| e <= goal + 1e-9)
            .unwrap_or(AUTO_MAX_FILTERS);
        (chosen, AutoMethod::Knee, training_error)
    };
    let filters = std::mem::take(&mut fits[chosen]);
    let choice = AutoChoice {
        chosen: filters.len(),
        limit: chosen,
        method,
        error_by_count: curve.into_iter().enumerate().collect(),
    };
    (filters, choice)
}

/// The fitting problem for `measured` against `target`.
fn problem<'a>(measured: &'a Response, target: &'a Target) -> Problem<'a> {
    let freqs = &measured.freqs;
    Problem {
        freqs,
        grid: Grid::new(freqs),
        error: freqs
            .iter()
            .zip(&measured.db)
            .map(|(&f, &d)| {
                let e = d - target.db_at(f);
                // Below the bass limit only peaks above the roll-off count.
                if target.cut_only(f) {
                    e.max(0.0)
                } else {
                    e.max(-DIP_LIMIT)
                }
            })
            .collect(),
        raw_error: freqs
            .iter()
            .zip(&measured.db)
            .map(|(&f, &d)| {
                let e = d - target.db_at(f);
                if target.cut_only(f) { e.max(0.0) } else { e }
            })
            .collect(),
        in_range: freqs
            .iter()
            .map(|&f| target.in_range(f) || target.cut_only(f))
            .collect(),
        // Below the bass limit no boost beyond the faint skirt of one placed higher up.
        boost_cap: freqs
            .iter()
            .map(|&f| {
                if f < target.low_hz {
                    BELOW_LIMIT_BOOST_DB
                } else {
                    boost_cap_db(f)
                }
            })
            .collect(),
        target,
    }
}

fn fit_problem(problem: &Problem<'_>, max_filters: usize) -> Vec<Filter> {
    let start = seeds(problem, max_filters).pop().unwrap_or_default();
    finish(problem, start)
}

/// Refines placed filters fully, merges any that converged onto one spot, and rounds them
/// to what the EQ program stores.
fn finish(problem: &Problem<'_>, mut filters: Vec<Filter>) -> Vec<Filter> {
    let target = problem.target;
    refine(problem, &mut filters, 400);
    let before = filters.len();
    merge_duplicates(&mut filters, target);
    filters.retain(|f| f.gain.abs() >= MIN_ABS_GAIN);
    if filters.len() != before {
        refine(problem, &mut filters, 100);
        filters.retain(|f| f.gain.abs() >= MIN_ABS_GAIN);
    }
    let mut filters: Vec<Filter> = filters
        .iter()
        .map(|f| {
            let mut r = f.rounded();
            constrain(&mut r, target);
            r.rounded()
        })
        .collect();
    filters.sort_by(|a, b| a.freq.total_cmp(&b.freq));
    filters
}

/// Merges filters that converged onto the same spot (within 1/24 octave and 10% Q).
fn merge_duplicates(filters: &mut Vec<Filter>, target: &Target) {
    let mut i = 0;
    while i < filters.len() {
        let mut j = i + 1;
        while j < filters.len() {
            let (a, b) = (filters[i], filters[j]);
            if (a.freq / b.freq).log2().abs() < 1.0 / 24.0 && (a.q / b.q - 1.0).abs() < 0.1 {
                filters[i].gain = a.gain + b.gain;
                constrain(&mut filters[i], target);
                filters.remove(j);
            } else {
                j += 1;
            }
        }
        i += 1;
    }
}

/// Peaking-filter Q for a bandwidth of `octaves` between half-gain points.
fn q_for_bandwidth(octaves: f64) -> f64 {
    let b = 2f64.powf(octaves.max(1.0 / POINTS_PER_OCTAVE));
    b.sqrt() / (b - 1.0)
}

/// Places one filter on the largest remaining error region, skipping regions that
/// are too small or are narrow dips. Returns `None` when nothing is worth correcting.
fn seed_one(
    p: &Problem<'_>,
    residual: &[f64],
    raw: &[f64],
    skipped: &mut [bool],
) -> Option<Filter> {
    loop {
        let (lo, hi, centre) = largest_region(p, residual, skipped)?;
        let peak = residual[centre];
        // Width from the unclamped residual: clamping would make deep, narrow dips look wide.
        let raw_centre = (lo..=hi)
            .max_by(|&a, &b| raw[a].abs().total_cmp(&raw[b].abs()))
            .unwrap_or(centre);
        let half = raw[raw_centre].abs() / 2.0;
        let mut l = raw_centre;
        while l > lo && raw[l - 1].abs() >= half {
            l -= 1;
        }
        let mut r = raw_centre;
        while r < hi && raw[r + 1].abs() >= half {
            r += 1;
        }
        let q = q_for_bandwidth((r - l + 1) as f64 / POINTS_PER_OCTAVE);
        // Too small to matter, or a narrow dip (a cancellation EQ cannot fill).
        if peak.abs() < DONE_DB || (peak < 0.0 && q > BOOST_Q_MAX) {
            skipped[lo..=hi].iter_mut().for_each(|s| *s = true);
            continue;
        }
        let mut filter = Filter {
            freq: p.freqs[centre],
            gain: -peak,
            q,
        };
        constrain(&mut filter, p.target);
        return Some(filter);
    }
}

/// Greedy seeding interleaved with short refinements: the filters after placing each one,
/// up to `max_filters` (fewer when nothing more is worth correcting).
fn seeds(p: &Problem<'_>, max_filters: usize) -> Vec<Vec<Filter>> {
    let n = p.error.len();
    let mut skipped = vec![false; n];
    let mut filters: Vec<Filter> = Vec::new();
    let mut placed = Vec::new();
    while filters.len() < max_filters {
        let eq = super::response_db(&filters, p.freqs);
        let residual: Vec<f64> = p.error.iter().zip(&eq).map(|(e, q)| e + q).collect();
        let raw: Vec<f64> = p.raw_error.iter().zip(&eq).map(|(e, q)| e + q).collect();
        let Some(filter) = seed_one(p, &residual, &raw, &mut skipped) else {
            break;
        };
        filters.push(filter);
        refine(p, &mut filters, 40);
        placed.push(filters.clone());
    }
    placed
}

/// Finds the in-range same-sign run with the largest weighted area.
/// Returns (first index, last index, index of largest |error|).
fn largest_region(
    p: &Problem<'_>,
    residual: &[f64],
    skipped: &[bool],
) -> Option<(usize, usize, usize)> {
    let n = residual.len();
    let mut best: Option<(f64, usize, usize, usize)> = None;
    let mut i = 0;
    while i < n {
        if !p.in_range[i] {
            i += 1;
            continue;
        }
        let positive = residual[i] > 0.0;
        let start = i;
        let (mut area, mut centre) = (0.0, i);
        while i < n && p.in_range[i] && (residual[i] > 0.0) == positive {
            area += residual[i].abs();
            if residual[i].abs() > residual[centre].abs() {
                centre = i;
            }
            i += 1;
        }
        let end = i - 1;
        if skipped[centre] {
            continue;
        }
        let weighted = if positive { PEAK_WEIGHT * area } else { area };
        if best.is_none_or(|b| weighted > b.0) {
            best = Some((weighted, start, end, centre));
        }
    }
    best.map(|(_, s, e, c)| (s, e, c))
}

/// Joint coordinate-descent refinement of every filter's (log f, gain, log Q).
fn refine(p: &Problem<'_>, filters: &mut [Filter], max_rounds: usize) {
    let n = p.error.len();
    let mut curves: Vec<Vec<f64>> = filters
        .iter()
        .map(|f| {
            let mut c = vec![0.0; n];
            p.grid.curve(f, &mut c);
            c
        })
        .collect();
    let mut total = vec![0.0; n];
    for c in &curves {
        for (t, v) in total.iter_mut().zip(c) {
            *t += v;
        }
    }
    let mut cost = p.cost(&total);
    // Steps: octaves for frequency, dB for gain, octaves (log2) for Q.
    let mut steps: Vec<[f64; 3]> = vec![[1.0 / 6.0, 1.0, 0.25]; filters.len()];
    let min_steps = [1.0 / 384.0, 0.02, 1.0 / 128.0];
    let mut trial_curve = vec![0.0; n];
    let mut trial_total = vec![0.0; n];
    for _ in 0..max_rounds {
        let mut any_active = false;
        for k in 0..filters.len() {
            for param in 0..3 {
                if steps[k][param] < min_steps[param] {
                    continue;
                }
                any_active = true;
                let mut improved = false;
                for dir in [1.0, -1.0] {
                    let mut f = filters[k];
                    let s = steps[k][param] * dir;
                    match param {
                        0 => f.freq *= 2f64.powf(s),
                        1 => f.gain += s,
                        _ => f.q *= 2f64.powf(s),
                    }
                    constrain(&mut f, p.target);
                    if f == filters[k] {
                        continue;
                    }
                    p.grid.curve(&f, &mut trial_curve);
                    for i in 0..n {
                        trial_total[i] = total[i] - curves[k][i] + trial_curve[i];
                    }
                    let c = p.cost(&trial_total);
                    if c < cost {
                        cost = c;
                        filters[k] = f;
                        std::mem::swap(&mut curves[k], &mut trial_curve);
                        std::mem::swap(&mut total, &mut trial_total);
                        improved = true;
                        break;
                    }
                }
                if improved {
                    steps[k][param] *= 1.2;
                } else {
                    steps[k][param] *= 0.5;
                }
            }
        }
        if !any_active {
            break;
        }
    }
}

/// One channel's design result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelDesign {
    pub target: Target,
    pub filters: Vec<Filter>,
    /// How the filter count was chosen, when automatic.
    pub auto: Option<AutoChoice>,
    /// The measured (fit-smoothed) response.
    pub measured: Response,
    /// Measured plus EQ plus trim (preamp excluded).
    pub predicted: Response,
    pub rms_before_db: f64,
    pub rms_after_db: f64,
}

/// The stereo design.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Design {
    pub channels: [ChannelDesign; 2],
    pub eq: EqSet,
    pub options: DesignOptions,
    /// Left minus right reference level before trimming, dB.
    pub imbalance_db: f64,
}

impl Design {
    /// Common target level after trim, used for display.
    #[must_use]
    pub fn display_target(&self, channel: usize) -> Target {
        let mut t = self.channels[channel].target.clone();
        let trim = self.eq.trim_db[channel];
        t.reference_db += trim;
        for p in &mut t.roll_off {
            p.1 += trim;
        }
        t
    }
}

/// Designs the stereo correction from fit-smoothed responses.
#[must_use]
pub fn design(fit_resp: [&Response; 2], options: &DesignOptions) -> Design {
    design_with_positions(fit_resp, [&[], &[]], options)
}

/// [`design`], with each mic position's own fit-smoothed response per channel, which the
/// automatic filter count cross-validates against.
#[must_use]
pub fn design_with_positions(
    fit_resp: [&Response; 2],
    positions: [&[Response]; 2],
    options: &DesignOptions,
) -> Design {
    let targets: [Target; 2] = [0, 1].map(|c| Target::for_response(fit_resp[c], options));
    let fitted: [(Vec<Filter>, Option<AutoChoice>); 2] = std::thread::scope(|scope| {
        let handles = [0, 1].map(|c| {
            let target = &targets[c];
            scope.spawn(move || match options.max_filters {
                Some(n) => (fit(fit_resp[c], target, n), None),
                None => {
                    let (filters, choice) = fit_auto(fit_resp[c], target, positions[c]);
                    (filters, Some(choice))
                }
            })
        });
        handles.map(|h| h.join().expect("fit threads do not panic"))
    });
    let [(filters_l, auto_l), (filters_r, auto_r)] = fitted;
    let autos = [auto_l, auto_r];
    let filters: [Vec<Filter>; 2] = [filters_l, filters_r];
    let imbalance_db = targets[0].reference_db - targets[1].reference_db;
    let trim_db = if imbalance_db > 0.0 {
        [-imbalance_db, 0.0]
    } else {
        [0.0, imbalance_db]
    };
    let trim_db = trim_db.map(|t| (t * 10.0).round() / 10.0);
    let freqs = fit_resp[0].freqs.clone();
    let eq_curves: [Vec<f64>; 2] = [0, 1].map(|c| response_db(&filters[c], &freqs));
    let max_gain = (0..2)
        .flat_map(|c| eq_curves[c].iter().map(move |v| v + trim_db[c]))
        .fold(f64::NEG_INFINITY, f64::max);
    let preamp_db = if max_gain > 0.0 {
        -((max_gain + 0.5) * 10.0).ceil() / 10.0
    } else {
        0.0
    };
    let channels = [0, 1].map(|c| {
        let target = targets[c].clone();
        let measured = fit_resp[c].clone();
        let predicted = Response {
            freqs: freqs.clone(),
            db: measured
                .db
                .iter()
                .zip(&eq_curves[c])
                .map(|(m, e)| m + e + trim_db[c])
                .collect(),
        };
        let rms = |r: &Response, offset: f64| {
            let (sum, n) = r
                .freqs
                .iter()
                .zip(&r.db)
                .filter(|(f, _)| target.in_range(**f))
                .fold((0.0, 0usize), |(s, n), (&f, &d)| {
                    (s + (d - target.db_at(f) - offset).powi(2), n + 1)
                });
            if n == 0 { 0.0 } else { (sum / n as f64).sqrt() }
        };
        ChannelDesign {
            rms_before_db: rms(&measured, 0.0),
            rms_after_db: rms(&predicted, trim_db[c]),
            target,
            filters: filters[c].clone(),
            auto: autos[c].clone(),
            measured,
            predicted,
        }
    });
    Design {
        channels,
        eq: EqSet {
            preamp_db,
            trim_db,
            channels: filters,
        },
        options: *options,
        imbalance_db,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::spectrum::grid;

    /// A speaker in a room: rolled off below 45 Hz, a few peaks and dips.
    fn measured() -> Response {
        let freqs: Vec<f64> = grid().into_iter().filter(|&f| f <= 20_000.0).collect();
        let bump =
            |f: f64, at: f64, db: f64, width: f64| db * (-((f / at).log2() / width).powi(2)).exp();
        let db = freqs
            .iter()
            .map(|&f| {
                80.0 - 24.0 * (45.0 / f).max(1.0).log2()
                    + bump(f, 33.0, 9.0, 0.08)
                    + bump(f, 62.0, -7.0, 0.1)
                    + bump(f, 120.0, 6.0, 0.15)
                    + bump(f, 900.0, -3.0, 0.4)
                    + bump(f, 3000.0, 2.5, 0.5)
            })
            .collect();
        Response { freqs, db }
    }

    #[test]
    fn placing_the_filters_once_fits_every_count_as_fitting_it_alone_would() {
        let m = measured();
        let target = Target::for_response(&m, &DesignOptions::default());
        let p = problem(&m, &target);
        let all = fit_all(&p);
        assert!(all[0].is_empty());
        for (n, fitted) in all.iter().enumerate().skip(1) {
            assert_eq!(*fitted, fit_problem(&p, n), "{n} filters");
        }
        assert!(!all[AUTO_MAX_FILTERS].is_empty());
    }
}
