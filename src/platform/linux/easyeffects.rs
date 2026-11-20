//! EasyEffects control via its command line (`easyeffects -b`, `-l`) and preset files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::eq::EqSet;
use crate::eq::export::easyeffects_preset;
use crate::paths::{easyeffects_config_dir, easyeffects_data_dir};
use crate::platform::Applied;

/// Installed presets are named `CoSo-<sequence>-<DDMMYYYY>`, e.g. `CoSo-007-13112026`.
pub const PRESET_PREFIX: &str = "CoSo";

/// Runs `easyeffects` with a timeout (it talks to the running instance and exits).
fn cli(args: &[&str]) -> Result<String> {
    let mut child = Command::new("easyeffects")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("running easyeffects")?;
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let mut out = String::new();
            if let Some(mut s) = child.stdout.take() {
                use std::io::Read;
                s.read_to_string(&mut out)?;
            }
            if !status.success() {
                bail!("easyeffects {} failed", args.join(" "));
            }
            return Ok(out);
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            bail!("easyeffects {} timed out", args.join(" "));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// True if an EasyEffects process is running.
fn running() -> bool {
    Command::new("pgrep")
        .args(["-x", "easyeffects"])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Global bypass state: `Some(true)` bypassed, `Some(false)` active, `None` not running.
///
/// # Errors
///
/// Fails if the CLI fails or answers something unexpected.
pub fn bypass() -> Result<Option<bool>> {
    if !running() {
        return Ok(None);
    }
    match cli(&["-b", "3"])?.trim() {
        "1" => Ok(Some(true)),
        "2" => Ok(Some(false)),
        other => bail!("unexpected answer from easyeffects -b 3: {other:?}"),
    }
}

/// Sets global bypass.
///
/// # Errors
///
/// Fails if the CLI fails.
pub fn set_bypass(on: bool) -> Result<()> {
    cli(&["-b", if on { "1" } else { "2" }])?;
    Ok(())
}

/// The output preset EasyEffects loaded last, if it says.
#[must_use]
pub fn last_output_preset() -> Option<String> {
    let name = cli(&["-a", "output"]).ok()?.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// An autoload file written during this session, and whether a pre-existing file was
/// backed up before CorrectSound first wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoloadBackup {
    pub path: PathBuf,
    pub had_original: bool,
}

/// What EasyEffects was doing before the first apply, to roll back to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Before {
    /// The output preset it had loaded last.
    pub preset: Option<String>,
    /// Its global bypass before the app changed it.
    pub bypass: Option<bool>,
    /// The autoload file the apply wrote (a copy of any file it replaced is in the
    /// backup).
    pub autoload: Option<AutoloadBackup>,
}

/// Rolls EasyEffects back to `before`: the autoload file as it was, the preset it had
/// loaded, its bypass. Returns what was done, for the user.
///
/// # Errors
///
/// Fails if the autoload file cannot be put back or EasyEffects rejects the preset.
pub fn undo(before: &Before, applied: &Applied) -> Result<String> {
    if let Some(autoload) = &before.autoload {
        restore_autoload(autoload, &applied.backup_dir)?;
    }
    let loaded = match &before.preset {
        Some(name) => {
            let out = cli(&["-l", name])?;
            if out.contains("does not exist") {
                bail!("EasyEffects no longer has the preset {name} it had loaded before");
            }
            format!("preset {name} loaded again")
        }
        // Nothing to load: its effects as they were cannot be named, so they are bypassed.
        None => {
            set_bypass(true)?;
            "it had no preset loaded before, so its effects are bypassed now".to_owned()
        }
    };
    if before.preset.is_some()
        && let Some(on) = before.bypass
    {
        set_bypass(on)?;
    }
    Ok(format!(
        "EasyEffects is back to how it was: {loaded}. {} stays in its presets, to load \
         again any time.",
        applied.preset_name
    ))
}

/// Puts an autoload file written by [`set_autoload`] back as it was: the copy of the one
/// it replaced, from `backup_dir`, or no file when there was none before the first apply.
fn restore_autoload(autoload: &AutoloadBackup, backup_dir: &Path) -> Result<()> {
    if autoload.had_original {
        let copy = backup_dir
            .join("autoload-output")
            .join(autoload.path.file_name().unwrap_or_default());
        if copy.exists() {
            fs::copy(&copy, &autoload.path)
                .with_context(|| format!("restoring {}", autoload.path.display()))?;
        }
    } else if autoload.path.exists() {
        fs::remove_file(&autoload.path)
            .with_context(|| format!("removing {}", autoload.path.display()))?;
    }
    Ok(())
}

/// `outputDevice` from `easyeffectsrc`, if set.
#[must_use]
pub fn configured_output_device() -> Option<String> {
    let text = fs::read_to_string(easyeffects_config_dir().join("db/easyeffectsrc")).ok()?;
    let mut in_section = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line == "[StreamOutputs]";
        } else if in_section && let Some(v) = line.strip_prefix("outputDevice=") {
            return Some(v.to_owned());
        }
    }
    None
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), &dest)
                .with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// The highest sequence number among `CoSo-NNN-…` names.
