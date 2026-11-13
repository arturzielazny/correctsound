//! Writes Equalizer APO text, EasyEffects presets and a plain-text report.

use std::fmt::Write as _;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::fit::Design;
use super::{EqSet, Filter};

/// Equalizer APO text for one channel; `Preamp` includes the channel trim.
#[must_use]
pub fn apo_text(eq: &EqSet, channel: usize) -> String {
    let mut s = String::new();
    let preamp = eq.preamp_db + eq.trim_db[channel];
    let _ = writeln!(s, "Preamp: {preamp:.1} dB");
    for (i, f) in eq.channels[channel].iter().enumerate() {
        let _ = writeln!(
            s,
            "Filter {}: ON PK Fc {:.1} Hz Gain {:.1} dB Q {:.2}",
            i + 1,
            f.freq,
            f.gain,
            f.q
        );
    }
    s
}

/// Parses the subset of Equalizer APO text that [`apo_text`] writes.
///
/// # Errors
///
/// Fails on lines that are neither a preamp nor a PK filter.
pub fn parse_apo(text: &str) -> Result<(f64, Vec<Filter>)> {
    let mut preamp = 0.0;
    let mut filters = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.first() == Some(&"Preamp:") {
            preamp = words.get(1).and_then(|w| w.parse().ok()).unwrap_or(0.0);
            continue;
        }
        let value = |key: &str| {
            words
                .iter()
                .position(|w| *w == key)
                .and_then(|i| words.get(i + 1))
                .and_then(|w| w.parse::<f64>().ok())
        };
        match (value("Fc"), value("Gain"), value("Q")) {
            (Some(freq), Some(gain), Some(q)) if words.contains(&"PK") => {
                filters.push(Filter { freq, gain, q });
            }
            _ => bail!("unrecognised APO line: {line}"),
        }
    }
    Ok((preamp, filters))
}

/// LSP equalizer balance (percent) and output gain (dB) that apply the trims.
///
/// LSP scales left by `(100 - b) / 100` and right by `(100 + b) / 100`; the output
/// gain then brings the louder-side factor back to unity.
#[must_use]
pub fn balance_for_trim(trim_db: [f64; 2]) -> (f64, f64) {
    let ratio = 10f64.powf((trim_db[1] - trim_db[0]) / 20.0); // right / left gain
    let x = (ratio - 1.0) / (ratio + 1.0);
    let louder = 1.0 + x.abs();
    let base = 10f64.powf(trim_db[0].max(trim_db[1]) / 20.0);
    (100.0 * x, 20.0 * (base / louder).log10())
}

fn bands_json(filters: &[Filter], num_bands: usize) -> Value {
    let mut map = serde_json::Map::new();
    for i in 0..num_bands {
        let band = match filters.get(i) {
            Some(f) => json!({
                "type": "Bell", "mode": "APO (DR)", "slope": "x1",
                "solo": false, "mute": false,
                "frequency": f.freq, "gain": f.gain, "q": f.q, "width": 4.0,
            }),
            None => json!({
                "type": "Off", "mode": "APO (DR)", "slope": "x1",
                "solo": false, "mute": false,
                "frequency": 1000.0, "gain": 0.0, "q": 0.71, "width": 4.0,
            }),
        };
        map.insert(format!("band{i}"), band);
    }
    Value::Object(map)
}

/// An EasyEffects 8.x output preset holding one split-channel equalizer.
#[must_use]
pub fn easyeffects_preset(eq: &EqSet) -> Value {
    let num_bands = eq.channels.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let (balance, output_gain) = balance_for_trim(eq.trim_db);
    json!({
        "output": {
            "blocklist": [],
            "plugins_order": ["equalizer#0"],
            "equalizer#0": {
                "bypass": false,
                "input-gain": eq.preamp_db,
                "output-gain": (output_gain * 100.0).round() / 100.0,
                "mode": "IIR",
                "decramp": "Off",
                "split-channels": true,
                "balance": (balance * 100.0).round() / 100.0,
                "pitch-left": 0.0,
                "pitch-right": 0.0,
                "num-bands": num_bands,
                "left": bands_json(&eq.channels[0], num_bands),
                "right": bands_json(&eq.channels[1], num_bands),
            }
        }
    })
}

