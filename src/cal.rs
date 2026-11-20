//! UMIK calibration files: download, cache, parse and interpolate.

use crate::files as fs;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

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
                && f.is_finite()
                && f > 0.0
                && d.is_finite()
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
    rest[..end].parse::<f64>().ok().filter(|v| v.is_finite())
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

/// The file for `serial` saved by an earlier download, if there is one that parses.
#[must_use]
pub fn saved(
    serial: &Serial,
    orientation: Orientation,
    cache: &Path,
) -> Option<(Calibration, PathBuf)> {
    let path = cache.join(orientation.file_name(serial));
    let cal = Calibration::parse(&fs::read_to_string(&path).ok()?).ok()?;
    Some((cal, path))
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
    if let Some(saved) = saved(serial, orientation, cache) {
        return Ok(saved);
    }
    let path = cache.join(orientation.file_name(serial));
    let url = orientation.url(serial);
    let text = download(&url)?;
    if text.contains("Unable to locate calibration data") {
        bail!("miniDSP has no calibration file for serial {serial}; check the number on the mic");
    }
    let cal = Calibration::parse(&text).with_context(|| format!("parsing {url}"))?;
    fs::create_dir_all(cache).with_context(|| format!("creating {}", cache.display()))?;
    fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    Ok((cal, path))
}

/// The text at `url`.
#[cfg(not(target_family = "wasm"))]
fn download(url: &str) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .into();
    agent
        .get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?
        .body_mut()
        .read_to_string()
        .with_context(|| format!("reading {url}"))
}

/// A web page may not download from miniDSP: its server does not let other sites read its
/// files.
#[cfg(target_family = "wasm")]
fn download(url: &str) -> Result<String> {
    bail!("a web page cannot download {url}; the installed app can")
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
    fn a_saved_file_is_used_without_downloading() {
        let cache = std::env::temp_dir().join(format!("correctsound-cal-{}", std::process::id()));
        fs::create_dir_all(&cache).expect("cache directory");
        let serial: Serial = "700-0000".parse().expect("serial");
        assert!(saved(&serial, Orientation::OnAxis, &cache).is_none());
        fs::write(
            cache.join("7000000_90deg.txt"),
            include_str!("../tests/data/example-cal_90deg.txt"),
        )
        .expect("saved file");
        // Only the 90° file is saved: the 0° one is still missing.
        assert!(saved(&serial, Orientation::OnAxis, &cache).is_none());
        let (_, path) = saved(&serial, Orientation::Vertical, &cache).expect("the saved file");
        assert!(path.ends_with("7000000_90deg.txt"));
        // fetch takes it too, so no download (which would fail here, for a made-up serial).
        let (cal, _) = fetch(&serial, Orientation::Vertical, &cache).expect("no download");
        assert_eq!(cal.len(), 615);
        // A file that does not parse is not used.
        fs::write(cache.join("7000000.txt"), "<!doctype html>").expect("broken file");
        assert!(saved(&serial, Orientation::OnAxis, &cache).is_none());
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn rejects_html() {
        assert!(Calibration::parse("<!doctype html>\n<p>404</p>").is_err());
    }

    #[test]
    fn rejects_non_finite_values() {
        let only_bad = "20.0 NaN\n100.0 inf\n1000.0 -inf\n+inf 0.0\n1e500 0.0\n";
        assert!(Calibration::parse(only_bad).is_err());

        let mixed = "20.0 -1.0\n100.0 NaN\n1000.0 inf\n1e500 0.0\n20000.0 1.0\n";
        let cal = Calibration::parse(mixed).expect("valid finite rows remain");
        assert_eq!(cal.len(), 2);
        assert!(cal.db_at(500.0).is_finite());
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
