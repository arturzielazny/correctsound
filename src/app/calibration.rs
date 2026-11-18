//! Step 2: the mic's calibration file, downloaded by serial number, and its orientation.

use std::sync::mpsc;

use super::keys::KeyCode;
use super::{App, CalStatus, Step};
use crate::cal::{self, Orientation, Serial};

impl App {
    pub(super) fn key_calibration(&mut self, code: KeyCode) {
        if self.cal_status == CalStatus::Downloading {
            return;
        }
        match code {
            KeyCode::Char(c) if c.is_ascii_digit() || c == '-' => {
                if self.serial_input.len() < 9 {
                    self.serial_input.push(c);
                }
                self.cal = None;
            }
            KeyCode::Backspace => {
                self.serial_input.pop();
                self.cal = None;
            }
            KeyCode::Tab | KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                self.orientation = match self.orientation {
                    Orientation::OnAxis => Orientation::Vertical,
                    Orientation::Vertical => Orientation::OnAxis,
                };
                self.cal = None;
            }
            KeyCode::Enter => {
                if self.cal.is_some() {
                    self.go(Step::MicGain);
                } else {
                    self.start_download();
                }
            }
            _ => {}
        }
    }

    pub(super) fn start_download(&mut self) {
        let serial: Serial = match self.serial_input.parse() {
            Ok(s) => s,
            Err(e) => {
                self.cal_status = CalStatus::Failed(e.to_string());
                return;
            }
        };
        let orientation = self.orientation;
        self.config.serial = Some(serial.clone());
        self.config.orientation = orientation;
        self.config.save();
        self.cal_rx = Some(crate::background::spawn(move || {
            cal::fetch(&serial, orientation, &cal::cache_dir())
        }));
        self.cal_status = CalStatus::Downloading;
    }

    pub(super) fn poll_download(&mut self) {
        let Some(rx) = &self.cal_rx else { return };
        match rx.try_recv() {
            Ok(Ok((c, path))) => {
                self.cal = Some(c);
                self.cal_path = Some(path);
                self.cal_status = CalStatus::Idle;
                self.cal_rx = None;
            }
            Ok(Err(e)) => {
                self.cal_status = CalStatus::Failed(format!("{e:#}"));
                self.cal_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.cal_status = CalStatus::Failed("download stopped unexpectedly".into());
                self.cal_rx = None;
            }
        }
    }
}
