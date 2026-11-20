//! Curiosities about the room and the speakers, read from the measurement: resonances and
//! room dimensions they suggest, reverberation, reflections, cancellations, tonal balance.
//! None of this changes the EQ; it is shown on the curiosities screen and in report.txt.

use crate::analysis::Responses;
use crate::dsp::room::SPEED_OF_SOUND;
use crate::dsp::spectrum::{Response, octave_average_db};
use crate::dsp::sweep::SweepAnalysis;
use crate::eq::fit::Design;

/// Resonances and cancellations are looked for up to here, Hz (above it the room is
/// diffuse and narrow features belong to the position, not the room).
const MODAL_LIMIT_HZ: f64 = 300.0;
/// A peak this far above the octave-averaged response counts as a resonance, dB.
const RESONANCE_DB: f64 = 4.0;
/// A dip this far below it counts as a cancellation, dB.
const CANCELLATION_DB: f64 = 8.0;
/// Frequencies within this ratio are the same feature (left vs right, mode vs peak).
const SAME_FEATURE: f64 = 0.03;

/// A titled group of findings.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub lines: Vec<String>,
}

/// What the curiosities screen needs.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub responses: [&'a Responses; 2],
    pub analyses: [&'a [SweepAnalysis]; 2],
    pub design: &'a Design,
    /// Background noise in the level step's band (500–2000 Hz), dB SPL.
    pub ambient_spl: Option<f64>,
    /// The level measured at, the level the room called for, and the background in the
    /// 63 Hz octave, all dB SPL.
    pub level_spl: Option<f64>,
    pub needed_spl: Option<f64>,
    pub bass_noise_spl: Option<f64>,
}

/// A local maximum or minimum relative to the octave-averaged response.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Feature {
    hz: f64,
    /// Height above (positive) or depth below (negative) the local average, dB.
    db: f64,
}

/// Peaks (`sign` 1) or dips (`sign` −1) at least `threshold` dB from the local average,
/// between `from` and [`MODAL_LIMIT_HZ`], at most one per sixth of an octave.
fn features(r: &Response, sign: f64, threshold: f64, from: f64) -> Vec<Feature> {
    let trend = octave_average_db(r, 1.0);
    let dev: Vec<f64> =
        r.db.iter()
            .zip(&trend)
            .map(|(d, t)| sign * (d - t))
            .collect();
    let mut found: Vec<Feature> = Vec::new();
    for i in 1..dev.len().saturating_sub(1) {
        let hz = r.freqs[i];
        if hz < from || hz > MODAL_LIMIT_HZ {
            continue;
        }
        if dev[i] >= threshold && dev[i] >= dev[i - 1] && dev[i] >= dev[i + 1] {
            let f = Feature {
                hz,
                db: sign * dev[i],
            };
            match found.last_mut() {
                Some(last) if (hz / last.hz).log2() < 1.0 / 6.0 => {
                    if f.db.abs() > last.db.abs() {
                        *last = f;
                    }
                }
                _ => found.push(f),
            }
        }
    }
    found
}

