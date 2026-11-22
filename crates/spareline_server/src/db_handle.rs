//! The one database connection the service's tasks share.

use std::sync::{Arc, Mutex};
use std::{future, panic};

use spareline_db::Database;
use tokio::task::{self, JoinHandle};

/// The service's database, shared by every task: one [`Database`] behind a mutex, used on
/// tokio's blocking threads so a slow statement never stalls the async ones.
///
/// Cloning is cheap and every clone uses the same connection. Each [`DbHandle::call`] holds the
/// lock for the whole closure, so a closure that reads and then writes sees no other task's
/// change in between, as spareline-old's single-threaded calls did.
#[derive(Clone, Debug)]
pub struct DbHandle {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "every task of the service shares the one SQLite connection, and the \
                      blocking threads that run its calls outlive any borrow"
        )
    )]
    database: Arc<Mutex<Database>>,
}

impl DbHandle {
    /// Shares `database`.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self {
            database: Arc::new(Mutex::new(database)),
        }
    }

    /// Runs `f` with the database on a blocking thread and returns what it returns. Dropping the
    /// future doesn't stop `f`: it finishes its statements, so a cancelled request never leaves
    /// a transaction half done. When the runtime shuts down before `f` starts, tokio cancels it,
    /// and the call never finishes, since its task is being torn down too.
    ///
    /// # Panics
    ///
    /// Panics when an earlier call panicked while it held the database, or `f` panics, with
    /// `f`'s own panic. Either is a bug, and release builds abort on the first panic.
    pub async fn call<T, F>(&self, f: F) -> T
    where
        F: FnOnce(&mut Database) -> T + Send + 'static,
        T: Send + 'static,
    {
        let database = Arc::clone(&self.database);
        joined(task::spawn_blocking(move || {
            let mut guard = database
                .lock()
                .expect("no earlier database call panicked while holding the connection");
            f(&mut guard)
        }))
        .await
    }
}

/// What the blocking task `handle` returned. A task the runtime cancelled at shutdown, before it
/// started, never returns, rather than panicking, which a release build turns into an abort;
/// a panic in the task carries on in the caller.
async fn joined<T>(handle: JoinHandle<T>) -> T {
    match handle.await {
        Ok(value) => value,
        Err(error) if error.is_cancelled() => future::pending().await,
        Err(error) => panic::resume_unwind(error.into_panic()),
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use spareline_core::Timestamp;
    use spareline_db::NewEvent;
    use tokio::runtime::Builder;
    use tokio::task::JoinSet;
    use tokio::time;

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn handle() -> DbHandle {
        let database =
            Database::open_in_memory(|| Timestamp::from_millis(1_793_000_000_000)).unwrap();
        DbHandle::new(database)
    }

    #[test]
    fn runs_calls_off_the_async_threads() {
        block_on(async {
            let db = handle();
            let async_thread = thread::current().id();
            let blocking_thread = db.call(|_| thread::current().id()).await;
            assert_ne!(
                async_thread, blocking_thread,
                "the call ran on a blocking thread"
            );
            let users = db.call(|db| db.accounts().has_users()).await.unwrap();
            assert!(!users, "a new database has no users");
        });
    }

    #[test]
    fn a_call_the_runtime_cancelled_before_it_started_never_finishes_rather_than_panicking() {
        Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async {
                let cancelled = task::spawn(future::pending::<()>());
                cancelled.abort();
                let waited = time::timeout(Duration::from_hours(1), joined(cancelled)).await;
                assert!(waited.is_err(), "a cancelled call waits forever");
            });
    }

    #[test]
    #[should_panic(expected = "the closure's own panic")]
    fn a_panic_in_the_call_carries_on_with_its_own_payload() {
        block_on(async {
            handle().call(|_| panic!("the closure's own panic")).await;
        });
    }

    #[test]
    fn concurrent_calls_share_one_connection_and_never_interleave() {
        block_on(async {
            let db = handle();
            let mut tasks = JoinSet::new();
            for _ in 0..16 {
                let db = db.clone();
                tasks.spawn(async move {
                    db.call(|db| {
                        let before = db.device_events().latest_id().unwrap();
                        let event = NewEvent::adb_reachable().unwrap();
                        let recorded = db
                            .device_events()
                            .insert(&[event], Timestamp::from_millis(1_793_000_000_000))
                            .unwrap();
                        assert_eq!(
                            recorded[0].id,
                            before + 1,
                            "no other call wrote between this call's read and write"
                        );
                    })
                    .await;
                });
            }
            while let Some(done) = tasks.join_next().await {
                done.unwrap();
            }
            let latest = db.call(|db| db.device_events().latest_id()).await.unwrap();
            assert_eq!(latest, 16, "every clone wrote to the same database");
        });
    }
}
