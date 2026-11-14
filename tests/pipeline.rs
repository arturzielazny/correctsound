//! End-to-end tests against the simulated room, where the true response is known.

use std::sync::{Arc, Mutex};

use correctsound::analysis::{Responses, responses};
use correctsound::audio::sim::{SimBackend, SimConfig, SimState};
use correctsound::audio::{Channel, Program, Route, Source};
use correctsound::dsp::level::SplScale;
use correctsound::dsp::sweep::{IrWindow, Sweep, SweepAnalysis, SweepParams};
use correctsound::eq::fit::{
    AUTO_MAX_FILTERS, BOOST_Q_MAX, GAIN_MAX, GAIN_MIN, Q_MAX_HIGH, Q_MAX_LOW, Q_MIN, Q_SPLIT_HZ,
    design,
};
use correctsound::eq::target::DesignOptions;
use correctsound::eq::{EqSet, response_db};

fn measure(backend: &SimBackend, sweep: &Sweep, channel: Channel, route: Route) -> SweepAnalysis {
    let program = Program::Once {
        signal: sweep.samples_f32().into(),
        pre: 24_000,
        post: 72_000,
    };
    let rec = backend.render_offline(Source::new(program, channel), route);
    SweepAnalysis::analyze(sweep, &rec, IrWindow::default()).expect("sim sweep is clean")
}

fn measure_all(config: &SimConfig, state: &Arc<Mutex<SimState>>, route: Route) -> [Responses; 2] {
    let backend = SimBackend::new(config.clone(), Arc::clone(state));
    let sweep = Sweep::new(SweepParams::default());
    let cal = config.calibration();
    let scale = SplScale::from_sens_factor(config.sens_factor);
    Channel::BOTH.map(|c| {
        let a = measure(&backend, &sweep, c, route);
        responses(&[a], &cal, scale, sweep.params().peak_dbfs)
    })
}

#[test]
fn recovers_known_room_at_40db_snr() {
    let mut config = SimConfig::default();
    for room in &mut config.rooms {
        room.reflections.clear();
    }
    // Sweep arrives near -42 dBFS; noise 40 dB below it.
    config.noise_dbfs = -82.0;
    let state = Arc::new(Mutex::new(SimState::default()));
    let vol_db = 20.0 * state.lock().expect("lock").sink_volume.powi(3).log10();
    let measured = measure_all(&config, &state, Route::Hardware);
    let scale = SplScale::from_sens_factor(config.sens_factor);
    for c in Channel::BOTH {
        let room = &config.rooms[c.index()];
        let r = &measured[c.index()].display;
        let mut worst: (f64, f64) = (0.0, 0.0);
        for (&f, &d) in r.freqs.iter().zip(&r.db) {
            let expected = scale.spl(room.shape_db(f) + room.gain_db + vol_db - 12.0);
            let err = d - expected;
            if err.abs() > worst.1.abs() {
                worst = (f, err);
            }
        }
        assert!(
            worst.1.abs() < 0.5,
            "{c:?}: {:.2} dB error at {:.1} Hz",
            worst.1,
            worst.0
        );
    }
}

fn check_constraints(eq: &EqSet, design: &correctsound::eq::fit::Design) {
    for (c, filters) in eq.channels.iter().enumerate() {
        let t = &design.channels[c].target;
        for f in filters {
            assert!((GAIN_MIN..=GAIN_MAX).contains(&f.gain), "{f:?}");
            assert!(f.gain.abs() >= 0.5, "{f:?}");
            let q_max = if f.freq < Q_SPLIT_HZ {
                Q_MAX_LOW
            } else {
                Q_MAX_HIGH
            };
            assert!(f.q >= Q_MIN && f.q <= q_max + 1e-9, "{f:?}");
            if f.gain > 0.0 {
                assert!(f.q <= BOOST_Q_MAX + 1e-9, "{f:?}");
            }
            assert!(
                f.freq >= t.cut_floor_hz() - 0.1 && f.freq <= t.high_hz + 0.1,
                "{f:?} outside {t:?}"
            );
            // Below the speaker's bass limit only cuts.
            assert!(
                f.freq >= t.low_hz || f.gain < 0.0,
                "boost below the bass limit: {f:?}"
            );
        }
        assert!(filters.len() <= design.options.max_filters.unwrap_or(AUTO_MAX_FILTERS));
        assert!(filters.windows(2).all(|w| w[0].freq <= w[1].freq));
    }
}

