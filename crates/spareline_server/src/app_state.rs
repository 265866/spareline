//! What every HTTP route shares.

use std::mem;
use std::sync::{Arc, Mutex};

use spareline_db::{AttachmentStore, Passwords};
use tokio::sync::watch::Sender;
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio::time::{self, Instant};

use crate::buses::Buses;
use crate::clock::Clock;
use crate::config::Config;
use crate::db_handle::DbHandle;
use crate::devices::Devices;
use crate::event_log::EventLog;
use crate::login_limiter::LoginLimiter;
use crate::monitor::Monitor;
use crate::screen::Screens;
use crate::web_files::WebFiles;

/// The parts of the running service the HTTP layer uses, handed to [`AppState::new`].
#[derive(Debug)]
pub struct AppParts {
    /// The settings.
    pub config: Config,
    /// The version `/api/v1/health` and the API docs report.
    pub version: String,
    /// The database.
    pub db: DbHandle,
    /// The channels device events, texts and auth changes are published on.
    pub buses: Buses,
    /// Where the routes read the time.
    pub clock: Clock,
    /// Where the routes log.
    pub log: EventLog,
    /// The phone manager.
    pub devices: Devices,
    /// The policy checks, Set up phone and the alerts.
    pub monitor: Monitor,
    /// The phones' screen sessions.
    pub screens: Screens,
}

/// The state every route shares. Cloning is cheap: it is one reference to the shared parts.
#[derive(Clone, Debug)]
pub struct AppState {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "axum hands every request its own clone of the state, and the request \
                      tasks outlive any borrow"
        )
    )]
    shared: Arc<Shared>,
}

/// What [`AppState`] shares.
#[derive(Debug)]
struct Shared {
    config: Config,
    version: String,
    db: DbHandle,
    buses: Buses,
    clock: Clock,
    log: EventLog,
    devices: Devices,
    monitor: Monitor,
    screens: Screens,
    limiter: Mutex<LoginLimiter>,
    passwords: Passwords,
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "each permit moves into the blocking task that hashes, which runs on after \
                      a request whose client went away is dropped"
        )
    )]
    hashing: Arc<Semaphore>,
    shutdown: Sender<bool>,
    sockets: Mutex<JoinSet<()>>,
    web: WebFiles,
    attachments: AttachmentStore,
}

impl AppState {
    /// The state of a service built from `parts`, with an empty sign-in limiter.
    #[must_use]
    pub fn new(parts: AppParts) -> Self {
        let web = WebFiles::new(parts.config.web_dir.clone());
        let attachments = AttachmentStore::new(parts.config.data_dir.join("attachments"));
        Self {
            shared: Arc::new(Shared {
                config: parts.config,
                version: parts.version,
                db: parts.db,
                buses: parts.buses,
                clock: parts.clock,
                log: parts.log,
                devices: parts.devices,
                monitor: parts.monitor,
                screens: parts.screens,
                limiter: Mutex::new(LoginLimiter::new()),
                passwords: Passwords::new(),
                hashing: Arc::new(Semaphore::new(spareline_db::max_concurrent_hashes())),
                shutdown: Sender::new(false),
                sockets: Mutex::new(JoinSet::new()),
                web,
                attachments,
            }),
        }
    }

    /// The settings.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.shared.config
    }

    /// The version the service reports.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.shared.version
    }

    /// The database.
    #[must_use]
    pub fn db(&self) -> &DbHandle {
        &self.shared.db
    }

    /// The broadcast channels.
    #[must_use]
    pub fn buses(&self) -> &Buses {
        &self.shared.buses
    }

    /// The clock.
    #[must_use]
    pub fn clock(&self) -> &Clock {
        &self.shared.clock
    }

    /// The log.
    #[must_use]
    pub fn log(&self) -> &EventLog {
        &self.shared.log
    }

    /// The phone manager.
    #[must_use]
    pub fn devices(&self) -> &Devices {
        &self.shared.devices
    }

    /// The monitor.
    #[must_use]
    pub fn monitor(&self) -> &Monitor {
        &self.shared.monitor
    }

    /// The screen sessions.
    #[must_use]
    pub fn screens(&self) -> &Screens {
        &self.shared.screens
    }

    /// The failed sign-ins per name. Hold the lock only for one check-and-record.
    pub(crate) fn limiter(&self) -> &Mutex<LoginLimiter> {
        &self.shared.limiter
    }

    /// The password hasher, which keeps the dummy hash unknown names are checked against.
    pub(crate) fn passwords(&self) -> &Passwords {
        &self.shared.passwords
    }

    /// Waits for a place to check or hash a password on a blocking thread. There are as many
    /// places as `spareline_db`'s hash gate lets hashes run,
    /// [`spareline_db::max_concurrent_hashes`], so the others wait as async tasks rather than on
    /// blocking threads. The permit moves into
    /// that thread's task, so a request dropped while its hash runs still holds its place until
    /// the hash ends.
    ///
    /// # Errors
    ///
    /// Never: the semaphore is never closed.
    pub(crate) async fn hash_permit(&self) -> Result<OwnedSemaphorePermit, AcquireError> {
        Arc::clone(&self.shared.hashing).acquire_owned().await
    }

    /// How many places to check or hash a password are free now.
    #[must_use]
    pub fn free_hash_permits(&self) -> usize {
        self.shared.hashing.available_permits()
    }

    /// Runs `socket`, an upgraded connection such as a screen socket, as a task the server waits
    /// for at shutdown: [`AppState::close_sockets`] waits for it, then drops it. A socket started
    /// after that goes into a new set, whose tasks are dropped with the last clone of the state.
    ///
    /// # Panics
    ///
    /// Panics when a task panicked while holding the set of socket tasks, a bug.
    pub fn spawn_socket<F>(&self, socket: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .expect("no task panicked while holding the socket tasks");
        while sockets.try_join_next().is_some() {}
        sockets.spawn(socket);
    }

    /// Waits until `deadline` for every task [`AppState::spawn_socket`] started to end, then
    /// drops the rest, returning once none is left.
    ///
    /// # Panics
    ///
    /// Panics when a task panicked while holding the set of socket tasks, a bug.
    pub async fn close_sockets(&self, deadline: Instant) {
        let mut sockets = mem::take(
            &mut *self
                .shared
                .sockets
                .lock()
                .expect("no task panicked while holding the socket tasks"),
        );
        let drained = time::timeout_at(deadline, async {
            while sockets.join_next().await.is_some() {}
        })
        .await;
        match drained {
            Ok(()) | Err(_) => {}
        }
        sockets.shutdown().await;
    }

    /// Tells every open event stream to end and every waiting long poll to answer its empty page
    /// now, as the service stops. Requests that start waiting later answer at once.
    pub fn shut_down(&self) {
        self.shared.shutdown.send_replace(true);
    }

    /// Resolves once [`AppState::shut_down`] has been called. The future holds no reference to
    /// the state, so a stream that waits on it doesn't keep the state alive.
    pub(crate) fn stopping(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut receiver = self.shared.shutdown.subscribe();
        async move {
            match receiver.wait_for(|stopping| *stopping).await {
                Ok(_) | Err(_) => {}
            }
        }
    }

    /// The built web UI.
    pub(crate) fn web(&self) -> &WebFiles {
        &self.shared.web
    }

    /// The MMS part files.
    pub(crate) fn attachments(&self) -> &AttachmentStore {
        &self.shared.attachments
    }
}
