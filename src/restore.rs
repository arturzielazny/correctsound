//! Process-wide list of steps that put the system back as it was.
//!
//! Every change the app makes to the system (EQ host bypass, mic gain, output volume)
//! registers its undo step here first. [`run_all`] runs them in reverse order and is
//! called on normal exit, on error, from the panic hook and on signals. A step is a
//! closure, so a platform can undo through an API as well as with a command
//! ([`register_command`]).

use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long an undo command may run before being killed.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_STEP: Duration = Duration::from_millis(20);

/// An undo step.
struct Undo {
    /// Identifies the setting, so a later change does not register a second undo.
    key: String,
    /// What putting it back does, as a sentence for the user: "EasyEffects is on again".
    what: String,
    action: Box<dyn FnOnce() + Send>,
}

static UNDO: Mutex<Vec<Undo>> = Mutex::new(Vec::new());
/// True while undo steps are running; another caller waits on the condvar until restoration
/// completes (or until its deadline in [`run_all_within`]).
static RUNNING: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

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
/// `true` when `key` was newly registered. Each command is bounded by [`COMMAND_TIMEOUT`]
/// so a hung helper cannot block program exit forever.
pub fn register_command(key: &str, what: &str, argv: &[&str]) -> bool {
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    register(key, what, move || {
        if let Some((prog, args)) = argv.split_first() {
            run_command_with_timeout(prog, args, COMMAND_TIMEOUT);
        }
    })
}

fn run_command_with_timeout(prog: &str, args: &[String], timeout: Duration) {
    let Ok(mut child) = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {}
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        std::thread::sleep(POLL_STEP);
    }
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

struct RunningGuard;

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut running = RUNNING.0.lock().unwrap_or_else(PoisonError::into_inner);
        *running = false;
        RUNNING.1.notify_all();
    }
}

fn run_all_inner(wait_limit: Option<Duration>) -> bool {
    let started = Instant::now();
    let mut running = RUNNING.0.lock().unwrap_or_else(PoisonError::into_inner);
    while *running {
        match wait_limit {
            None => {
                running = RUNNING
                    .1
                    .wait(running)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            Some(limit) => {
                let Some(remaining) = limit.checked_sub(started.elapsed()) else {
                    return false;
                };
                if remaining.is_zero() {
                    return false;
                }
                let (guard, res) = RUNNING
                    .1
                    .wait_timeout(running, remaining)
                    .unwrap_or_else(PoisonError::into_inner);
                running = guard;
                if res.timed_out() && *running {
                    return false;
                }
            }
        }
    }
    *running = true;
    drop(running);
    let _guard = RunningGuard;

    let steps = std::mem::take(&mut *list());
    for step in steps.into_iter().rev() {
        (step.action)();
    }
    true
}

/// Runs every undo step in reverse order and clears the list.
///
/// Concurrent calls wait until an in-flight restoration finishes.
pub fn run_all() {
    let _ = run_all_inner(None);
}

/// Like [`run_all`], but waits at most `timeout` if another thread is already running undo
/// steps. Returns `true` if restoration finished before the deadline, `false` if waiting
/// timed out (so a signal watchdog still exits rather than hanging forever).
pub fn run_all_within(timeout: Duration) -> bool {
    run_all_inner(Some(timeout))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// Serialises tests in this module because they share the process-wide `UNDO` list.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn concurrent_run_all_waits_for_in_flight_undo() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        run_all();
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
    fn run_all_within_returns_false_when_in_flight_undo_exceeds_deadline() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        run_all();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        register("test-deadline", "slow step", move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        });
        let worker = std::thread::spawn(run_all);
        started_rx.recv().expect("undo step started");
        let start = Instant::now();
        let completed = run_all_within(Duration::from_millis(60));
        assert!(
            !completed,
            "expected wait to time out while undo is blocked"
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "run_all_within blocked past its deadline"
        );
        let _ = release_tx.send(());
        worker.join().expect("worker joined");
    }

    #[cfg(unix)]
    #[test]
    fn run_command_with_timeout_kills_hung_command() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let start = Instant::now();
        run_command_with_timeout(
            "sh",
            &["-c".to_owned(), "sleep 5".to_owned()],
            Duration::from_millis(100),
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "hung undo command was not killed promptly"
        );
    }

    #[test]
    fn register_reports_whether_key_was_added_and_forget_removes_it() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
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