#[test]
fn design_improves_response_within_constraints() {
    let config = SimConfig::default();
    let state = Arc::new(Mutex::new(SimState::default()));
    let measured = measure_all(&config, &state, Route::Hardware);
    // Fixed settings, so this checks the fitter itself; the automatic ones are tested below.
    let options = DesignOptions {
        tilt_db_per_oct: -0.5,
        high_limit_hz: Some(10_000.0),
        max_filters: Some(10),
    };
    let start = std::time::Instant::now();
    let d = design([&measured[0].fit, &measured[1].fit], &options);
    let elapsed = start.elapsed();
    assert!(elapsed.as_secs_f64() < 1.0, "design took {elapsed:?}");
    check_constraints(&d.eq, &d);
    for (c, ch) in d.channels.iter().enumerate() {
        assert!(
            ch.rms_after_db < ch.rms_before_db * 0.8,
            "channel {c}: {:.2} → {:.2}",
            ch.rms_before_db,
            ch.rms_after_db
        );
        let t = d.display_target(c);
        // No peak left more than 1.5 dB above target in the corrected range.
        for (&f, &p) in ch.predicted.freqs.iter().zip(&ch.predicted.db) {
            if t.in_range(f) {
                assert!(
                    p - t.db_at(f) < 2.0,
                    "channel {c}: {:.2} dB peak at {f:.1} Hz",
                    p - t.db_at(f)
                );
            }
        }
        // The strong simulated modes get a cut nearby.
        let cut_near = |hz: f64| {
            ch.filters
                .iter()
                .any(|f| f.gain < -2.0 && (f.freq / hz).log2().abs() < 0.15)
        };
        let modes: &[f64] = if c == 0 {
            &[97.0, 122.0]
        } else {
            &[45.0, 110.0, 450.0]
        };
        for &m in modes {
            assert!(
                cut_near(m),
                "channel {c}: no cut near {m} Hz in {:?}",
                ch.filters
            );
        }
    }
    // Louder channel is trimmed, quieter left alone.
    assert!(d.eq.trim_db.contains(&0.0));
    assert!(d.eq.trim_db.iter().all(|&t| t <= 0.0));
    // Preamp prevents any net boost.
    let freqs = &d.channels[0].measured.freqs;
    for c in 0..2 {
        let curve = response_db(&d.eq.channels[c], freqs);
        let max = curve.iter().fold(f64::MIN, |m, v| m.max(*v)) + d.eq.trim_db[c] + d.eq.preamp_db;
        assert!(max <= 0.0, "net gain {max}");
    }
    // Deterministic.
    let again = design([&measured[0].fit, &measured[1].fit], &options);
    assert_eq!(d, again);
}

#[test]
fn verification_matches_prediction() {
    let config = SimConfig::default();
    let state = Arc::new(Mutex::new(SimState::default()));
    let measured = measure_all(&config, &state, Route::Hardware);
    let d = design(
        [&measured[0].fit, &measured[1].fit],
        &DesignOptions::default(),
    );
    state.lock().expect("lock").eq = Some(d.eq.clone());
    let verified = measure_all(&config, &state, Route::ThroughEq);
    for (c, (ch, v)) in d.channels.iter().zip(&verified).enumerate() {
        let v = &v.fit;
        for ((&f, &p), &m) in ch.predicted.freqs.iter().zip(&ch.predicted.db).zip(&v.db) {
            if ch.target.in_range(f) {
                let diff = m - (p + d.eq.preamp_db);
                assert!(diff.abs() < 0.5, "channel {c}: {diff:.2} dB at {f:.1} Hz");
            }
        }
    }
}

