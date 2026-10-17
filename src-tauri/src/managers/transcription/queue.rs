//! A background transcription queue and its worker thread: jobs (meetings,
//! imported files) wait in a FIFO and are run one at a time. Shared by the
//! meeting and the file transcribers, which decide what running a job means.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use super::TranscriptionManager;

/// How often [`JobQueue::wait_until_not_current`] checks again.
const CURRENT_POLL: Duration = Duration::from_millis(50);

#[derive(Default)]
struct Queue {
    /// Ids of the jobs waiting, oldest first.
    pending: VecDeque<String>,
    /// The job being run.
    current: Option<Current>,
}

struct Current {
    id: String,
    cancelled: Arc<AtomicBool>,
    /// Past the point of no return: its result is being saved, a cancel
    /// comes too late.
    committed: bool,
}

/// What a cancel did.
#[derive(Debug, PartialEq, Eq)]
enum CancelOutcome {
    /// Taken out of the queue.
    Dequeued,
    /// Being run: flagged to stop.
    Stopping,
    /// Its result is already being saved.
    TooLate,
    NotQueued,
}

impl Queue {
    fn contains(&self, id: &str) -> bool {
        self.pending.iter().any(|p| p == id) || self.current.as_ref().is_some_and(|c| c.id == id)
    }

    /// Takes the next job, if any, as the current one.
    fn start_next(&mut self) -> Option<(String, Arc<AtomicBool>)> {
        let id = self.pending.pop_front()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        self.current = Some(Current {
            id: id.clone(),
            cancelled: Arc::clone(&cancelled),
            committed: false,
        });
        Some((id, cancelled))
    }

    fn cancel(&mut self, id: &str) -> CancelOutcome {
        if let Some(at) = self.pending.iter().position(|p| p == id) {
            self.pending.remove(at);
            return CancelOutcome::Dequeued;
        }
        match self.current.as_mut().filter(|c| c.id == id) {
            Some(current) if current.committed => CancelOutcome::TooLate,
            Some(current) => {
                current.cancelled.store(true, Ordering::Release);
                CancelOutcome::Stopping
            }
            None => CancelOutcome::NotQueued,
        }
    }

    /// The point of no return for the current job's result: false if it was
    /// cancelled first, else no cancel is accepted from now on.
    fn commit(&mut self) -> bool {
        match self.current.as_mut() {
            Some(current) if !current.cancelled.load(Ordering::Acquire) => {
                current.committed = true;
                true
            }
            _ => false,
        }
    }
}

struct Shared {
    app: AppHandle,
    queue: Mutex<Queue>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The job being run, as its runner sees it.
pub(super) struct RunningJob<'a> {
    shared: &'a Shared,
    pub id: &'a str,
    pub cancelled: &'a AtomicBool,
}

impl RunningJob<'_> {
    /// Whether the user cancelled the job.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// The point of no return: false if the job was cancelled first, else
    /// no cancel is accepted from now on and its result can be saved.
    pub fn commit(&self) -> bool {
        self.shared.lock().commit()
    }
}

/// A FIFO of job ids and the thread running them, one at a time.
#[derive(Clone)]
pub(super) struct JobQueue {
    shared: Arc<Shared>,
}

impl JobQueue {
    /// Starts the worker thread `name`; `run` is called for each job.
    pub fn spawn(
        app: &AppHandle,
        name: &str,
        run: impl Fn(&RunningJob<'_>) + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            app: app.clone(),
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name(name.into())
            .spawn(move || loop {
                let (id, cancelled) = {
                    let mut queue = worker.lock();
                    loop {
                        match queue.start_next() {
                            Some(next) => break next,
                            None => {
                                queue = worker.wake.wait(queue).unwrap_or_else(|e| e.into_inner())
                            }
                        }
                    }
                };
                run(&RunningJob {
                    shared: &worker,
                    id: &id,
                    cancelled: &cancelled,
                });
                worker.lock().current = None;
            })
            .unwrap_or_else(|e| panic!("Failed to spawn the {name} thread: {e}"));
        Self { shared }
    }

    /// Queues a job, unless it is already queued or being run.
    pub fn enqueue(&self, id: &str) {
        let mut queue = self.shared.lock();
        if queue.contains(id) {
            return;
        }
        queue.pending.push_back(id.to_string());
        self.shared.wake.notify_one();
    }

    /// Takes a job out of the queue, or stops it if it is being run. Returns
    /// false if it was not queued, or its result is already being saved.
    pub fn cancel(&self, id: &str) -> bool {
        let outcome = self.shared.lock().cancel(id);
        match outcome {
            CancelOutcome::Dequeued => true,
            CancelOutcome::Stopping => {
                // Free the engine now, unless that would also stop a
                // dictation: then the job stops once its chunk is done.
                if let Some(tm) = self.shared.app.try_state::<Arc<TranscriptionManager>>() {
                    if !tm.dictation_busy() {
                        tm.cancel_transcription();
                    }
                }
                true
            }
            CancelOutcome::TooLate | CancelOutcome::NotQueued => false,
        }
    }

    /// Whether `id` is the job being run.
    pub fn is_current(&self, id: &str) -> bool {
        self.shared
            .lock()
            .current
            .as_ref()
            .is_some_and(|c| c.id == id)
    }

    /// Waits until `id` is no longer the job being run, at most `timeout`.
    /// Returns whether it finished.
    pub fn wait_until_not_current(&self, id: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.is_current(id) {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(CURRENT_POLL);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(ids: &[&str]) -> Queue {
        Queue {
            pending: ids.iter().map(|id| id.to_string()).collect(),
            current: None,
        }
    }

    #[test]
    fn a_queued_job_is_dequeued() {
        let mut q = queue(&["a", "b"]);
        assert_eq!(q.cancel("b"), CancelOutcome::Dequeued);
        assert!(!q.contains("b"));
        assert_eq!(q.cancel("c"), CancelOutcome::NotQueued);
    }

    #[test]
    fn a_cancelled_job_saves_nothing() {
        let mut q = queue(&["a"]);
        let (_, cancelled) = q.start_next().unwrap();
        assert_eq!(q.cancel("a"), CancelOutcome::Stopping);
        assert!(cancelled.load(Ordering::Acquire));
        // Its decode may still finish (a dictation kept the engine from
        // stopping): the result is not committed.
        assert!(!q.commit());
    }

    #[test]
    fn a_cancel_after_the_commit_is_refused() {
        let mut q = queue(&["a"]);
        let (_, cancelled) = q.start_next().unwrap();
        assert!(q.commit());
        assert_eq!(q.cancel("a"), CancelOutcome::TooLate);
        assert!(!cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn a_running_job_is_not_queued_twice() {
        let mut q = queue(&["a"]);
        q.start_next().unwrap();
        assert!(q.contains("a"));
        assert!(q.start_next().is_none());
    }
}