/// Features of both channels; one seen in both within [`SAME_FEATURE`] is listed once.
fn merged(left: &[Feature], right: &[Feature]) -> Vec<(Feature, &'static str)> {
    let mut out: Vec<(Feature, &'static str)> = Vec::new();
    for l in left {
        let both = right
            .iter()
            .any(|r| (r.hz / l.hz - 1.0).abs() < SAME_FEATURE);
        out.push((*l, if both { "both" } else { "left" }));
    }
    for r in right {
        if !left
            .iter()
            .any(|l| (r.hz / l.hz - 1.0).abs() < SAME_FEATURE)
        {
            out.push((*r, "right"));
        }
    }
    out.sort_by(|a, b| a.0.hz.total_cmp(&b.0.hz));
    out
}

/// A room dimension whose standing-wave series (343 / 2L × n) lines up with resonances.
#[derive(Debug, Clone, PartialEq)]
struct Dimension {
    metres: f64,
    explains: Vec<f64>,
}

/// Up to three dimensions between 1.8 and 8 m, each explaining at least two resonances
/// not explained by an earlier one, weighted by how strong the resonances are.
fn dimensions(resonances: &[Feature]) -> Vec<Dimension> {
    let mut left: Vec<Feature> = resonances.to_vec();
    let mut chosen: Vec<Dimension> = Vec::new();
    while chosen.len() < 3 {
        let mut best: Option<(f64, Dimension)> = None;
        for cm in 180..=800 {
            let metres = f64::from(cm) / 100.0;
            let f1 = SPEED_OF_SOUND / (2.0 * metres);
            // A new dimension must not be a harmonic of one already found.
            let related = chosen.iter().any(|d| {
                let ratio = (SPEED_OF_SOUND / (2.0 * d.metres)) / f1;
                let r = if ratio >= 1.0 { ratio } else { 1.0 / ratio };
                (r - r.round()).abs() < 0.05
            });
            if related {
                continue;
            }
            // Each resonance near one of the first six standing waves, with how far off.
            let predicted = (1..=6)
                .filter(|n| f1 * f64::from(*n) <= MODAL_LIMIT_HZ)
                .count();
            let mut fundamental = false;
            let mut hits: Vec<(&Feature, f64)> = Vec::new();
            for p in &left {
                let best_n = (1..=6)
                    .map(|n| (n, (p.hz / (f1 * f64::from(n)) - 1.0).abs()))
                    .filter(|(n, _)| f1 * f64::from(*n) <= MODAL_LIMIT_HZ)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((n, err)) = best_n
                    && err <= 0.025
                {
                    fundamental |= n == 1;
                    hits.push((p, err));
                }
            }
            // The first standing wave must show, and at least one more.
            if !fundamental || hits.len() < 2 {
                continue;
            }
            // Closer matches count more; a dimension with many predicted waves (a long one)
            // only scores for the share of them it actually finds.
            let matched: f64 = hits.iter().map(|(p, err)| p.db * (1.0 - err / 0.05)).sum();
            let score = matched * hits.len() as f64 / predicted.max(1) as f64;
            let hits: Vec<&Feature> = hits.into_iter().map(|(p, _)| p).collect();
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((
                    score,
                    Dimension {
                        metres,
                        explains: hits.iter().map(|p| p.hz).collect(),
                    },
                ));
            }
        }
        let Some((_, d)) = best else { break };
        left.retain(|p| !d.explains.contains(&p.hz));
        chosen.push(d);
    }
    chosen
}

