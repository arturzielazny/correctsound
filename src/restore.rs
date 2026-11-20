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
/// Held while undo steps are running, so a second caller (e.g. the signal watchdog) waits
/// for in-flight restorations to finish before returning.
static RUNNING: Mutex<()> = Mutex::new(());

/// The list, recovered from poisoning: this runs from the panic hook too.
fn list() -> MutexGuard<'static, Vec<Undo>> {
    UNDO.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Registers `action` as the undo for `key`, unless `key` already has one; `what` says in
/// a sentence what it puts back ("the mic's input volume is 0.42 again"), for the summary
/// on exit. Returns `true` when `key` was newly registered (so the caller can [`forget`] it
/// if the change itself fails). The action runs on the way out, so it must not panic;
/// errors should be ignored.
pub fn register(key: &str, what: &str, action: impl FnOnce() + Send + 'static) -> bool {
    let mut list = list();
    if list.iter().any(|u| u.key == key) {
        return false;
    }
    list.push(Undo {
        key: key.to_owned(),
        what: what.to_owned(),
        action: Box::new(action),
    });
    true
}

/// Registers running `argv` (a program and its arguments) as the undo for `key`. Returns
/// `true` when `key` was newly registered.
pub fn register_command(key: &str, what: &str, argv: &[&str]) -> bool {
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
    })
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
///
/// Concurrent calls wait until an in-flight restoration finishes, so a watchdog thread
/// cannot exit the process while the main thread is halfway through restoring the system.
pub fn run_all() {
    let _running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    let steps = std::mem::take(&mut *list());
    for step in steps.into_iter().rev() {
        (step.action)();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn concurrent_run_all_waits_for_in_flight_undo() {
        let (started_tx, started_rx) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let finished2 = Arc::clone(&finished);
        register("test-wait", "test step", move || {
            let _ = started_tx.send(());
            std::thread::sleep(Duration::from_millis(80));
            finished2.store(true, Ordering::Release);
        });
        let worker = std::thread::spawn(run_all);
        started_rx.recv().expect("undo step started");
        // A second concurrent call must block until the first has finished its undo step.
        run_all();
        assert!(
            finished.load(Ordering::Acquire),
            "second run_all returned before in-flight undo finished"
        );
        worker.join().expect("worker joined");
    }

    #[test]
    fn register_reports_whether_key_was_added_and_forget_removes_it() {
        let key = "test-register-reports-key";
        forget(key);
        assert!(register(key, "first undo", || {}));
        assert!(!register(key, "duplicate undo", || {}));
        assert!(pending().iter().any(|w| w == "first undo"));
        forget(key);
        assert!(!pending().iter().any(|w| w == "first undo"));
        assert!(register(key, "re-added undo", || {}));
        forget(key);
    }
}