/// Human-readable summary of the design.
#[must_use]
pub fn report(design: &Design, header: &str) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{header}\n");
    let o = &design.options;
    let _ = writeln!(
        s,
        "Target tilt {:.1} dB/oct, high limit {}, filters {}",
        o.tilt_db_per_oct,
        o.high_limit_label(),
        o.filters_label()
    );
    let _ = writeln!(s, "Preamp {:.1} dB", design.eq.preamp_db);
    let _ = writeln!(
        s,
        "Left/right level difference {:+.1} dB (trim L {:.1} dB, R {:.1} dB)\n",
        design.imbalance_db, design.eq.trim_db[0], design.eq.trim_db[1]
    );
    for (c, name) in ["Left", "Right"].iter().enumerate() {
        let ch = &design.channels[c];
        let _ = writeln!(
            s,
            "{name}: corrected {:.0}–{:.0} Hz{}, RMS deviation {:.1} dB → {:.1} dB",
            ch.target.low_hz,
            ch.target.high_hz,
            if ch.target.roll_off.is_empty() {
                String::new()
            } else {
                format!(
                    " (resonances cut down to {:.0} Hz)",
                    ch.target.cut_floor_hz()
                )
            },
            ch.rms_before_db,
            ch.rms_after_db
        );
        if let Some(auto) = &ch.auto {
            let curve: Vec<String> = auto
                .error_by_count
                .iter()
                .map(|(k, e)| format!("{k}:{e:.2}"))
                .collect();
            let _ = writeln!(
                s,
                "  {} filters chosen automatically; correctable error (dB) by count: {}",
                auto.chosen,
                curve.join(" ")
            );
        }
        for (i, f) in ch.filters.iter().enumerate() {
            let _ = writeln!(
                s,
                "  {:2}. {:>7.1} Hz  {:>+5.1} dB  Q {:.2}",
                i + 1,
                f.freq,
                f.gain,
                f.q
            );
        }
        let _ = writeln!(s);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EqSet {
        EqSet {
            preamp_db: -3.2,
            trim_db: [-1.5, 0.0],
            channels: [
                vec![
                    Filter {
                        freq: 96.7,
                        gain: -7.6,
                        q: 14.5,
                    },
                    Filter {
                        freq: 1000.0,
                        gain: 2.0,
                        q: 1.0,
                    },
                ],
                vec![Filter {
                    freq: 45.2,
                    gain: -8.4,
                    q: 9.5,
                }],
            ],
        }
    }

    #[test]
    fn apo_round_trips() {
        let eq = sample();
        for c in 0..2 {
            let (preamp, filters) = parse_apo(&apo_text(&eq, c)).expect("parses own output");
            assert!((preamp - (eq.preamp_db + eq.trim_db[c])).abs() < 1e-9);
            assert_eq!(filters, eq.channels[c]);
        }
    }

    #[test]
    fn balance_trims_left_only() {
        let (b, out) = balance_for_trim([-2.0, 0.0]);
        let left = (100.0 - b) / 100.0 * 10f64.powf(out / 20.0);
        let right = (100.0 + b) / 100.0 * 10f64.powf(out / 20.0);
        assert!((20.0 * left.log10() + 2.0).abs() < 1e-9, "left {left}");
        assert!((20.0 * right.log10()).abs() < 1e-9, "right {right}");
        assert_eq!(balance_for_trim([0.0, 0.0]), (0.0, 0.0));
    }

    #[test]
    fn preset_has_expected_shape() {
        let p = easyeffects_preset(&sample());
        let eq = &p["output"]["equalizer#0"];
        assert_eq!(eq["num-bands"], 2);
        assert_eq!(eq["left"]["band0"]["type"], "Bell");
        assert_eq!(eq["right"]["band1"]["type"], "Off");
        assert_eq!(eq["left"]["band0"]["mode"], "APO (DR)");
        assert_eq!(p["output"]["plugins_order"][0], "equalizer#0");
    }
}
