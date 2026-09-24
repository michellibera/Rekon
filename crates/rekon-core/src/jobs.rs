//! Worker threads for model calls. No async runtime: every call is a child process,
//! so plain threads are enough.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

/// Runs `f` over `items` on `workers` threads and returns the results (in completion order).
pub fn run_parallel<T, R, F>(workers: usize, items: Vec<T>, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
{
    let queue = Mutex::new(VecDeque::from(items));
    let results = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..workers.max(1) {
            s.spawn(|| {
                loop {
                    let Some(item) = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front() else {
                        break;
                    };
                    let r = f(item);
                    results.lock().unwrap_or_else(|e| e.into_inner()).push(r);
                }
            });
        }
    });
    results.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// Identity of a job; a second job with the same key is ignored while the first
/// one is queued or running.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum JobKey {
    FileSummary(String),
    DirSummary(String),
    Level1(String),
    Split(String, (u32, u32)),
    Init,
}

#[derive(Debug)]
pub struct JobDone {
    pub key: JobKey,
    pub result: Result<(), String>,
}

type Work = Box<dyn FnOnce() -> anyhow::Result<()> + Send>;

struct State {
    queue: VecDeque<(JobKey, Work)>,
    keys: HashSet<JobKey>,
    shutdown: bool,
}

struct Inner {
    state: Mutex<State>,
    ready: Condvar,
}

/// Fixed pool of worker threads with a queue and deduplication by [`JobKey`].
/// Each job saves its own result through the store; the pool reports completion
/// on the `done` channel.
pub struct JobPool {
    inner: Arc<Inner>,
}

impl JobPool {
    pub fn new(workers: usize, done: Sender<JobDone>) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                keys: HashSet::new(),
                shutdown: false,
            }),
            ready: Condvar::new(),
        });
        for _ in 0..workers.max(1) {
            let inner = Arc::clone(&inner);
            let done = done.clone();
            std::thread::spawn(move || worker(&inner, &done));
        }
        Self { inner }
    }

    /// Queues a job; returns false when a job with the same key is already pending.
    pub fn submit(&self, key: JobKey, work: impl FnOnce() -> anyhow::Result<()> + Send + 'static) -> bool {
        let mut state = self.lock();
        if !state.keys.insert(key.clone()) {
            return false;
        }
        state.queue.push_back((key, Box::new(work)));
        self.inner.ready.notify_one();
        true
    }

    pub fn is_pending(&self, key: &JobKey) -> bool {
        self.lock().keys.contains(key)
    }

    pub fn pending(&self) -> HashSet<JobKey> {
        self.lock().keys.clone()
    }

    pub fn pending_count(&self) -> usize {
        self.lock().keys.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for JobPool {
    fn drop(&mut self) {
        self.lock().shutdown = true;
        self.inner.ready.notify_all();
    }
}

fn worker(inner: &Inner, done: &Sender<JobDone>) {
    loop {
        let (key, work) = {
            let mut state = inner.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.shutdown {
                    return;
                }
                if let Some(job) = state.queue.pop_front() {
                    break job;
                }
                state = inner.ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("job panicked")))
            .map_err(|e| format!("{e:#}"));
        inner.state.lock().unwrap_or_else(|e| e.into_inner()).keys.remove(&key);
        if done.send(JobDone { key, result }).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn parallel_runs_everything() {
        let mut out = run_parallel(3, (0..20).collect(), |i: i32| i * 2);
        out.sort();
        assert_eq!(out, (0..20).map(|i| i * 2).collect::<Vec<_>>());
    }

    #[test]
    fn pool_deduplicates_pending_keys() {
        let (tx, rx) = mpsc::channel();
        let pool = JobPool::new(1, tx);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let key = JobKey::Level1("a.rs".into());
        assert!(pool.submit(key.clone(), move || {
            gate_rx.recv().ok();
            Ok(())
        }));
        assert!(!pool.submit(key.clone(), || Ok(())));
        assert!(pool.is_pending(&key));
        gate_tx.send(()).unwrap();
        let done = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(done.key, key);
        assert!(done.result.is_ok());
        assert!(!pool.is_pending(&key));
        assert!(pool.submit(key, || anyhow::bail!("boom")));
        let done = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(done.result.unwrap_err(), "boom");
    }
}
