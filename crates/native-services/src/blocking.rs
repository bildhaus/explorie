//! Bounded worker pool behind [`crate::ServiceContext::spawn_blocking`].
//!
//! Blocking service work used to get a fresh OS thread per call. The pool
//! keeps a bounded set of reusable workers instead:
//!
//! * Threads are started on demand, up to [`MAX_BLOCKING_WORKERS`], and exit
//!   after [`WORKER_KEEP_ALIVE`] without work. Jobs beyond the bound queue in
//!   FIFO order. The bound is generous on purpose: many service jobs (file
//!   operations, archive extraction, remote-drive connects, helper
//!   conversions) block for a long time, and a small pool would let a few of
//!   them starve folder listings and previews.
//! * FIFO order matters for jobs that serialize themselves with tickets taken
//!   at submission time (video commands): a job never waits on a ticket held by
//!   a job queued behind it.
//! * A worker that blocks in [`BlockingTask::wait`] on a job that has not
//!   started yet runs that job inline instead of parking. Nested waits
//!   (a blocking job waiting on another blocking job) therefore make progress
//!   even when every worker is busy.
//! * Cancellation is opt-in. Jobs spawned with
//!   [`crate::ServiceContext::spawn_cancellable`], or marked with
//!   [`BlockingTask::cancel_on_drop`], are skipped if their handle is dropped
//!   while they are still queued, and running ones can poll their
//!   [`CancellationToken`] (or [`job_cancelled`]) to stop early. Other jobs
//!   keep the historical fire-and-forget semantics: dropping the handle only
//!   discards the result.
//!
//! Every job runs under `catch_unwind`; a panic becomes an `Internal` error
//! and the worker thread survives.

use crate::{ErrorCode, ServiceError, ServiceResult};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// Upper bound on pool threads, and therefore on concurrently running jobs.
pub(crate) const MAX_BLOCKING_WORKERS: usize = 64;
/// How long an idle worker waits for new work before its thread exits.
pub(crate) const WORKER_KEEP_ALIVE: Duration = Duration::from_secs(10);

const PANICKED: &str = "Native service worker panicked";
const STOPPED: &str = "Native service worker stopped before returning a result";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Cooperative cancellation flag shared by a [`BlockingTask`] and its job.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

