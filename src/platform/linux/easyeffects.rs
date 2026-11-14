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
/// (once per session). Returns the file written.
fn set_autoload(
    dir: &Path,
    target: &AutoloadTarget,
    preset: &str,
    backup_dir: &Path,
) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = autoload_path(dir, target);
    if path.exists() {
        let saved = backup_dir.join("autoload-output");
        let copy = saved.join(path.file_name().unwrap_or_default());
        if !copy.exists() {
            fs::create_dir_all(&saved)?;
            fs::copy(&path, &copy).with_context(|| format!("backing up {}", path.display()))?;
        }
    }
    let json = serde_json::json!({
        "device": target.device,
        "device-description": target.description,
        "device-profile": target.route,
        "preset-name": preset,
    });
    fs::write(&path, serde_json::to_string_pretty(&json)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Backs up the live settings, writes the preset, loads it and turns bypass off. With an
/// `autoload` target the preset also becomes the one EasyEffects loads for that output, so
/// it survives EasyEffects restarting without saving its state.
///
/// # Errors
///
/// Fails if the backup or preset cannot be written, or EasyEffects rejects the preset.
pub fn apply(
    eq: &EqSet,
    backup_parent: &Path,
    autoload: Option<&AutoloadTarget>,
) -> Result<Applied> {
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
    let autoload = match autoload {
        Some(target) => {
            let dir = easyeffects_data_dir().join("autoload/output");
            set_autoload(&dir, target, &name, &backup_dir)?;
            Some(target.route.clone())
        }
        None => None,
    };
    Ok(Applied {
        preset_name: name,
        preset_path: path,
        backup_dir,
        autoload,
    })
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
        let path = set_autoload(&dir, &target, "CoSo-001-13112026", &backup).expect("written");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("alsa_output.pci-0000_c1_00.1.HiFi__HDMI4__sink:HDMI _ DisplayPort 4 Output.json")
        );
        let read = |p: &Path| -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(p).expect("read")).expect("json")
        };
        assert_eq!(read(&path)["preset-name"], "CoSo-001-13112026");
        assert_eq!(read(&path)["device-profile"], "HDMI / DisplayPort 4 Output");
        // A second apply replaces it; the first (pre-existing) file is what the backup keeps.
        set_autoload(&dir, &target, "CoSo-002-13112026", &backup).expect("written");
        set_autoload(&dir, &target, "CoSo-003-13112026", &backup).expect("written");
        assert_eq!(read(&path)["preset-name"], "CoSo-003-13112026");
        let saved = backup
            .join("autoload-output")
            .join(path.file_name().expect("name"));
        assert_eq!(read(&saved)["preset-name"], "CoSo-001-13112026");
        let _ = fs::remove_dir_all(&root);
    }
}
