//! XDG locations used by the app.

use std::path::PathBuf;
use std::sync::OnceLock;

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Redirects every app and EasyEffects path under `root` (for tests). First call wins.
pub fn set_root(root: PathBuf) {
    let _ = ROOT.set(root);
}

/// `path` for showing: the home directory (or the test root standing in for it) as `~`.
#[must_use]
pub fn tilde(path: &std::path::Path) -> String {
    let home = ROOT.get().cloned().unwrap_or_else(home);
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    if let Some(root) = ROOT.get() {
        return root.join(fallback);
    }
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

/// `$XDG_DATA_HOME/correctsound`.
#[must_use]
pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("correctsound")
}

/// `$XDG_CONFIG_HOME/correctsound`.
#[must_use]
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("correctsound")
}

/// `$XDG_CONFIG_HOME/easyeffects`.
#[must_use]
pub fn easyeffects_config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("easyeffects")
}

/// `$XDG_DATA_HOME/easyeffects`.
#[must_use]
pub fn easyeffects_data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("easyeffects")
}