fn last_sequence<'a>(names: impl IntoIterator<Item = &'a str>) -> u32 {
    names
        .into_iter()
        .filter_map(|n| {
            let rest = n.strip_prefix(PRESET_PREFIX)?.strip_prefix('-')?;
            rest.split('-').next()?.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0)
}

/// The next preset name in `dir`: one more than the highest sequence there, and today's date.
fn next_preset_path(dir: &Path, date: chrono::NaiveDate) -> (String, PathBuf) {
    let names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let seq = last_sequence(names.iter().map(|n| n.trim_end_matches(".json"))) + 1;
    let name = format!("{PRESET_PREFIX}-{seq:03}-{}", date.format("%d%m%Y"));
    let path = dir.join(format!("{name}.json"));
    (name, path)
}

/// The output EasyEffects should load a preset for automatically: the sink's node name
/// and description, and the description of its active route (port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoloadTarget {
    pub device: String,
    pub description: String,
    pub route: String,
}

/// The file EasyEffects 8 looks up for an output: `<node name>:<route description>.json`,
/// with "/" replaced (presets_autoload_manager.cpp); only its `preset-name` is read.
fn autoload_path(dir: &Path, target: &AutoloadTarget) -> PathBuf {
    dir.join(format!(
        "{}:{}.json",
        target.device.replace('/', "_"),
        target.route.replace('/', "_")
    ))
}

/// Makes `preset` the one EasyEffects loads for `target`, at startup and whenever that
/// output becomes active. An autoload file it replaces is copied to `backup_dir` first
/// (only when `existing` has not already recorded this file's pre-session state). Returns
/// the file written and whether a pre-existing file was backed up.
fn set_autoload(
    dir: &Path,
    target: &AutoloadTarget,
    preset: &str,
    backup_dir: &Path,
    existing: Option<&AutoloadBackup>,
) -> Result<AutoloadBackup> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = autoload_path(dir, target);
    let had_original = if let Some(prev) = existing.filter(|p| p.path == path) {
        prev.had_original
    } else if path.exists() {
        let saved = backup_dir.join("autoload-output");
        let copy = saved.join(path.file_name().unwrap_or_default());
        if !copy.exists() {
            fs::create_dir_all(&saved)?;
            fs::copy(&path, &copy).with_context(|| format!("backing up {}", path.display()))?;
        }
        true
    } else {
        false
    };
    let json = serde_json::json!({
        "device": target.device,
        "device-description": target.description,
        "device-profile": target.route,
        "preset-name": preset,
    });
    fs::write(&path, serde_json::to_string_pretty(&json)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(AutoloadBackup { path, had_original })
}