/// Least-squares slope of the response in dB per octave between `from` and `to` Hz.
fn slope_db_per_octave(r: &Response, from: f64, to: f64) -> f64 {
    let pts: Vec<(f64, f64)> = r
        .freqs
        .iter()
        .zip(&r.db)
        .filter(|(f, _)| (from..=to).contains(*f))
        .map(|(f, d)| (f.log2(), *d))
        .collect();
    let n = pts.len().max(1) as f64;
    let (mx, my) = pts
        .iter()
        .fold((0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n));
    let (cov, var) = pts.iter().fold((0.0, 0.0), |a, p| {
        (a.0 + (p.0 - mx) * (p.1 - my), a.1 + (p.0 - mx).powi(2))
    });
    if var > 0.0 { cov / var } else { 0.0 }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

fn hz(f: f64) -> String {
    if f >= 1000.0 {
        format!("{:.1} kHz", f / 1000.0)
    } else {
        format!("{f:.0} Hz")
    }
}

/// The bass notes that ring longest after the sound stops, from both channels' waterfalls.
fn ringing_line(analyses: &[&[SweepAnalysis]; 2]) -> Option<String> {
    use crate::dsp::decay::{FADE_DB, Waterfall};
    let mut found: Vec<(f64, f64, &str)> = Vec::new();
    let mut typical = Vec::new();
    for (name, runs) in ["left", "right"].into_iter().zip(analyses) {
        let all: Vec<&Waterfall> = runs.iter().map(|a| &a.waterfall).collect();
        let Some(w) = Waterfall::average(&all) else {
            continue;
        };
        typical.extend(w.median_fade_ms(30.0, 300.0));
        for r in w.ringing(30.0).into_iter().take(3) {
            match found
                .iter_mut()
                .find(|f| (f.0 / r.hz).log2().abs() < 1.0 / 6.0)
            {
                Some(f) => {
                    f.1 = f.1.max(r.fade_ms);
                    f.2 = "both";
                }
                None => found.push((r.hz, r.fade_ms, name)),
            }
        }
    }
    let typical = median(typical)?;
    if found.is_empty() {
        return Some(format!(
            "No bass note rings much longer than the rest (they fade {FADE_DB:.0} dB in about \
             {typical:.0} ms): an evenly damped room."
        ));
    }
    found.sort_by(|a, b| b.1.total_cmp(&a.1));
    let modes = all_modes(analyses);
    let list: Vec<String> = found
        .iter()
        .take(4)
        .map(|(f, t, ch)| {
            if *t >= crate::dsp::decay::SLICE_STEP_MS * (crate::dsp::decay::SLICES - 1) as f64 {
                // Past the waterfall's 400 ms: the mode measured on its own says how long.
                match crate::dsp::modes::near(&modes, *f) {
                    Some(m) => format!("{} {} ({ch})", hz(*f), m.fade_20_text()),
                    None => format!("{} over {t:.0} ms ({ch})", hz(*f)),
                }
            } else {
                format!("{} {t:.0} ms ({ch})", hz(*f))
            }
        })
        .collect();
    Some(format!(
        "Bass notes that keep ringing after the music stops (time to fade {FADE_DB:.0} dB; \
         typical here {typical:.0} ms): {}. Press w for the decay view.",
        list.join(", ")
    ))
}

/// The room modes heard from both speakers, merged over every measurement.
fn all_modes(analyses: &[&[SweepAnalysis]; 2]) -> Vec<crate::dsp::modes::Mode> {
    let all: Vec<&[crate::dsp::modes::Mode]> = analyses
        .iter()
        .flat_map(|runs| runs.iter().map(|a| a.modes.as_slice()))
        .collect();
    crate::dsp::modes::merge(&all)
}

/// The room modes and how long each rings, against the room's reverberation elsewhere.
fn modes_line(analyses: &[&[SweepAnalysis]; 2]) -> Option<String> {
    let modes = all_modes(analyses);
    if modes.is_empty() {
        return None;
    }
    let reverb = median(
        analyses
            .iter()
            .flat_map(|runs| runs.iter())
            .flat_map(|a| a.decay.iter().map(|d| d.1))
            .collect(),
    );
    let list: Vec<String> = modes
        .iter()
        .map(|m| format!("{:.1} Hz {}", m.hz, m.t60_text()))
        .collect();
    let elsewhere = reverb.map_or(String::new(), |r| format!("; the room elsewhere {r:.1} s"));
    let boomy = reverb.is_some_and(|r| modes.iter().any(|m| m.t60_s > 2.0 * r));
    Some(format!(
        "Room modes and how long each rings (time to fade 60 dB{elsewhere}): {}.{}",
        list.join(", "),
        if boomy {
            " Those ringing more than twice as long as the room are what sounds boomy; a bass \
             trap that works shortens them, which fast sweeps in placement show."
        } else {
            ""
        }
    ))
}

/// Everything the curiosities screen shows.
#[must_use]
pub fn insights(inp: &Inputs<'_>) -> Vec<Section> {
    let [l, r] = inp.responses;
    let floor = inp
        .design
        .channels
        .iter()
        .map(|c| c.target.cut_floor_hz())
        .fold(f64::INFINITY, f64::min)
        .min(30.0);
    let resonances = merged(
        &features(&l.fit, 1.0, RESONANCE_DB, floor),
        &features(&r.fit, 1.0, RESONANCE_DB, floor),
    );
    let cancellations = merged(
        &features(&l.fit, -1.0, CANCELLATION_DB, floor),
        &features(&r.fit, -1.0, CANCELLATION_DB, floor),
    );
    let mut sections = Vec::new();

    // ----- room -----
    let mut room = Vec::new();
    if resonances.is_empty() {
        room.push("No marked bass resonances: an evenly behaved room at this seat.".to_owned());
    } else {
        let list: Vec<String> = resonances
            .iter()
            .map(|(f, ch)| format!("{} (+{:.0} dB, {ch})", hz(f.hz), f.db))
            .collect();
        room.push(format!(
            "Bass resonances (room modes): {}.",
            list.join(", ")
        ));
    }
    // Dimensions from resonances seen in both channels when there are enough, else all.
    let shared: Vec<Feature> = resonances
        .iter()
        .filter(|(_, ch)| *ch == "both")
        .map(|(f, _)| *f)
        .collect();
    let pool: Vec<Feature> = if shared.len() >= 3 {
        shared
    } else {
        resonances.iter().map(|(f, _)| *f).collect()
    };
    let dims = dimensions(&pool);
    if !dims.is_empty() {
        let list: Vec<String> = dims
            .iter()
            .map(|d| {
                let modes: Vec<String> = d.explains.iter().map(|f| format!("{f:.0}")).collect();
                format!("{:.1} m (explains {} Hz)", d.metres, modes.join(", "))
            })
            .collect();
        room.push(format!(
            "Room dimensions that would produce these resonances: {}. A guess from standing waves \
             between parallel walls; seat and speaker positions decide which ones show.",
            list.join("; ")
        ));
    }
    // The strongest resonance no dimension above explains, on its own.
    if let Some((f, _)) = resonances
        .iter()
        .filter(|(f, _)| f.hz <= 150.0 && !dims.iter().any(|d| d.explains.contains(&f.hz)))
        .max_by(|a, b| a.0.db.total_cmp(&b.0.db))
    {
        // Two dimensions' first standing waves together (around four walls) ring at
        // √(f₁² + f₂²): a resonance that fits that is not a third pair of walls.
        let combined = dims.iter().enumerate().find_map(|(i, a)| {
            dims[i + 1..].iter().find_map(|b| {
                let (fa, fb) = (
                    SPEED_OF_SOUND / (2.0 * a.metres),
                    SPEED_OF_SOUND / (2.0 * b.metres),
                );
                ((fa.hypot(fb) / f.hz - 1.0).abs() < 0.025).then_some((a.metres, b.metres))
            })
        });
        room.push(match combined {
            Some((a, b)) => format!(
                "The strongest resonance not explained by those, {} (+{:.0} dB), is where the \
                 {a:.1} m and {b:.1} m dimensions' standing waves meet: one running around four \
                 walls, strongest in the corners.",
                hz(f.hz),
                f.db
            ),
            None => format!(
                "The strongest resonance{}, {} (+{:.0} dB), would be the first standing wave of \
                 a {:.1} m dimension (or the second of {:.1} m), or two dimensions' waves \
                 together.",
                if dims.is_empty() {
                    ""
                } else {
                    " not explained by those"
                },
                hz(f.hz),
                f.db,
                SPEED_OF_SOUND / (2.0 * f.hz),
                SPEED_OF_SOUND / f.hz
            ),
        });
    }
    room.push(
        "Press r for the room seen from above: which standing wave each resonance is, where \
         bass traps work, and whether another seat would sound more even."
            .to_owned(),
    );
    let all: Vec<&SweepAnalysis> = inp.analyses.iter().flat_map(|a| a.iter()).collect();
    let decay: Vec<(f64, f64)> = crate::dsp::room::DECAY_BANDS_HZ
        .iter()
        .filter_map(|&band| {
            let times: Vec<f64> = all
                .iter()
                .filter_map(|a| a.decay.iter().find(|d| d.0 == band).map(|d| d.1))
                .collect();
            median(times).map(|t| (band, t))
        })
        .collect();
    if !decay.is_empty() {
        let list: Vec<String> = decay
            .iter()
            .map(|(f, t)| format!("{} {t:.2} s", hz(*f)))
            .collect();
        let mid = median(
            decay
                .iter()
                .filter(|(f, _)| (500.0..=1000.0).contains(f))
                .map(|d| d.1)
                .collect(),
        );
        let verdict = match mid {
            Some(t) if t < 0.3 => "dry, well damped (a studio or a very furnished room)",
            Some(t) if t < 0.7 => "typical of a living room or office (0.3–0.7 s)",
            Some(_) => "lively: hard surfaces; rugs, curtains or panels would calm it",
            None => "",
        };
        room.push(format!(
            "Reverberation time (time to fade by 60 dB): {}. {}{}.",
            list.join(", "),
            if verdict.is_empty() { "" } else { "Midrange: " },
            verdict
        ));
        if dims.len() == 3
            && let Some(t) = mid
        {
            let volume: f64 = dims.iter().map(|d| d.metres).product();
            room.push(format!(
                "If those are the room's three dimensions: {volume:.0} m³; below about {:.0} Hz \
                 (the Schroeder frequency) it behaves as separate resonances, above it as diffuse \
                 reverberation.",
                2000.0 * (t / volume).sqrt()
            ));
        }
    }
    if let Some(line) = modes_line(&inp.analyses) {
        room.push(line);
    }
    if let Some(line) = ringing_line(&inp.analyses) {
        room.push(line);
    }
    for (name, analyses) in ["Left", "Right"].iter().zip(inp.analyses) {
        if let Some(refl) = analyses.first().and_then(|a| a.reflection) {
            let path = refl.extra_path_m();
            let what = if path < 1.0 {
                "probably the desk or something next to the speaker"
            } else {
                "probably a wall, the ceiling or the floor"
            };
            room.push(format!(
                "{name} speaker's strongest early reflection: {:.1} ms after the direct sound, \
                 {:.0} dB, a path {:.0} cm longer: {what}.",
                refl.delay_ms,
                refl.level_db,
                path * 100.0
            ));
        }
    }
    if let Some(a) = inp.ambient_spl {
        room.push(format!(
            "Background noise: {a:.0} dB SPL in the level meter's band, 250 Hz–4 kHz ({}).",
            if a < 30.0 {
                "very quiet"
            } else if a < 40.0 {
                "quiet"
            } else {
                "noticeable; it limits how quietly you can listen"
            }
        ));
    }
    if let Some(b) = inp.bass_noise_spl {
        room.push(format!(
            "Background in the bass (63 Hz octave): {b:.0} dB SPL ({}). Traffic, ventilation \
             and fridges are loudest there, so it usually decides how loud the measurement \
             must be.",
            if b < 30.0 {
                "quiet"
            } else if b < 40.0 {
                "some rumble"
            } else {
                "a lot of rumble"
            }
        ));
    }
    if let Some(l) = inp.level_spl {
        let needed = inp.needed_spl.unwrap_or(l);
        room.push(if l > needed + crate::app::level::TOLERANCE {
            format!(
                "Measured at {l:.0} dB SPL on the level meter; {needed:.0} dB would have kept \
                 every octave about 30 dB above this room's background, so the extra was \
                 margin, not a need."
            )
        } else {
            format!(
                "Measured at {l:.0} dB SPL on the level meter, the quietest level that keeps \
                 every octave about 30 dB above this room's background."
            )
        });
    }
    sections.push(Section {
        title: "Room",
        lines: room,
    });

    // ----- speakers -----
    let mut speakers = Vec::new();
    let [dl, dr] = &inp.design.channels;
    speakers.push(format!(
        "Bass reaches down to about {} (left) and {} (right) before falling 6 dB.",
        hz(dl.target.low_hz),
        hz(dr.target.low_hz)
    ));
    let top = |h: f64| {
        if h >= 19_000.0 {
            "the top of hearing".to_owned()
        } else {
            hz(h)
        }
    };
    speakers.push(format!(
        "Treble extends to {} (left) and {} (right).",
        top(dl.target.high_hz),
        top(dr.target.high_hz)
    ));
    for (name, resp) in [("left", &l.fit), ("right", &r.fit)] {
        let s = slope_db_per_octave(resp, 100.0, 10_000.0);
        let feel = if s > 0.3 {
            "bright"
        } else if s > -0.3 {
            "neutral, flat"
        } else if s > -1.0 {
            "slightly warm, the usual in-room balance"
        } else {
            "warm, dark"
        };
        speakers.push(format!(
            "Tonal balance as measured ({name}): {s:+.1} dB per octave from 100 Hz to 10 kHz, {feel}."
        ));
    }
    let lr: Vec<(f64, f64)> = l
        .fit
        .freqs
        .iter()
        .zip(l.fit.db.iter().zip(&r.fit.db))
        .filter(|(f, _)| (100.0..=10_000.0).contains(*f))
        .map(|(f, (a, b))| (*f, a - b))
        .collect();
    if !lr.is_empty() {
        let mean = lr.iter().map(|p| p.1).sum::<f64>() / lr.len() as f64;
        let rms = (lr.iter().map(|p| (p.1 - mean).powi(2)).sum::<f64>() / lr.len() as f64).sqrt();
        let worst = lr
            .iter()
            .max_by(|a, b| (a.1 - mean).abs().total_cmp(&(b.1 - mean).abs()))
            .copied()
            .unwrap_or((0.0, 0.0));
        speakers.push(format!(
            "Left vs right: {:+.1} dB in level; shapes differ by {rms:.1} dB RMS, most at {} \
             ({:+.0} dB). {}",
            inp.design.imbalance_db,
            hz(worst.0),
            worst.1 - mean,
            if rms < 1.5 {
                "A well matched pair in a symmetric spot."
            } else {
                "Some asymmetry: one speaker is nearer a wall or corner, or the desk differs."
            }
        ));
    }
    sections.push(Section {
        title: "Speakers in the room",
        lines: speakers,
    });

    // ----- unusual -----
    let mut unusual = Vec::new();
    let strong: Vec<&(Feature, &str)> = resonances.iter().filter(|(f, _)| f.db >= 8.0).collect();
    if !strong.is_empty() {
        // Whether a cut sits on it, in the channel(s) it shows in.
        let cut = |f: &Feature, ch: &str| {
            let channels: &[usize] = match ch {
                "left" => &[0],
                "right" => &[1],
                _ => &[0, 1],
            };
            channels.iter().all(|&c| {
                inp.design.channels[c]
                    .filters
                    .iter()
                    .any(|g| g.gain < 0.0 && (g.freq / f.hz).log2().abs() < 1.0 / 6.0)
            })
        };
        let list: Vec<String> = strong
            .iter()
            .map(|(f, ch)| {
                format!(
                    "{} (+{:.0} dB, {ch}, {})",
                    hz(f.hz),
                    f.db,
                    if cut(f, ch) {
                        "cut by the EQ"
                    } else {
                        "not cut"
                    }
                )
            })
            .collect();
        unusual.push(format!(
            "Booming notes (strong resonances): {}.",
            list.join(", ")
        ));
    }
    if !cancellations.is_empty() {
        let list: Vec<String> = cancellations
            .iter()
            .map(|(f, ch)| format!("{} ({:.0} dB, {ch})", hz(f.hz), f.db))
            .collect();
        unusual.push(format!(
            "Cancellations: {}. A reflection arrives out of phase and cancels the sound; EQ \
             cannot fill these (more power cancels too), but moving the seat or the speakers by \
             20–50 cm moves them.",
            list.join(", ")
        ));
    }
    // Broad dips: the octave-averaged response well below its two-octave surroundings.
    for (name, resp) in [("left", &l.fit), ("right", &r.fit)] {
        let narrow = octave_average_db(resp, 1.0 / 3.0);
        let wide = octave_average_db(resp, 2.0);
        let mut i = 0;
        while i < narrow.len() {
            let f = resp.freqs[i];
            if (60.0..=8000.0).contains(&f) && wide[i] - narrow[i] > 5.0 {
                let start = i;
                while i < narrow.len() && wide[i] - narrow[i] > 5.0 {
                    i += 1;
                }
                let centre = (resp.freqs[start] * resp.freqs[i - 1]).sqrt();
                let depth = (start..i).map(|k| wide[k] - narrow[k]).fold(0.0, f64::max);
                let quarter = SPEED_OF_SOUND / (4.0 * centre);
                unusual.push(format!(
                    "Broad dip around {} ({name}, −{depth:.0} dB): typical of a reflection from a \
                     surface about {:.0} cm from the speaker (wall behind it, desk) cancelling the \
                     direct sound at a quarter wavelength.",
                    hz(centre),
                    quarter * 100.0
                ));
            }
            i += 1;
        }
    }
    if unusual.is_empty() {
        unusual.push("Nothing unusual: no strong resonances, cancellations or broad dips.".into());
    }
    sections.push(Section {
        title: "Unusual",
        lines: unusual,
    });
    sections
}

/// The sections as plain text, for report.txt.
#[must_use]
pub fn text(sections: &[Section]) -> String {
    let mut s = String::from("Curiosities\n");
    for sec in sections {
        s.push_str(&format!("\n{}\n", sec.title));
        for l in &sec.lines {
            s.push_str(&format!("- {l}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standing_waves_point_to_their_dimension() {
        // Axial modes of a 4.2 m and a 3.1 m dimension, plus an unrelated peak.
        let modes = |m: f64| (1..=3).map(move |n| SPEED_OF_SOUND / (2.0 * m) * f64::from(n));
        let peaks: Vec<Feature> = modes(4.2)
            .chain(modes(3.1))
            .chain([137.0])
            .map(|hz| Feature { hz, db: 6.0 })
            .collect();
        let dims = dimensions(&peaks);
        let found: Vec<f64> = dims.iter().map(|d| d.metres).collect();
        assert!(found.iter().any(|m| (m - 4.2).abs() < 0.06), "{found:?}");
        assert!(found.iter().any(|m| (m - 3.1).abs() < 0.06), "{found:?}");
    }
}