thread_local! {
    static CURRENT_JOB: RefCell<Option<CancellationToken>> = const { RefCell::new(None) };
    static POOL_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Whether the blocking job running on the current thread has been
/// cancelled. Long loops inside service jobs can poll this without threading
/// a [`CancellationToken`] through every helper. Always `false` outside a job.
pub fn job_cancelled() -> bool {
    CURRENT_JOB.with(|job| {
        job.borrow()
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    })
}

type Operation<T> = Box<dyn FnOnce() -> ServiceResult<T> + Send>;

enum Slot<T> {
    Queued(Operation<T>),
    Running,
    Finished(ServiceResult<T>),
    Consumed,
}

struct JobInner<T> {
    slot: Slot<T>,
    waker: Option<Waker>,
}

struct Job<T> {
    inner: Mutex<JobInner<T>>,
    finished: Condvar,
    token: CancellationToken,
    cancel_on_drop: AtomicBool,
}

impl<T> Job<T> {
    fn finish(&self, result: ServiceResult<T>) {
        let waker = {
            let mut inner = lock(&self.inner);
            inner.slot = Slot::Finished(result);
            inner.waker.take()
        };
        self.finished.notify_all();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// Type-erased view of a queued job, as seen by the pool.
trait PoolJob: Send + Sync {
    /// Run the job (or skip it when cancelled) and publish its result.
    /// `before_publish` runs once the work is done but before any waiter is
    /// woken.
    fn run(&self, before_publish: &dyn Fn());
    /// Fail a job that no worker will ever run.
    fn abandon(&self);
}

impl<T: Send> PoolJob for Job<T> {
    fn run(&self, before_publish: &dyn Fn()) {
        let operation = {
            let mut inner = lock(&self.inner);
            match std::mem::replace(&mut inner.slot, Slot::Running) {
                Slot::Queued(operation) => operation,
                // A waiting worker already claimed and ran the job inline.
                other => {
                    inner.slot = other;
                    drop(inner);
                    before_publish();
                    return;
                }
            }
        };
        let result = if self.token.is_cancelled() {
            // Nobody is left to observe the result; drop the work unstarted.
            drop(operation);
            Err(cancelled())
        } else {
            run_guarded(&self.token, operation)
        };
        before_publish();
        self.finish(result);
    }

    fn abandon(&self) {
        let claimed = {
            let mut inner = lock(&self.inner);
            match std::mem::replace(&mut inner.slot, Slot::Running) {
                Slot::Queued(operation) => Some(operation),
                other => {
                    inner.slot = other;
                    None
                }
            }
        };
        if claimed.is_some() {
            self.finish(Err(ServiceError::new(ErrorCode::Internal, STOPPED)));
        }
    }
}

fn cancelled() -> ServiceError {
    ServiceError::new(ErrorCode::Cancelled, "Native service job was cancelled")
}

fn run_guarded<T>(token: &CancellationToken, operation: Operation<T>) -> ServiceResult<T> {
    let previous = CURRENT_JOB.with(|job| job.replace(Some(token.clone())));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
        .unwrap_or_else(|_| Err(ServiceError::new(ErrorCode::Internal, PANICKED)));
    CURRENT_JOB.with(|job| *job.borrow_mut() = previous);
    result
}

struct PoolState {
    queue: VecDeque<Arc<dyn PoolJob>>,
    workers: usize,
    idle: usize,
}

pub(crate) struct BlockingPool {
    state: Mutex<PoolState>,
    work_available: Condvar,
    max_workers: usize,
    keep_alive: Duration,
}

impl BlockingPool {
    pub(crate) fn new(max_workers: usize, keep_alive: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PoolState {
                queue: VecDeque::new(),
                workers: 0,
                idle: 0,
            }),
            work_available: Condvar::new(),
            max_workers: max_workers.max(1),
            keep_alive,
        })
    }

    /// The process-wide pool shared by every [`crate::ServiceContext`].
    pub(crate) fn global() -> &'static Arc<Self> {
        static POOL: OnceLock<Arc<BlockingPool>> = OnceLock::new();
        POOL.get_or_init(|| Self::new(MAX_BLOCKING_WORKERS, WORKER_KEEP_ALIVE))
    }

    #[cfg(test)]
    pub(crate) fn worker_count(&self) -> usize {
        lock(&self.state).workers
    }

    fn submit(self: &Arc<Self>, job: Arc<dyn PoolJob>) {
        let mut state = lock(&self.state);
        state.queue.push_back(job);
        // Idle workers each take one job once they wake; start another thread
        // only when the queue outgrows them.
        if state.queue.len() <= state.idle || state.workers >= self.max_workers {
            drop(state);
            self.work_available.notify_one();
            return;
        }
        state.workers += 1;
        let index = state.workers;
        drop(state);
        let pool = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("explorie-blocking-{index}"))
            .spawn(move || pool.work());
        if spawned.is_err() {
            let orphaned = {
                let mut state = lock(&self.state);
                state.workers -= 1;
                if state.workers == 0 {
                    state.queue.drain(..).collect::<Vec<_>>()
                } else {
                    Vec::new()
                }
            };
            // Without any worker left the queue would never drain, so fail
            // those jobs instead of leaving their callers waiting forever.
            for job in orphaned {
                job.abandon();
            }
        }
    }

    fn work(self: Arc<Self>) {
        struct Exit<'a>(&'a BlockingPool);
        impl Drop for Exit<'_> {
            fn drop(&mut self) {
                lock(&self.0.state).workers -= 1;
            }
        }

        POOL_WORKER.with(|worker| worker.set(true));
        let _exit = Exit(&self);
        let mut state = lock(&self.state);
        loop {
            if let Some(job) = state.queue.pop_front() {
                drop(state);
                // Count this worker as available before waking the caller, so
                // a caller that immediately submits follow-up work reuses it
                // instead of starting another thread.
                job.run(&|| lock(&self.state).idle += 1);
                drop(job);
                state = lock(&self.state);
                state.idle -= 1;
                continue;
            }
            state.idle += 1;
            let (next, timeout) = self
                .work_available
                .wait_timeout(state, self.keep_alive)
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
            state.idle -= 1;
            if timeout.timed_out() && state.queue.is_empty() {
                return;
            }
        }
    }
}

/// Handle to blocking service work. Await it from UI code; non-UI callers can
/// block on [`BlockingTask::wait`].
pub struct BlockingTask<T> {
    job: Arc<Job<T>>,
}