/// Backs up the live settings, writes the preset, loads it and turns bypass off. With an
/// `autoload` target the preset also becomes the one EasyEffects loads for that output, so
/// it survives EasyEffects restarting without saving its state.
///
/// Returns the autoload backup state, if any, beside what was applied.
///
/// # Errors
///
/// Fails if the backup or preset cannot be written, or EasyEffects rejects the preset.
pub fn apply(
    eq: &EqSet,
    backup_parent: &Path,
    autoload: Option<&AutoloadTarget>,
    existing_autoload: Option<&AutoloadBackup>,
) -> Result<(Applied, Option<AutoloadBackup>)> {
    let backup_dir = backup_parent.join("easyeffects-backup");
    let db = easyeffects_config_dir().join("db");
    // Applying again in the same session must keep the backup of the settings from
    // before the first apply, not replace it with our own preset.
    if !backup_dir.exists() {
        if db.is_dir() {
            copy_dir(&db, &backup_dir.join("db"))?;
        } else {
            fs::create_dir_all(&backup_dir)?;
        }
    }
    let dir = easyeffects_data_dir().join("output");
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let (name, path) = next_preset_path(&dir, chrono::Local::now().date_naive());
    let json = serde_json::to_string_pretty(&easyeffects_preset(eq))?;
    fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    let out = cli(&["-l", &name])?;
    if out.contains("does not exist") {
        bail!("EasyEffects did not find the preset {name}");
    }
    set_bypass(false)?;
    let (autoload, file) = match autoload {
        Some(target) => {
            let dir = easyeffects_data_dir().join("autoload/output");
            let file = set_autoload(&dir, target, &name, &backup_dir, existing_autoload)?;
            (Some(target.route.clone()), Some(file))
        }
        None => (None, None),
    };
    let applied = Applied {
        preset_name: name,
        preset_path: path,
        backup_dir,
        autoload,
    };
    Ok((applied, file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_names_continue_the_sequence() {
        assert_eq!(last_sequence(["CorrectSound", "Music"]), 0);
        assert_eq!(
            last_sequence([
                "CoSo-001-12112026",
                "CoSo-012-13112026",
                "CoSo-x",
                "CoSoX-099-1"
            ]),
            12
        );

        let dir = std::env::temp_dir().join(format!("correctsound-presets-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 11, 3).expect("valid date");
        let (name, path) = next_preset_path(&dir, date);
        assert_eq!(name, "CoSo-001-03112026");
        fs::write(&path, "{}").expect("write preset");
        fs::write(dir.join("CoSo-041-01012026.json"), "{}").expect("write preset");
        assert_eq!(next_preset_path(&dir, date).0, "CoSo-042-03112026");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn autoload_file_matches_easyeffects_naming_and_keeps_a_backup() {
        let root =
            std::env::temp_dir().join(format!("correctsound-autoload-{}", std::process::id()));
        let (dir, backup) = (root.join("autoload/output"), root.join("backup"));
        let target = AutoloadTarget {
            device: "alsa_output.pci-0000_c1_00.1.HiFi__HDMI4__sink".into(),
            description: "HDMI 4".into(),
            route: "HDMI / DisplayPort 4 Output".into(),
        };
        let read = |p: &Path| -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(p).expect("read")).expect("json")
        };
        // Pre-existing autoload file before CorrectSound first applies.
        fs::create_dir_all(&dir).expect("autoload dir");
        let existing_path = autoload_path(&dir, &target);
        fs::write(&existing_path, r#"{"preset-name": "UserPreset"}"#).expect("pre-existing");
        let first =
            set_autoload(&dir, &target, "CoSo-001-13112026", &backup, None).expect("written");
        assert!(first.had_original);
        assert_eq!(
            first.path.file_name().and_then(|n| n.to_str()),
            Some("alsa_output.pci-0000_c1_00.1.HiFi__HDMI4__sink:HDMI _ DisplayPort 4 Output.json")
        );
        assert_eq!(read(&first.path)["preset-name"], "CoSo-001-13112026");
        assert_eq!(
            read(&first.path)["device-profile"],
            "HDMI / DisplayPort 4 Output"
        );
        // Subsequent applies keep the pre-existing file as the backup.
        let second = set_autoload(&dir, &target, "CoSo-002-13112026", &backup, Some(&first))
            .expect("written");
        assert!(second.had_original);
        assert_eq!(read(&second.path)["preset-name"], "CoSo-002-13112026");
        let saved = backup
            .join("autoload-output")
            .join(first.path.file_name().expect("name"));
        assert_eq!(read(&saved)["preset-name"], "UserPreset");
        // Rolling back puts the pre-existing file back.
        restore_autoload(&second, &backup).expect("restored");
        assert_eq!(read(&first.path)["preset-name"], "UserPreset");
        // Where there was no file before, even after applying twice, rolling back removes
        // CorrectSound's file rather than backing up CoSo-004 and restoring it.
        let other = AutoloadTarget {
            device: "alsa_output.usb-speakers".into(),
            description: "USB speakers".into(),
            route: "Analog Output".into(),
        };
        let fresh = root.join("fresh-backup");
        let first = set_autoload(&dir, &other, "CoSo-004-13112026", &fresh, None).expect("written");
        assert!(!first.had_original);
        let second =
            set_autoload(&dir, &other, "CoSo-005-13112026", &fresh, Some(&first)).expect("written");
        assert!(!second.had_original);
        assert!(
            !fresh
                .join("autoload-output")
                .join(first.path.file_name().expect("name"))
                .exists(),
            "second apply must not back up CorrectSound's own autoload file"
        );
        restore_autoload(&second, &fresh).expect("restored");
        assert!(!second.path.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
