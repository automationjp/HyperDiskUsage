//! Work-stealing scheduler shared by all platform backends.
//!
//! Design:
//! - Each worker owns a LIFO deque. Directories discovered while enumerating are
//!   pushed there (depth-first, keeps the frontier small and cache-friendly).
//! - Other workers steal from the *opposite* end, i.e. the shallowest (largest)
//!   subtrees, which gives good load balance for free.
//! - Resume jobs (large directories split by `dir_yield_every`) go through a
//!   high-priority injector so a split directory is finished promptly.
//! - Termination is tracked with a `pending` counter (queued + in-flight jobs).
//!   A worker only exits when `pending == 0`; while work is in flight elsewhere
//!   it spins briefly, then parks with a bounded timeout.

use std::{
    path::PathBuf,
    sync::{
        atomic::{fence, AtomicUsize, Ordering},
        Condvar, Mutex,
    },
    time::Duration,
};

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use crossbeam_utils::Backoff;

/// Upper bound for a parked worker before it re-checks the queues on its own.
const PARK_TIMEOUT: Duration = Duration::from_millis(1);

#[derive(Clone, Debug)]
pub struct Job {
    pub dir: PathBuf,
    pub depth: u32,
    pub resume: Option<u64>,
}

pub struct Scheduler {
    /// Resume jobs and the root: always taken before anything else.
    high: Injector<Job>,
    stealers: Vec<Stealer<Job>>,
    /// Jobs queued or currently being processed.
    pending: AtomicUsize,
    /// Number of workers currently parked on `cv`.
    idle: AtomicUsize,
    park: Mutex<()>,
    cv: Condvar,
}

impl Scheduler {
    pub fn new(workers: &[Worker<Job>]) -> Self {
        Self {
            high: Injector::new(),
            stealers: workers.iter().map(|w| w.stealer()).collect(),
            pending: AtomicUsize::new(0),
            idle: AtomicUsize::new(0),
            park: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    /// Create `n` LIFO worker deques.
    pub fn make_workers(n: usize) -> Vec<Worker<Job>> {
        (0..n).map(|_| Worker::new_lifo()).collect()
    }

    #[inline]
    pub fn push_high(&self, job: Job) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.high.push(job);
        self.wake_one();
    }

    /// Push onto the calling worker's own deque (cheapest path).
    #[inline]
    pub fn push_local(&self, local: &Worker<Job>, job: Job) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        local.push(job);
        self.wake_one();
    }

    /// Must be called exactly once after a job obtained via `find_job` is done.
    #[inline]
    pub fn finish_job(&self) {
        if self.pending.fetch_sub(1, Ordering::SeqCst) == 1 {
            // Last job finished: release every parked worker so they can exit.
            let _g = self.park.lock().unwrap_or_else(|e| e.into_inner());
            self.cv.notify_all();
        }
    }

    #[inline]
    pub fn is_finished(&self) -> bool {
        self.pending.load(Ordering::SeqCst) == 0
    }

    #[inline]
    fn wake_one(&self) {
        fence(Ordering::SeqCst);
        if self.idle.load(Ordering::SeqCst) > 0 {
            let _g = self.park.lock().unwrap_or_else(|e| e.into_inner());
            self.cv.notify_one();
        }
    }

    /// Try to obtain a job: the high-priority queue when it holds anything, then
    /// the own deque, then steal from peers (round-robin starting at `*next`).
    ///
    /// High priority is checked first so a split directory is finished promptly
    /// instead of waiting behind a worker's whole local backlog. That queue is
    /// almost always empty and `is_empty` is a relaxed load, so the common path
    /// still effectively starts at the local deque.
    pub fn find_job(&self, local: &Worker<Job>, next: &mut usize) -> Option<Job> {
        if !self.high.is_empty() {
            if let Some(j) = steal_retrying(|| self.high.steal_batch_and_pop(local)) {
                return Some(j);
            }
        }
        if let Some(j) = local.pop() {
            return Some(j);
        }
        if let Some(j) = steal_retrying(|| self.high.steal_batch_and_pop(local)) {
            return Some(j);
        }
        let len = self.stealers.len();
        for k in 0..len {
            let idx = (*next + k) % len;
            if let Some(j) = steal_retrying(|| self.stealers[idx].steal_batch_and_pop(local)) {
                *next = (idx + 1) % len;
                return Some(j);
            }
        }
        if len > 0 {
            *next = (*next + 1) % len;
        }
        None
    }