impl<T: Send + 'static> BlockingTask<T> {
    pub(crate) fn spawn<F>(pool: &Arc<BlockingPool>, operation: F) -> Self
    where
        F: FnOnce() -> ServiceResult<T> + Send + 'static,
    {
        Self::spawn_with_token(pool, CancellationToken::new(), false, Box::new(operation))
    }

    pub(crate) fn spawn_cancellable<F>(pool: &Arc<BlockingPool>, operation: F) -> Self
    where
        F: FnOnce(&CancellationToken) -> ServiceResult<T> + Send + 'static,
    {
        let token = CancellationToken::new();
        let job_token = token.clone();
        Self::spawn_with_token(pool, token, true, Box::new(move || operation(&job_token)))
    }

    fn spawn_with_token(
        pool: &Arc<BlockingPool>,
        token: CancellationToken,
        cancel_on_drop: bool,
        operation: Operation<T>,
    ) -> Self {
        let job = Arc::new(Job {
            inner: Mutex::new(JobInner {
                slot: Slot::Queued(operation),
                waker: None,
            }),
            finished: Condvar::new(),
            token,
            cancel_on_drop: AtomicBool::new(cancel_on_drop),
        });
        pool.submit(Arc::clone(&job) as Arc<dyn PoolJob>);
        Self { job }
    }
}

impl<T> BlockingTask<T> {
    /// Block a non-UI caller until the worker completes. UI adapters should
    /// await this task instead.
    ///
    /// When called from a pool worker on a job that has not started yet, the
    /// job runs inline so nested waits cannot exhaust the pool.
    pub fn wait(self) -> ServiceResult<T> {
        let mut inner = lock(&self.job.inner);
        match std::mem::replace(&mut inner.slot, Slot::Consumed) {
            Slot::Finished(result) => return result,
            Slot::Queued(operation) if POOL_WORKER.with(Cell::get) => {
                inner.slot = Slot::Running;
                drop(inner);
                let result = run_guarded(&self.job.token, operation);
                lock(&self.job.inner).slot = Slot::Consumed;
                return result;
            }
            other => inner.slot = other,
        }
        loop {
            inner = self
                .job
                .finished
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
            if let Slot::Finished(_) = inner.slot
                && let Slot::Finished(result) = std::mem::replace(&mut inner.slot, Slot::Consumed)
            {
                return result;
            }
        }
    }

    /// Declare the job safe to abandon: if this handle is dropped before the
    /// job finishes, a queued job is skipped and a running one observes
    /// cancellation through [`job_cancelled`]. Only use this for work without
    /// side effects that must complete (reads, previews, cache fills).
    pub fn cancel_on_drop(self) -> Self {
        self.job.cancel_on_drop.store(true, Ordering::Release);
        self
    }

    /// Request cooperative cancellation now. A job that has not started yet
    /// is skipped and resolves to a `Cancelled` error.
    pub fn cancel(&self) {
        self.job.token.cancel();
    }
}

impl<T> Drop for BlockingTask<T> {
    fn drop(&mut self) {
        if !self.job.cancel_on_drop.load(Ordering::Acquire) {
            return;
        }
        let mut inner = lock(&self.job.inner);
        inner.waker = None;
        if !matches!(inner.slot, Slot::Finished(_) | Slot::Consumed) {
            self.job.token.cancel();
        }
    }
}

impl<T> Unpin for BlockingTask<T> {}

impl<T> Future for BlockingTask<T> {
    type Output = ServiceResult<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = lock(&self.job.inner);
        match std::mem::replace(&mut inner.slot, Slot::Consumed) {
            Slot::Finished(result) => Poll::Ready(result),
            Slot::Consumed => Poll::Ready(Err(ServiceError::new(ErrorCode::Internal, STOPPED))),
            other => {
                inner.slot = other;
                if !inner
                    .waker
                    .as_ref()
                    .is_some_and(|waker| waker.will_wake(context.waker()))
                {
                    inner.waker = Some(context.waker().clone());
                }
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Barrier, mpsc};
    use std::time::Instant;

    fn wait_for(condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "condition not reached in time");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn sequential_jobs_reuse_pool_threads() {
        let pool = BlockingPool::new(4, Duration::from_secs(5));
        let threads = (0..64)
            .map(|_| {
                BlockingTask::spawn(&pool, || Ok(std::thread::current().id()))
                    .wait()
                    .unwrap()
            })
            .collect::<HashSet<_>>();
        assert_eq!(threads.len(), 1, "one worker serves sequential jobs");
        assert_eq!(pool.worker_count(), 1);
    }

    #[test]
    fn concurrency_never_exceeds_the_worker_bound() {
        let pool = BlockingPool::new(3, Duration::from_secs(5));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let tasks = (0..24)
            .map(|index| {
                let running = Arc::clone(&running);
                let peak = Arc::clone(&peak);
                BlockingTask::spawn(&pool, move || {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(5));
                    running.fetch_sub(1, Ordering::SeqCst);
                    Ok(index)
                })
            })
            .collect::<Vec<_>>();
        let results = tasks
            .into_iter()
            .map(|task| task.wait().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results, (0..24).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 3);
        assert!(pool.worker_count() <= 3);
    }

