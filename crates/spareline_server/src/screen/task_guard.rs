//! A wait for every task the screens' shutdown must outlast.

use std::sync::{Mutex, MutexGuard};

use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc::{self, Receiver, Sender};

/// Held by each task the shutdown waits for: every phone's relay, and every session until its
/// server has been killed. Cloning it adds a holder.
#[derive(Clone, Debug)]
pub(crate) struct TaskGuard {
    _holder: Sender<()>,
}

/// Hands out guards until it closes, then waits for every guard to be dropped.
#[derive(Debug)]
pub(crate) struct TaskGuards {
    holder: Mutex<Option<Sender<()>>>,
    receiver: AsyncMutex<Receiver<()>>,
}

impl TaskGuards {
    /// Guards that hand out guards.
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::channel(1);
        Self {
            holder: Mutex::new(Some(sender)),
            receiver: AsyncMutex::new(receiver),
        }
    }

    /// A new guard, or `None` once [`TaskGuards::close_and_wait`] has begun.
    pub(crate) fn guard(&self) -> Option<TaskGuard> {
        self.holder()
            .clone()
            .map(|sender| TaskGuard { _holder: sender })
    }

    /// Stops handing out guards and finishes once every guard is dropped.
    pub(crate) async fn close_and_wait(&self) {
        drop(self.holder().take());
        let mut receiver = self.receiver.lock().await;
        while receiver.recv().await.is_some() {}
    }

    fn holder(&self) -> MutexGuard<'_, Option<Sender<()>>> {
        self.holder
            .lock()
            .expect("no task panics while taking a guard")
    }
}

#[cfg(test)]
mod tests {
    use tokio::runtime::Builder;
    use tokio::task;

    use super::*;

    #[test]
    fn the_wait_finishes_once_every_guard_is_dropped_and_no_more_are_handed_out() {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let guards = TaskGuards::new();
                let held = guards.guard().unwrap();
                let worker = tokio::spawn(async move {
                    task::yield_now().await;
                    drop(held);
                });
                guards.close_and_wait().await;
                assert!(guards.guard().is_none(), "a closed wait hands out no guard");
                worker.await.unwrap();
                guards.close_and_wait().await;
            });
    }
}
