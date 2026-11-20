//! Process-wide list of steps that put the system back as it was.
//!
//! Every change the app makes to the system (EQ host bypass, mic gain, output volume)
//! registers its undo step here first. [`run_all`] runs them in reverse order and is
//! called on normal exit, on error, from the panic hook and on signals. A step is a
//! closure, so a platform can undo through an API as well as with a command
//! ([`register_command`]).

use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// An undo step.
struct Undo {
    /// Identifies the setting, so a later change does not register a second undo.
    key: String,
    /// What putting it back does, as a sentence for the user: "EasyEffects is on again".
    what: String,
    action: Box<dyn FnOnce() + Send>,
}

static UNDO: Mutex<Vec<Undo>> = Mutex::new(Vec::new());

/// The list, recovered from poisoning: this runs from the panic hook too.
fn list() -> MutexGuard<'static, Vec<Undo>> {
    UNDO.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Registers `action` as the undo for `key`, unless `key` already has one; `what` says in
/// a sentence what it puts back ("the mic's input volume is 0.42 again"), for the summary
/// on exit. The action runs on the way out, so it must not panic; errors should be ignored.
pub fn register(key: &str, what: &str, action: impl FnOnce() + Send + 'static) {
    let mut list = list();
    if list.iter().any(|u| u.key == key) {
        return;
    }
    list.push(Undo {
        key: key.to_owned(),
        what: what.to_owned(),
        action: Box::new(action),
    });
}

/// Registers running `argv` (a program and its arguments) as the undo for `key`.
pub fn register_command(key: &str, what: &str, argv: &[&str]) {
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    register(key, what, move || {
        if let Some((prog, args)) = argv.split_first() {
            let _ = Command::new(prog)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    });
}

/// Forgets the undo for `key` (the setting was restored some other way).
pub fn forget(key: &str) {
    list().retain(|u| u.key != key);
}

/// What the pending undo steps put back, in the order they were registered, for display.
#[must_use]
pub fn pending() -> Vec<String> {
    list().iter().map(|u| u.what.clone()).collect()
}

/// Runs every undo step in reverse order and clears the list.
pub fn run_all() {
    let steps = std::mem::take(&mut *list());
    for step in steps.into_iter().rev() {
        (step.action)();
    }
}
