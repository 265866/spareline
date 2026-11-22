//! The monitor's background work in flight, so shutdown and tests can wait for it to settle.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Counts the monitor's queued and running work: checks, Set up phone, SIM reactions and
/// data-offs. Unlike a `JoinSet`, it keeps nothing of a task once it finished, so a service
/// that runs for months doesn't collect them.
///
/// Once closed, it refuses background work, deciding under the same lock that counts it, so no
/// work can start after [`WorkTracker::close`] and slip past a [`WorkTracker::settled`] that
/// follows it.
#[derive(Clone, Debug, Default)]
pub(crate) struct WorkTracker {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "every task the monitor spawns holds the counter and signals it when it \
                      ends, after the spawning call returned"
        )
    )]
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    counts: Mutex<Counts>,
    idle: Notify,
}

/// The work in flight, and whether background work is refused.
#[derive(Debug, Default)]
struct Counts {
    running: usize,
    closed: bool,
}

/// Decrements the count when the task ends, even when it is aborted or panics.
struct Running {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the guard lives in the spawned task, which outlives the tracker's borrow"
        )
    )]
    inner: Arc<Inner>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut counts = lock(&self.inner);
        counts.running -= 1;
        if counts.running == 0 {
            self.inner.idle.notify_waiters();
        }
    }
}

impl WorkTracker {
    /// Spawns `work` on the runtime and counts it until it ends, closed or not, for work a
    /// caller asked for and waits on. The work carries on when the handle is dropped, so a
    /// caller that stops waiting doesn't cut a check short.
    pub(crate) fn spawn<F>(&self, work: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let mut counts = lock(&self.inner);
        counts.running += 1;
        drop(counts);
        self.start(work)
    }

    /// Spawns and counts `work`, as [`WorkTracker::spawn`] does, unless the tracker is closed,
    /// for the monitor's own background work. Returns `None`, and drops `work` unstarted, when
    /// it is closed.
    pub(crate) fn spawn_while_open<F>(&self, work: F) -> Option<JoinHandle<F::Output>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let mut counts = lock(&self.inner);
        if counts.closed {
            return None;
        }
        counts.running += 1;
        drop(counts);
        Some(self.start(work))
    }

    /// Admits background work again, after [`WorkTracker::close`].
    pub(crate) fn open(&self) {
        lock(&self.inner).closed = false;
    }

    /// Refuses background work from now on. Work already admitted carries on and is still
    /// counted.
    pub(crate) fn close(&self) {
        lock(&self.inner).closed = true;
    }

    /// Waits until no counted work is left, including work that counted work spawned.
    pub(crate) async fn settled(&self) {
        loop {
            let idle = self.inner.idle.notified();
            if lock(&self.inner).running == 0 {
                return;
            }
            idle.await;
        }
    }

    /// Spawns `work` with a guard that uncounts it when it ends; the caller has counted it and
    /// released the counts' lock. A runtime that is shutting down drops `work` inside
    /// `tokio::spawn`, and with it the guard, which takes that lock.
    fn start<F>(&self, work: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let running = Running {
            inner: Arc::clone(&self.inner),
        };
        tokio::spawn(async move {
            let output = work.await;
            drop(running);
            output
        })
    }
}

/// The counts, locked for one change or read.
fn lock(inner: &Inner) -> MutexGuard<'_, Counts> {
    inner
        .counts
        .lock()
        .expect("no monitor task panicked while counting its work")
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use tokio::runtime::Builder;
    use tokio::sync::oneshot;
    use tokio::{task, time};

    use super::*;

    #[test]
    fn settles_once_nested_work_is_done_and_dropped_handles_keep_running() {
        Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async {
                let tracker = WorkTracker::default();
                tracker.settled().await;
                let (done, finished) = oneshot::channel();
                let nested = tracker.clone();
                drop(tracker.spawn(async move {
                    time::sleep(Duration::from_secs(5)).await;
                    nested.spawn(async move {
                        time::sleep(Duration::from_secs(5)).await;
                        done.send(()).unwrap();
                    });
                }));
                tracker.settled().await;
                assert_eq!(finished.await, Ok(()), "the nested work ran to its end");
            });
    }

    #[test]
    fn work_spawned_while_the_runtime_shuts_down_does_not_hang_the_shutdown() {
        let (dropped, runtime_gone) = mpsc::channel();
        let shutting_down = thread::spawn(move || {
            let runtime = Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap();
            let tracker = WorkTracker::default();
            for _ in 0..8 {
                let tracker = tracker.clone();
                drop(runtime.spawn(async move {
                    loop {
                        drop(tracker.spawn_while_open(async {}));
                        drop(tracker.spawn(async {}));
                        task::yield_now().await;
                    }
                }));
            }
            thread::sleep(Duration::from_millis(50));
            drop(runtime);
            dropped.send(()).unwrap();
        });
        assert_eq!(
            runtime_gone.recv_timeout(Duration::from_secs(30)),
            Ok(()),
            "a spawn the closing runtime refuses drops its guard without deadlocking"
        );
        shutting_down.join().unwrap();
    }

    #[test]
    fn closing_refuses_background_work_but_waits_for_what_it_admitted() {
        Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async {
                let tracker = WorkTracker::default();
                let (done, mut finished) = oneshot::channel();
                let admitted = tracker.spawn_while_open(async move {
                    time::sleep(Duration::from_secs(5)).await;
                    done.send(()).unwrap();
                });
                assert!(admitted.is_some(), "an open tracker admits work");
                tracker.close();
                let refused = tracker.spawn_while_open(async {
                    panic!("work offered after the close never starts")
                });
                assert!(refused.is_none());
                tracker.settled().await;
                assert_eq!(
                    finished.try_recv(),
                    Ok(()),
                    "settled waited for the admitted work"
                );
                let asked = tracker.spawn(async { 7 });
                assert_eq!(asked.await.unwrap(), 7, "work a caller waits on still runs");
                tracker.open();
                assert!(
                    tracker.spawn_while_open(async {}).is_some(),
                    "reopened, it admits work again"
                );
                tracker.settled().await;
            });
    }
}