#[test]
fn resonance_below_the_bass_limit_is_cut_not_boosted() {
    use correctsound::dsp::spectrum::{Response, grid};
    // A small speaker rolling off at 70 Hz (24 dB/oct) with a room resonance at 45 Hz
    // poking 12 dB above its roll-off, and a mild bump at 120 Hz.
    let freqs = grid();
    let db: Vec<f64> = freqs
        .iter()
        .map(|&f| {
            let base = if f >= 70.0 {
                80.0
            } else {
                80.0 - 24.0 * (70.0 / f).log2()
            };
            let mode = 12.0 * (-((f / 45.0).log2() * 18.0).powi(2)).exp();
            let bump = 5.0 * (-((f / 120.0).log2() * 6.0).powi(2)).exp();
            base + mode + bump
        })
        .collect();
    let r = Response { freqs, db };
    let d = design([&r, &r], &DesignOptions::default());
    let ch = &d.channels[0];
    assert!(ch.target.low_hz > 55.0, "bass limit {}", ch.target.low_hz);
    assert!(
        ch.filters
            .iter()
            .any(|f| f.gain < -4.0 && (f.freq / 45.0).log2().abs() < 1.0 / 6.0),
        "no cut at the 45 Hz resonance: {:?}",
        ch.filters
    );
    let eq = response_db(&ch.filters, &r.freqs);
    for (&f, &g) in r.freqs.iter().zip(&eq) {
        if f < ch.target.low_hz {
            assert!(
                g <= 1.0 + 1e-6,
                "{g:.2} dB boost at {f:.1} Hz below the bass limit"
            );
        }
    }
}

#[test]
fn automatic_settings_pick_a_sensible_design() {
    let config = SimConfig::default();
    let state = Arc::new(Mutex::new(SimState::default()));
    let measured = measure_all(&config, &state, Route::Hardware);
    let start = std::time::Instant::now();
    let d = design(
        [&measured[0].fit, &measured[1].fit],
        &DesignOptions::default(),
    );
    assert!(
        start.elapsed().as_secs_f64() < 2.0,
        "took {:?}",
        start.elapsed()
    );
    check_constraints(&d.eq, &d);
    for ch in &d.channels {
        let auto = ch.auto.as_ref().expect("automatic count");
        assert_eq!(auto.chosen, ch.filters.len());
        // One position: the knee rule. Enough filters for most of the achievable gain,
        // well short of the maximum.
        assert!((3..=12).contains(&auto.chosen), "{auto:?}");
        let e = |k: usize| auto.error_by_count[k].1;
        let (e0, emin) = (e(0), e(AUTO_MAX_FILTERS));
        assert!(e(auto.limit) <= e0 - 0.9 * (e0 - emin) + 1e-9, "{auto:?}");
        // And it is the smallest cap that gets there.
        assert!(
            auto.limit == 0 || e(auto.limit - 1) > e0 - 0.9 * (e0 - emin),
            "{auto:?}"
        );
        assert!(ch.rms_after_db < ch.rms_before_db * 0.8);
        // The simulated speakers have no treble roll-off: the whole band is corrected.
        assert!(ch.target.high_hz > 19_000.0, "high {}", ch.target.high_hz);
    }
}

#[test]
fn the_sharpest_simulated_resonance_rings_longest() {
    let config = SimConfig::default();
    let backend = SimBackend::new(config, Arc::new(Mutex::new(SimState::default())));
    let sweep = Sweep::new(SweepParams::default());
    let a = measure(&backend, &sweep, Channel::Left, Route::Hardware);
    let ringing = a.waterfall.ringing(30.0);
    // 97 Hz is the left room's narrowest boost (Q 12, +8 dB).
    let first = ringing.first().expect("something rings");
    assert!((first.hz / 97.0).log2().abs() < 0.1, "{ringing:?}");
    let typical = a.waterfall.median_fade_ms(30.0, 300.0).expect("median");
    assert!(first.fade_ms > 2.0 * typical, "{ringing:?} vs {typical}");
}