    #[test]
    fn nested_waits_complete_when_every_worker_is_busy() {
        let pool = BlockingPool::new(2, Duration::from_secs(5));
        let barrier = Arc::new(Barrier::new(2));
        let outer = (0..2)
            .map(|index| {
                let pool = Arc::clone(&pool);
                let barrier = Arc::clone(&barrier);
                BlockingTask::spawn(&pool.clone(), move || {
                    // Both workers are now occupied; the inner job can only
                    // run if the waiting worker picks it up itself.
                    barrier.wait();
                    BlockingTask::spawn(&pool, move || Ok(index * 10)).wait()
                })
            })
            .collect::<Vec<_>>();
        let results = outer
            .into_iter()
            .map(|task| task.wait().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results, vec![0, 10]);
    }

    #[test]
    fn dropped_cancellable_jobs_are_skipped_or_stopped() {
        let pool = BlockingPool::new(1, Duration::from_secs(5));
        let (release, released) = mpsc::channel::<()>();
        let blocker = BlockingTask::spawn(&pool, move || {
            released.recv().ok();
            Ok(())
        });

        // Queued behind the blocker, then abandoned: it must never start.
        let started = Arc::new(AtomicBool::new(false));
        let queued = {
            let started = Arc::clone(&started);
            BlockingTask::spawn_cancellable(&pool, move |_| {
                started.store(true, Ordering::SeqCst);
                Ok(())
            })
        };
        drop(queued);

        // Fire-and-forget jobs still run to completion after their handle
        // is dropped.
        let completed = Arc::new(AtomicBool::new(false));
        drop({
            let completed = Arc::clone(&completed);
            BlockingTask::spawn(&pool, move || {
                completed.store(true, Ordering::SeqCst);
                Ok(())
            })
        });

        release.send(()).unwrap();
        blocker.wait().unwrap();
        wait_for(|| completed.load(Ordering::SeqCst));
        assert!(!started.load(Ordering::SeqCst));

        // A running job sees the cancellation through its token and through
        // the thread-local accessor.
        let (entered_tx, entered) = mpsc::channel();
        let observed = Arc::new(AtomicBool::new(false));
        let running = {
            let observed = Arc::clone(&observed);
            BlockingTask::spawn_cancellable(&pool, move |token| {
                entered_tx.send(()).unwrap();
                while !token.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                observed.store(job_cancelled(), Ordering::SeqCst);
                Ok(())
            })
        };
        entered.recv().unwrap();
        drop(running);
        wait_for(|| observed.load(Ordering::SeqCst));
        assert!(!job_cancelled(), "no job runs on the test thread");
    }

    #[test]
    fn panics_become_errors_and_workers_survive() {
        let pool = BlockingPool::new(1, Duration::from_secs(5));
        let error = BlockingTask::spawn(&pool, || -> ServiceResult<()> { panic!("boom") })
            .wait()
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(error.message, PANICKED);
        assert_eq!(BlockingTask::spawn(&pool, || Ok(7)).wait().unwrap(), 7);
        assert_eq!(pool.worker_count(), 1);
    }

    #[test]
    fn idle_workers_exit_after_the_keep_alive() {
        let pool = BlockingPool::new(4, Duration::from_millis(20));
        BlockingTask::spawn(&pool, || Ok(())).wait().unwrap();
        wait_for(|| pool.worker_count() == 0);
        assert_eq!(BlockingTask::spawn(&pool, || Ok(1)).wait().unwrap(), 1);
    }
}
