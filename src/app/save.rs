//! Step 7: writing the EQ files, applying them in the EQ host, and verifying through it.

use std::path::PathBuf;

use anyhow::Result;

use super::keys::KeyCode;
use super::{App, Purpose, SaveState, VerifyResult};
use crate::analysis::Responses;
use crate::audio::Channel;
use crate::eq::export;

impl App {
    pub(super) fn write_outputs(&mut self) {
        self.save = SaveState::default();
        let Some(design) = self.design.clone() else {
            return;
        };
        let header = format!(
            "CorrectSound report, {}\nOutput: {}\nMic: {}{}",
            chrono::Local::now().format("%Y-%m-%d %H:%M"),
            self.sink()
                .map(|s| s.description.clone())
                .unwrap_or_default(),
            self.cal
                .as_ref()
                .and_then(|c| c.serial.clone())
                .unwrap_or_else(|| "unknown".into()),
            if self.simulated() { " (simulated)" } else { "" }
        );
        let files = [
            ("left.txt", export::apo_text(&design.eq, 0)),
            ("right.txt", export::apo_text(&design.eq, 1)),
            (
                "CorrectSound.json",
                serde_json::to_string_pretty(&export::easyeffects_preset(&design.eq))
                    .unwrap_or_default(),
            ),
            (
                "report.txt",
                format!(
                    "{}\n{}",
                    export::report(&design, &header),
                    crate::insights::text(&self.curiosities)
                ),
            ),
        ];
        let responses = self.measure.responses.clone();
        let result = (|| -> Result<Vec<PathBuf>> {
            let session = self.session()?;
            let mut written = Vec::new();
            for (name, text) in &files {
                session.write(name, text)?;
                written.push(session.dir().join(name));
            }
            for (c, r) in responses.iter().enumerate() {
                if let Some(r) = r {
                    let name = if c == 0 {
                        "left-response"
                    } else {
                        "right-response"
                    };
                    session.save_response(name, &r.display)?;
                }
            }
            for (c, ch) in design.channels.iter().enumerate() {
                let name = Channel::BOTH[c].name().to_lowercase();
                session.save_response(&format!("{name}-predicted"), &ch.predicted)?;
            }
            Ok(written)
        })();
        match result {
            Ok(w) => self.save.written = w,
            Err(e) => self.save.error = Some(format!("{e:#}")),
        }
    }

    /// True when the apply step is available.
    #[must_use]
    pub fn can_apply(&self) -> bool {
        self.eq_host_running() && self.design.is_some()
    }

    /// True when the platform's EQ host (EasyEffects on Linux) is running.
    #[must_use]
    pub fn eq_host_running(&self) -> bool {
        self.devices
            .as_ref()
            .and_then(|d| d.eq_host.as_ref())
            .is_some_and(|h| h.running)
    }

    /// The EQ host's name, for messages: "EasyEffects".
    #[must_use]
    pub fn eq_host_name(&self) -> &str {
        self.devices
            .as_ref()
            .and_then(|d| d.eq_host.as_ref())
            .map_or("the EQ host", |h| h.name.as_str())
    }

    pub(super) fn key_save(&mut self, code: KeyCode) {
        if self.measure.running() {
            if code == KeyCode::Char('s') {
                self.measure.cancel();
            }
            return;
        }
        match code {
            KeyCode::Char('a') if self.can_apply() && self.save.applied.is_none() => self.apply(),
            KeyCode::Char('v') if self.reviewing() => {
                self.message = Some(
                    "Verifying plays sweeps and records them; start a new run (correctsound) \
                     for that."
                        .into(),
                );
            }
            KeyCode::Char('v') if self.save.applied.is_some() => {
                // The simulated mic goes back to the first position, as the screen asks.
                if let Some(state) = self.sim_state() {
                    state.lock().expect("sim state lock poisoned").mic_position = 0;
                }
                self.start_sweeps(Purpose::Verify);
            }
            KeyCode::Char('v') => {
                let host = self.eq_host_name();
                self.message = Some(if self.can_apply() {
                    format!(
                        "Verify measures again through {host} with the new EQ in it, so it comes \
                         after applying: press a first."
                    )
                } else {
                    format!("Verify measures through {host}, which is not running.")
                });
            }
            KeyCode::Enter => self.quit = true,
            _ => {}
        }
    }

