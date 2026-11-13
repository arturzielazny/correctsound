//! Process-wide list of commands that put the system back as it was.
//!
//! Every change the app makes to the system (EasyEffects bypass, mic gain, sink
//! volume) registers its undo command here first. [`run_all`] runs them in reverse
//! order and is called on normal exit, on error, from the panic hook and on SIGTERM.

use std::process::{Command, Stdio};
use std::sync::Mutex;

/// An undo step: a program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undo {
    /// Identifies the setting, so a later change does not register a second undo.
    pub key: String,
    pub argv: Vec<String>,
}

static UNDO: Mutex<Vec<Undo>> = Mutex::new(Vec::new());

/// Registers `argv` as the undo for `key`, unless `key` already has one.
pub fn register(key: &str, argv: &[&str]) {
    let mut list = UNDO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if list.iter().any(|u| u.key == key) {
        return;
    }
    list.push(Undo {
        key: key.to_owned(),
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
    });
}

/// Forgets the undo for `key` (the setting was restored some other way).
pub fn forget(key: &str) {
    let mut list = UNDO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    list.retain(|u| u.key != key);
}

/// Pending undo steps, for display.
#[must_use]
pub fn pending() -> Vec<Undo> {
    UNDO.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Runs every undo step in reverse order and clears the list. Errors are ignored:
/// this runs on the way out and must not fail.
pub fn run_all() {
    // Recover from poisoning: this runs from the panic hook too.
    let steps = std::mem::take(
        &mut *UNDO
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for step in steps.iter().rev() {
        if let Some((prog, args)) = step.argv.split_first() {
            let _ = Command::new(prog)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}
