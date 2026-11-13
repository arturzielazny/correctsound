//! UMIK calibration files: download, cache, parse and interpolate.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::dsp::spectrum::interpolate_log;

/// Which way the mic points; selects the 0° or the 90° calibration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Orientation {
    /// Mic tip points at the speakers.
    #[default]
    OnAxis,
    /// Mic tip points at the ceiling.
    Vertical,
}

impl Orientation {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::OnAxis => "0° — mic points at the speakers (best for stereo pairs)",
            Self::Vertical => "90° — mic points at the ceiling",
        }
    }

    fn url(self, serial: &Serial) -> String {
        match self {
            Self::OnAxis => {
                format!("https://www.minidsp.com/scripts/umikcal/umik.php/{serial}.txt")
            }
            Self::Vertical => {
                format!("https://www.minidsp.com/scripts/umikcal/umik90.php/{serial}_90deg.txt")
            }
        }
    }

    fn file_name(self, serial: &Serial) -> String {
        match self {
            Self::OnAxis => format!("{serial}.txt"),
            Self::Vertical => format!("{serial}_90deg.txt"),
        }
    }
}

/// A UMIK serial number: seven digits, entered with or without the dash (`700-1234`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Serial(String);

impl FromStr for Serial {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let digits: String = s.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
        if digits.len() == 7 && digits.chars().all(|c| c.is_ascii_digit()) {
            Ok(Self(digits))
        } else {
            bail!("a UMIK serial number has 7 digits, like 700-1234")
        }
    }
}

impl TryFrom<String> for Serial {
    type Error = anyhow::Error;
    fn try_from(s: String) -> Result<Self> {
        s.parse()
    }
}

impl From<Serial> for String {
    fn from(s: Serial) -> Self {
        s.0
    }
}

impl fmt::Display for Serial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A parsed calibration file.
#[derive(Debug, Clone, PartialEq)]
pub struct Calibration {
    pub sens_factor: f64,
    /// Present on old-generation mics only; its meaning for SPL is unverified.
    pub again: Option<f64>,
    pub serial: Option<String>,
    freqs: Vec<f64>,
    db: Vec<f64>,
}

impl Calibration {
    /// Builds a calibration from points, for tests and the simulator.
    #[must_use]
    pub fn from_points(sens_factor: f64, points: &[(f64, f64)]) -> Self {
        Self {
            sens_factor,
            again: None,
            serial: None,
            freqs: points.iter().map(|p| p.0).collect(),
            db: points.iter().map(|p| p.1).collect(),
        }
    }

    /// Parses miniDSP/REW calibration text.
    ///
    /// # Errors
    ///
    /// Fails if no data rows are present or the text is an HTML error page.
    pub fn parse(text: &str) -> Result<Self> {
        let mut sens_factor = None;
        let mut again = None;
        let mut serial = None;
        let mut points: Vec<(f64, f64)> = Vec::new();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('*') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('<') {
                bail!("the server did not return a calibration file (is the serial number right?)");
            }
            if line.starts_with('"') || line.chars().next().is_some_and(char::is_alphabetic) {
                sens_factor = sens_factor.or_else(|| header_number(line, "Sens Factor"));
                again = again.or_else(|| header_number(line, "AGain"));
                serial = serial.or_else(|| header_word(line, "SERNO"));
                continue;
            }
            let mut cols = line
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty());
            let (Some(f), Some(d)) = (cols.next(), cols.next()) else {
                continue;
            };
            if let (Ok(f), Ok(d)) = (f.parse::<f64>(), d.parse::<f64>())
                && f > 0.0
            {
                points.push((f, d));
            }
        }
        if points.len() < 2 {
            bail!("calibration file has no frequency/dB rows");
        }
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(Self {
            sens_factor: sens_factor.unwrap_or(0.0),
            again,
            serial,
            freqs: points.iter().map(|p| p.0).collect(),
            db: points.iter().map(|p| p.1).collect(),
        })
    }

    /// Mic response in dB at `freq`; subtract it from a measurement.
    #[must_use]
    pub fn db_at(&self, freq: f64) -> f64 {
        interpolate_log(&self.freqs, &self.db, freq)
    }

    /// True for the old 18 dB-gain UMIK-1, whose SPL constant is unverified.
    #[must_use]
    pub fn is_old_generation(&self) -> bool {
        self.again.is_some()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.freqs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.freqs.is_empty()
    }
}

