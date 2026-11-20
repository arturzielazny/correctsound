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

/// True when there is queued work or an active paced stream waiting for [`work`] (always
/// `false` where threads run background work on their own).
#[must_use]
pub fn has_work() -> bool {
    imp::has_work()
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::Receiver;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Helper threads currently owned by in-flight [`map`] calls across the process, so nested
    /// parallel maps do not multiply thread counts beyond the machine's parallelism.
    static SPAWNED_WORKERS: AtomicUsize = AtomicUsize::new(0);

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

    struct WorkerPermit {
        count: usize,
    }

    impl Drop for WorkerPermit {
        fn drop(&mut self) {
            if self.count > 0 {
                SPAWNED_WORKERS.fetch_sub(self.count, Ordering::AcqRel);
            }
        }
    }

    pub(super) fn max_extra_workers() -> usize {
        thread::available_parallelism()
            .map(NonZeroUsize::get)
            .unwrap_or(1)
            .saturating_sub(1)
    }

    fn claim_workers(wanted: usize) -> WorkerPermit {
        let limit = max_extra_workers();
        if wanted == 0 || limit == 0 {
            return WorkerPermit { count: 0 };
        }
        let mut cur = SPAWNED_WORKERS.load(Ordering::Acquire);
        loop {
            let free = limit.saturating_sub(cur);
            let take = wanted.min(free);
            if take == 0 {
                return WorkerPermit { count: 0 };
            }
            match SPAWNED_WORKERS.compare_exchange_weak(
                cur,
                cur + take,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return WorkerPermit { count: take },
                Err(actual) => cur = actual,
            }
        }
    }

    pub fn map<T: Send, R: Send>(items: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
        let len = items.len();
        if len <= 1 {
            return items.into_iter().map(f).collect();
        }
        let permit = claim_workers(len - 1);
        if permit.count == 0 {
            return items.into_iter().map(f).collect();
        }
        let queue = Mutex::new(items.into_iter().enumerate());
        let pop = || queue.lock().expect("background map queue poisoned").next();
        let f = &f;
        let pop = &pop;
        let mut results = Vec::with_capacity(len);
        thread::scope(|scope| {
            let handles: Vec<_> = (0..permit.count)
                .map(|_| {
                    scope.spawn(move || {
                        let mut local = Vec::new();
                        while let Some((idx, item)) = pop() {
                            local.push((idx, f(item)));
                        }
                        local
                    })
                })
                .collect();
            while let Some((idx, item)) = pop() {
                results.push((idx, f(item)));
            }
            for handle in handles {
                results.extend(handle.join().expect("background work does not panic"));
            }
        });
        drop(permit);
        results.sort_unstable_by_key(|(idx, _)| *idx);
        results.into_iter().map(|(_, r)| r).collect()
    }

    pub fn work() {}

    pub fn has_work() -> bool {
        false
    }

    #[cfg(test)]
    pub(super) fn active_workers() -> usize {
        SPAWNED_WORKERS.load(Ordering::Acquire)
    }
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

    pub fn has_work() -> bool {
        JOBS.with(|j| !j.borrow().is_empty()) || STREAMS.with(|s| !s.borrow().is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_preserves_order_for_empty_single_and_many_items() {
        assert!(map(Vec::<i32>::new(), |x| x + 1).is_empty());
        assert_eq!(map(vec![7], |x| x * 3), vec![21]);
        let items: Vec<usize> = (0..64).collect();
        let out = map(items, |i| i * i + 1);
        assert_eq!(out, (0..64).map(|i| i * i + 1).collect::<Vec<_>>());
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn nested_map_caps_spawned_workers() {
        let limit = imp::max_extra_workers();
        let out = map((0..16).collect(), |outer| {
            assert!(imp::active_workers() <= limit);
            map((0..16).collect(), |inner| {
                assert!(imp::active_workers() <= limit);
                outer * 100 + inner
            })
        });
        assert_eq!(out.len(), 16);
        assert_eq!(out[15][15], 1515);
    }
}
