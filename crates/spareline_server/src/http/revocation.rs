//! How an open stream, long poll or screen socket learns that its principal was signed out.

use std::pin;
use std::time::Duration;

use futures_util::future;
use spareline_db::Principal;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{self, Instant, Interval};

use crate::app_state::AppState;
use crate::buses::AuthChange;
use crate::clock::Clock;
use crate::db_handle::DbHandle;
use crate::event_log::EventLog;

/// How often a watched principal is checked even without an auth change, which catches changes
/// another process, such as the command line, made, and sessions that expire.
pub(crate) const RECHECK: Duration = Duration::from_secs(5);

/// Watches one principal: [`Revocation::revoked`] resolves once its session has ended or
/// expired, or its key is revoked. It checks on every auth change this process publishes and
/// every [`RECHECK`]. It keeps the database, clock and log rather than the whole [`AppState`],
/// so a screen socket's task, which runs apart from its HTTP connection until
/// [`AppState::close_sockets`] ends it at shutdown, doesn't hold the state.
#[derive(Debug)]
pub(crate) struct Revocation {
    db: DbHandle,
    clock: Clock,
    log: EventLog,
    principal: Principal,
    changes: Receiver<AuthChange>,
    ticks: Interval,
}

impl Revocation {
    /// Starts watching `principal`. Auth changes published from now on count, even before
    /// [`Revocation::revoked`] is first polled.
    pub(crate) fn watch(state: &AppState, principal: Principal) -> Self {
        Self {
            changes: state.buses().auth_changes(),
            db: state.db().clone(),
            clock: state.clock().clone(),
            log: state.log().clone(),
            principal,
            ticks: time::interval_at(Instant::now() + RECHECK, RECHECK),
        }
    }

    /// Resolves when the principal is no longer valid. A failed check is logged and counts as
    /// still valid, so a database hiccup doesn't close every stream.
    pub(crate) async fn revoked(mut self) {
        loop {
            let channel_closed = {
                let changed = pin::pin!(self.changes.recv());
                let ticked = pin::pin!(self.ticks.tick());
                matches!(
                    future::select(changed, ticked).await,
                    future::Either::Left((Err(RecvError::Closed), _))
                )
            };
            if channel_closed {
                self.ticks.tick().await;
            }
            if !self.still_valid().await {
                return;
            }
        }
    }

    async fn still_valid(&self) -> bool {
        let principal = self.principal.clone();
        let now = self.clock.now();
        match self
            .db
            .call(move |db| db.accounts().valid(&principal, now))
            .await
        {
            Ok(valid) => valid,
            Err(error) => {
                self.log
                    .event("request-failed")
                    .field("error", &error.to_string())
                    .write();
                true
            }
        }
    }
}
