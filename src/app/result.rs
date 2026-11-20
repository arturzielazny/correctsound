//! Step 4: designing the equalizer from the measurement, and the keys that change it.

use super::keys::KeyCode;
use super::{App, Step};
use crate::dsp::spectrum::Response;
use crate::eq::fit::{Design, design_with_positions};
use crate::eq::target::{DesignOptions, HIGH_LIMIT_PRESETS};

impl App {
    /// Works the design out now, for the current measurement and options: on entering the
    /// result screen. A background design under way is no longer wanted.
    pub(super) fn redesign(&mut self) {
        if let Some(job) = self.design_job.take() {
            let _ = crate::background::wait(&job);
        }
        let Some(inputs) = self.design_inputs() else {
            return;
        };
        let design = inputs.design();
        self.apply_design(design);
    }

    /// Makes sure the design is the one for the current options, waiting for the background
    /// design or working it out: before saving.
    pub(super) fn settle_design(&mut self) {
        if let Some(job) = self.design_job.take()
            && let Some((generation, design)) = crate::background::wait(&job)
            && generation == self.measure.generation
        {
            self.apply_design(design);
        }
        if self
            .design
            .as_ref()
            .is_none_or(|d| d.options != self.design_options)
        {
            self.redesign();
        }
    }

    /// Starts working the design out in the background after a key changed the options, so
    /// the screen keeps answering keys; the chart follows when it is ready. Keys pressed
    /// meanwhile only change the options: when the design under way is done, the latest
    /// options are designed next, once.
    pub(super) fn redesign_soon(&mut self) {
        if self.design_job.is_some() {
            return;
        }
        let Some(inputs) = self.design_inputs() else {
            return;
        };
        let generation = self.measure.generation;
        self.design_job = Some(crate::background::spawn(move || {
            (generation, inputs.design())
        }));
    }

    /// Takes a finished background design, and starts the next one when the options changed
    /// while it was worked out.
    pub(super) fn poll_design(&mut self) {
        let Some(job) = &self.design_job else { return };
        match job.try_recv() {
            Ok((generation, design)) => {
                self.design_job = None;
                if generation != self.measure.generation {
                    return;
                }
                let stale = design.options != self.design_options;
                self.apply_design(design);
                if stale {
                    self.redesign_soon();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.design_job = None,
        }
    }

    /// True while a design is being worked out in the background.
    #[must_use]
    pub fn designing(&self) -> bool {
        self.design_job.is_some()
    }

    /// What a design is made from, owned so it can be worked out on another thread.
    fn design_inputs(&self) -> Option<DesignInputs> {
        let [Some(l), Some(r)] = &self.measure.responses else {
            return None;
        };
        Some(DesignInputs {
            fit: [l.fit.clone(), r.fit.clone()],
            positions: [l.positions.clone(), r.positions.clone()],
            options: self.design_options,
        })
    }

    fn apply_design(&mut self, design: Design) {
        let [Some(l), Some(r)] = &self.measure.responses else {
            return;
        };
        self.curiosities = crate::insights::insights(&crate::insights::Inputs {
            responses: [l, r],
            analyses: [&self.measure.positions[0], &self.measure.positions[1]],
            design: &design,
            ambient_spl: self.level.ambient_spl,
            // Only when the level was worked out from the room.
            level_spl: self
                .level
                .room_noise
                .map(|_| self.level.set_spl().unwrap_or(self.level.target_spl)),
            needed_spl: self.level.room_noise.map(|_| self.level.target_spl),
            bass_noise_spl: self.level.room_noise.map(|n| n[1]),
        });
        self.design = Some(design);
        self.designs_run += 1;
    }

    pub(super) fn key_result(&mut self, code: KeyCode) {
        let o = &mut self.design_options;
        match code {
            KeyCode::Tab => {
                self.show_next_channels();
                return;
            }
            KeyCode::Char('t') => o.tilt_db_per_oct = (o.tilt_db_per_oct - 0.25).max(-1.5),
            KeyCode::Char('T') => o.tilt_db_per_oct = (o.tilt_db_per_oct + 0.25).min(0.0),
            KeyCode::Char('h') => {
                let i = HIGH_LIMIT_PRESETS
                    .iter()
                    .position(|&h| h == o.high_limit_hz)
                    .unwrap_or(0);
                o.high_limit_hz = HIGH_LIMIT_PRESETS[(i + 1) % HIGH_LIMIT_PRESETS.len()];
            }
            // From automatic, f/F step from the count it chose (the larger of the two).
            KeyCode::Char('f' | 'F') => {
                let auto = self.design.as_ref().map_or(10, |d| {
                    d.channels
                        .iter()
                        .map(|c| c.filters.len())
                        .max()
                        .unwrap_or(10)
                });
                let current = o.max_filters.unwrap_or(auto);
                o.max_filters = Some(if code == KeyCode::Char('f') {
                    (current + 1).min(20)
                } else {
                    current.saturating_sub(1).max(1)
                });
            }
            KeyCode::Char('d') => *o = DesignOptions::default(),
            KeyCode::Enter => {
                // Saving takes the design for the options as they are now.
                self.settle_design();
                self.go(Step::Save);
                return;
            }
            _ => return,
        }
        self.redesign_soon();
    }
}

/// The measurement and options a design is made from.
struct DesignInputs {
    fit: [Response; 2],
    positions: [Vec<Response>; 2],
    options: DesignOptions,
}

impl DesignInputs {
    fn design(&self) -> Design {
        design_with_positions(
            [&self.fit[0], &self.fit[1]],
            [&self.positions[0], &self.positions[1]],
            &self.options,
        )
    }
}
