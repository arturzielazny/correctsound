//! Step 6: designing the equalizer from the measurement, and the keys that change it.

use crossterm::event::KeyCode;

use super::{App, Step};
use crate::eq::fit::design_with_positions;
use crate::eq::target::{DesignOptions, HIGH_LIMIT_PRESETS};

impl App {
    pub(super) fn redesign(&mut self) {
        let [Some(l), Some(r)] = &self.measure.responses else {
            return;
        };
        let design = design_with_positions(
            [&l.fit, &r.fit],
            [&l.positions, &r.positions],
            &self.design_options,
        );
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
    }

    pub(super) fn key_result(&mut self, code: KeyCode) {
        let o = &mut self.design_options;
        match code {
            KeyCode::Tab => {
                self.view_channel = 1 - self.view_channel;
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
                self.go(Step::Save);
                return;
            }
            _ => return,
        }
        self.redesign();
    }
}