fn header_number(line: &str, key: &str) -> Option<f64> {
    let rest = &line[line.find(key)? + key.len()..];
    let rest = rest.trim_start_matches([' ', '=', ':']);
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.')))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn header_word(line: &str, key: &str) -> Option<String> {
    let rest = &line[line.find(key)? + key.len()..];
    let rest = rest.trim_start_matches([' ', '=', ':']);
    let word: String = rest
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    (!word.is_empty()).then_some(word)
}

/// Where calibration files are cached.
#[must_use]
pub fn cache_dir() -> PathBuf {
    crate::paths::data_dir().join("cal")
}

/// Returns the cached file for `serial`, downloading it if needed.
///
/// # Errors
///
/// Fails on network errors, an unknown serial, or a file that does not parse.
pub fn fetch(
    serial: &Serial,
    orientation: Orientation,
    cache: &Path,
) -> Result<(Calibration, PathBuf)> {
    let path = cache.join(orientation.file_name(serial));
    if let Ok(text) = fs::read_to_string(&path)
        && let Ok(cal) = Calibration::parse(&text)
    {
        return Ok((cal, path));
    }
    let url = orientation.url(serial);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into();
    let text = agent
        .get(&url)
        .call()
        .with_context(|| format!("downloading {url}"))?
        .body_mut()
        .read_to_string()
        .with_context(|| format!("reading {url}"))?;
    if text.contains("Unable to locate calibration data") {
        bail!("miniDSP has no calibration file for serial {serial}; check the number on the mic");
    }
    let cal = Calibration::parse(&text).with_context(|| format!("parsing {url}"))?;
    fs::create_dir_all(cache).with_context(|| format!("creating {}", cache.display()))?;
    fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    Ok((cal, path))
}

/// Loads a calibration file from disk.
///
/// # Errors
///
/// Fails if the file cannot be read or parsed.
pub fn load(path: &Path) -> Result<Calibration> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Calibration::parse(&text).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_files_in_the_umik_format() {
        // Made-up curves in the layout miniDSP serves: a quoted header, CRLF, 615 rows.
        let on =
            Calibration::parse(include_str!("../tests/data/example-cal.txt")).expect("0° file");
        assert_eq!(on.len(), 615);
        assert!((on.sens_factor + 0.5).abs() < 1e-9);
        assert_eq!(on.serial.as_deref(), Some("7000000"));
        assert!(!on.is_old_generation());
        assert!((on.db_at(10.054) + 2.6).abs() < 1e-9);
        assert!((on.db_at(1.0) + 2.6).abs() < 1e-9, "holds below range");
        let v = Calibration::parse(include_str!("../tests/data/example-cal_90deg.txt"))
            .expect("90° file");
        assert_eq!(v.len(), 615);
    }

    #[test]
    fn parses_old_format_with_again_and_phase() {
        let text = "\"Sens Factor =-18.5dB, AGain =18dB, SERNO: 7001234\"\r\n* comment\r\n20\t-1.0\t3.2\r\n1000\t0.0\t0\r\n20000\t2.5\t0\r\n";
        let c = Calibration::parse(text).expect("parses");
        assert_eq!(c.sens_factor, -18.5);
        assert_eq!(c.again, Some(18.0));
        assert_eq!(c.serial.as_deref(), Some("7001234"));
        assert!(c.is_old_generation());
        assert!((c.db_at(1000.0)).abs() < 1e-12);
    }

    #[test]
    fn rejects_html() {
        assert!(Calibration::parse("<!doctype html>\n<p>404</p>").is_err());
    }

    #[test]
    fn serial_formats() {
        assert_eq!(
            "700-1234".parse::<Serial>().expect("dash").to_string(),
            "7001234"
        );
        assert_eq!(
            "7001234".parse::<Serial>().expect("plain").to_string(),
            "7001234"
        );
        assert!("72033".parse::<Serial>().is_err());
        assert!("720330a".parse::<Serial>().is_err());
    }
}