    fn has_visible_work(&self) -> bool {
        !self.high.is_empty() || self.stealers.iter().any(|s| !s.is_empty())
    }

    /// Called when `find_job` returned `None`. Returns `false` when the scan is
    /// complete and the worker should exit; `true` when it should retry.
    pub fn wait_for_work(&self, backoff: &Backoff) -> bool {
        if self.is_finished() {
            return false;
        }
        if !backoff.is_completed() {
            backoff.snooze();
            return true;
        }
        // Park. Register as idle *before* re-checking so a concurrent push
        // (which checks `idle` after its push) cannot be missed.
        self.idle.fetch_add(1, Ordering::SeqCst);
        if self.is_finished() || self.has_visible_work() {
            self.idle.fetch_sub(1, Ordering::SeqCst);
            return !self.is_finished();
        }
        {
            let g = self.park.lock().unwrap_or_else(|e| e.into_inner());
            let _ = self.cv.wait_timeout(g, PARK_TIMEOUT);
        }
        self.idle.fetch_sub(1, Ordering::SeqCst);
        !self.is_finished()
    }
}

/// Retry a steal operation while it reports transient contention.
#[inline]
fn steal_retrying(mut op: impl FnMut() -> Steal<Job>) -> Option<Job> {
    loop {
        match op() {
            Steal::Success(j) => return Some(j),
            Steal::Empty => return None,
            Steal::Retry => std::hint::spin_loop(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(p: &str, depth: u32) -> Job {
        Job {
            dir: PathBuf::from(p),
            depth,
            resume: None,
        }
    }

    #[test]
    fn finishes_only_after_all_jobs_done() {
        let workers = Scheduler::make_workers(2);
        let sched = Scheduler::new(&workers);
        sched.push_high(job("a", 0));
        assert!(!sched.is_finished());
        let mut next = 0;
        let j = sched.find_job(&workers[0], &mut next).unwrap();
        assert_eq!(j.dir, PathBuf::from("a"));
        // Discover a child while "processing" the parent
        sched.push_local(&workers[0], job("a/b", 1));
        sched.finish_job();
        assert!(!sched.is_finished());
        // The other worker steals the child
        let mut next1 = 0;
        let j2 = sched.find_job(&workers[1], &mut next1).unwrap();
        assert_eq!(j2.dir, PathBuf::from("a/b"));
        sched.finish_job();
        assert!(sched.is_finished());
        assert!(!sched.wait_for_work(&Backoff::new()));
    }

    #[test]
    fn idle_worker_keeps_waiting_while_a_job_is_in_flight() {
        let workers = Scheduler::make_workers(1);
        let sched = Scheduler::new(&workers);
        sched.push_high(job("a", 0));
        let mut next = 0;
        let _j = sched.find_job(&workers[0], &mut next).unwrap();
        // Queue is empty but a job is in flight: must not report finished.
        let backoff = Backoff::new();
        for _ in 0..16 {
            assert!(sched.wait_for_work(&backoff));
        }
        sched.finish_job();
        assert!(!sched.wait_for_work(&backoff));
    }

    #[test]
    fn local_deque_is_lifo_and_stealers_take_oldest() {
        let workers = Scheduler::make_workers(2);
        let sched = Scheduler::new(&workers);
        sched.push_local(&workers[0], job("old", 1));
        sched.push_local(&workers[0], job("new", 1));
        let mut n0 = 0;
        let mut n1 = 0;
        let stolen = sched.find_job(&workers[1], &mut n1).unwrap();
        assert_eq!(stolen.dir, PathBuf::from("old"));
        let own = sched.find_job(&workers[0], &mut n0).unwrap();
        assert_eq!(own.dir, PathBuf::from("new"));
    }
}