    pub(super) fn apply(&mut self) {
        let Some(design) = &self.design else { return };
        let eq = design.eq.clone();
        let Some(sink) = self.sink().cloned() else {
            self.save.error = Some("no output device selected".into());
            return;
        };
        let parent = match self.session() {
            Ok(s) => s.dir().to_path_buf(),
            Err(e) => {
                self.set_error(&e);
                return;
            }
        };
        match self.system.apply_eq(&eq, &parent, &sink) {
            Ok(applied) => self.save.applied = Some(applied),
            Err(e) => self.save.error = Some(format!("{e:#}")),
        }
    }

    /// Compares the verification sweep with the prediction. The broadband offset is
    /// removed first (the EQ host's output has its own volume); what remains is shape.
    pub(super) fn finish_verify(&mut self) {
        if let Some(session) = &self.session {
            for (c, v) in self.measure.verify.iter().enumerate() {
                if let Some(v) = v {
                    let name = Channel::BOTH[c].name().to_lowercase();
                    // Same smoothing as `<channel>-predicted.csv`, for comparison.
                    if let Err(e) = session.save_response(&format!("verify-{name}"), &v.fit) {
                        self.save.error = Some(format!("{e:#}"));
                    }
                }
            }
        }
        let Some(design) = &self.design else { return };
        let mut results = [VerifyResult::default(); 2];
        for (c, result) in results.iter_mut().enumerate() {
            let Some(v) = &self.measure.verify[c] else {
                continue;
            };
            let ch = &design.channels[c];
            let expected =
                expected_at_first_position(&ch.predicted, self.measure.responses[c].as_ref());
            let diffs: Vec<(f64, f64)> = ch
                .predicted
                .freqs
                .iter()
                .zip(&expected)
                .zip(&v.fit.db)
                .filter(|((f, _), _)| ch.target.in_range(**f))
                .map(|((&f, p), m)| (f, m - p))
                .collect();
            if diffs.is_empty() {
                continue;
            }
            let mean = diffs.iter().map(|d| d.1).sum::<f64>() / diffs.len() as f64;
            let sq = diffs.iter().map(|d| (d.1 - mean).powi(2)).sum::<f64>();
            let (worst_hz, worst) = diffs
                .iter()
                .copied()
                .max_by(|a, b| (a.1 - mean).abs().total_cmp(&(b.1 - mean).abs()))
                .expect("not empty");
            *result = VerifyResult {
                rms_db: (sq / diffs.len() as f64).sqrt(),
                worst_db: worst - mean,
                worst_hz,
            };
        }
        self.save.verify = Some(results);
    }

    /// Verification responses, if measured.
    #[must_use]
    pub fn verify_responses(&self) -> &[Option<Responses>; 2] {
        &self.measure.verify
    }
}

/// What verification should measure, dB per frequency of `predicted`: with several mic
/// positions, the first position's own response with the EQ's effect added (the screen asks
/// for the mic back there), since the prediction itself is their average and one spot
/// differs from it by the room; with one position, the prediction.
fn expected_at_first_position(
    predicted: &crate::dsp::spectrum::Response,
    measured: Option<&crate::analysis::Responses>,
) -> Vec<f64> {
    let first = measured.and_then(|r| r.positions.first().map(|p| (p, &r.fit)));
    match first {
        Some((first, average))
            if first.freqs == predicted.freqs && average.freqs == predicted.freqs =>
        {
            predicted
                .db
                .iter()
                .zip(&average.db)
                .zip(&first.db)
                .map(|((p, a), f)| f + (p - a))
                .collect()
        }
        _ => predicted.db.clone(),
    }
}
