//! Work that runs beside the screen, so keys are answered at once: on threads of its own in
//! the installed program. A web page has no threads, so there the work waits in a queue and
//! its loop does it between frames, with [`work`].

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// Starts `job`; its result arrives on the receiver.
pub fn spawn<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    imp::spawn(Box::new(move || {
        let _ = tx.send(job());
    }));
    rx
}

/// Calls `next` once every `every`, in step with the clock, and sends what it makes, until
/// `stop` is set or the receiver is dropped: a stream of audio, played and recorded as it
/// would be in real time.
pub fn paced<T: Send + 'static>(
    every: Duration,
    stop: Arc<AtomicBool>,
    mut next: impl FnMut() -> T + Send + 'static,
) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    imp::paced(every, stop, Box::new(move || tx.send(next()).is_ok()));
    rx
}

/// The result of a job started with [`spawn`], waiting for it if it is not ready; `None`
/// if the job ended without one.
pub fn wait<T>(job: &Receiver<T>) -> Option<T> {
    imp::wait(job)
}

/// `f` of every item, in order: worked out side by side where there are threads.
pub fn map<T: Send, R: Send>(items: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    imp::map(items, f)
}

/// Does the work due: in a web page, every stream's due part and the queued jobs. Nothing to
/// do elsewhere, where threads do it.
pub fn work() {
    imp::work();
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Receiver;
    use std::thread;
    use std::time::{Duration, Instant};

    pub fn spawn(job: Box<dyn FnOnce() + Send>) {
        thread::spawn(job);
    }

    pub fn paced(
        every: Duration,
        stop: Arc<AtomicBool>,
        mut step: Box<dyn FnMut() -> bool + Send>,
    ) {
        thread::spawn(move || {
            let start = Instant::now();
            let mut made = 0u32;
            while !stop.load(Ordering::Acquire) {
                if !step() {
                    break;
                }
                made += 1;
                if let Some(wait) = (every * made).checked_sub(start.elapsed()) {
                    thread::sleep(wait);
                }
            }
        });
    }

    pub fn wait<T>(job: &Receiver<T>) -> Option<T> {
        job.recv().ok()
    }

    pub fn map<T: Send, R: Send>(items: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
        let f = &f;
        thread::scope(|scope| {
            let handles: Vec<_> = items
                .into_iter()
                .map(|item| scope.spawn(move || f(item)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("background work does not panic"))
                .collect()
        })
    }

    pub fn work() {}
}

#[cfg(target_family = "wasm")]
mod imp {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use web_time::Instant;

    /// The most a stream catches up in one go, after the page was hidden or busy: beyond it,
    /// the time is skipped, as a real stream would drop it.
    const MOST_BEHIND: Duration = Duration::from_millis(250);

    struct Stream {
        every: Duration,
        start: Instant,
        made: u32,
        stop: Arc<AtomicBool>,
        step: Box<dyn FnMut() -> bool>,
    }

    thread_local! {
        static JOBS: RefCell<VecDeque<Box<dyn FnOnce()>>> = RefCell::new(VecDeque::new());
        static STREAMS: RefCell<Vec<Stream>> = const { RefCell::new(Vec::new()) };
    }

    pub fn spawn(job: Box<dyn FnOnce() + Send>) {
        JOBS.with(|jobs| jobs.borrow_mut().push_back(job));
    }

    pub fn paced(every: Duration, stop: Arc<AtomicBool>, step: Box<dyn FnMut() -> bool + Send>) {
        STREAMS.with(|s| {
            s.borrow_mut().push(Stream {
                every,
                start: Instant::now(),
                made: 0,
                stop,
                step,
            });
        });
    }

    pub fn wait<T>(job: &Receiver<T>) -> Option<T> {
        // Nothing else can do the job: do every queued one now.
        run_jobs();
        job.try_recv().ok()
    }

    pub fn map<T, R>(items: Vec<T>, f: impl Fn(T) -> R) -> Vec<R> {
        items.into_iter().map(f).collect()
    }

    pub fn work() {
        run_streams();
        run_jobs();
    }

    fn run_jobs() {
        // A job may queue another; each is taken out before it runs.
        while let Some(job) = JOBS.with(|jobs| jobs.borrow_mut().pop_front()) {
            job();
        }
    }

    fn run_streams() {
        let mut streams = STREAMS.with(|s| std::mem::take(&mut *s.borrow_mut()));
        let now = Instant::now();
        streams.retain_mut(|s| {
            let behind = now.duration_since(s.start);
            if behind.saturating_sub(s.every * s.made) > MOST_BEHIND {
                s.start = now - s.every * s.made;
            }
            while !s.stop.load(Ordering::Acquire) && s.every * s.made <= now.duration_since(s.start)
            {
                if !(s.step)() {
                    return false;
                }
                s.made += 1;
            }
            !s.stop.load(Ordering::Acquire)
        });
        // Streams started while these ran come after them.
        STREAMS.with(|s| {
            let mut s = s.borrow_mut();
            streams.append(&mut s);
            *s = streams;
        });
    }
}
