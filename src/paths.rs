//! Where the app keeps its files: the XDG locations, on every platform (under the home
//! directory when the XDG variables are unset), so a session's layout is the same
//! everywhere.

use std::path::PathBuf;
use std::sync::OnceLock;

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Redirects every app and platform path under `root` (for tests). First call wins.
pub fn set_root(root: PathBuf) {
    let _ = ROOT.set(root);
}

/// True when [`set_root`] has redirected paths into a test root.
#[must_use]
pub fn has_root() -> bool {
    ROOT.get().is_some()
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
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("."))
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

/// `$XDG_DATA_HOME`, falling back to `~/.local/share`.
#[must_use]
pub fn xdg_data_home() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_CONFIG_HOME`, falling back to `~/.config`.
#[must_use]
pub fn xdg_config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME/correctsound`.
#[must_use]
pub fn data_dir() -> PathBuf {
    xdg_data_home().join("correctsound")
}

/// `$XDG_CONFIG_HOME/correctsound`.
#[must_use]
pub fn config_dir() -> PathBuf {
    xdg_config_home().join("correctsound")
}
