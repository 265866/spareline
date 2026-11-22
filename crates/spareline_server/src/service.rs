//! The running service: startup in spareline-old's order, the HTTP server, and the shutdown on
//! SIGTERM or SIGINT.

use std::net::SocketAddr;
use std::path::{self, Path, PathBuf};
use std::time::Duration;
use std::{io, pin};

use axum::Router;
use futures_util::future;
use hyper::server::conn::http1::Builder;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use spareline_adb::AdbClient;
use spareline_core::monitor::{Policy, ProfileError};
use spareline_db::{AttachmentStore, Database, DatabaseError};
use thiserror::Error;
use tokio::fs;
use tokio::net::{TcpListener, TcpStream};
use tokio::signal::unix::{self, SignalKind};
use tokio::task::{self, JoinError, JoinHandle, JoinSet};
use tokio::time::{self, Instant};

use crate::app_state::{AppParts, AppState};
use crate::buses::Buses;
use crate::clock::Clock;
use crate::config::Config;
use crate::db_handle::DbHandle;
use crate::devices::{DeviceManager, DeviceOptions};
use crate::event_log::EventLog;
use crate::http;
use crate::monitor::{Monitor, MonitorContext, MonitorSettings};
use crate::screen::Screens;
use crate::sms_ingest::{SmsIngest, SmsIngestOptions};

/// How often expired sessions are deleted, after the first time at startup.
const PRUNE_INTERVAL: Duration = Duration::from_hours(6);

/// How long requests still open at shutdown may run before the server drops them. Event streams
/// end and long polls answer as soon as the stop begins, so this bounds only requests that
/// don't, such as a client that never finishes sending its body. spareline-old's process exited
/// under them; this keeps the stop well inside the container's 25 s stop timeout.
const DRAIN: Duration = Duration::from_secs(5);

/// How long a client may take to send a request's headers before its connection is closed,
/// hyper's own default, so a client that never finishes them can't hold a socket forever.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the server waits before accepting again after an error such as running out of file
/// descriptors, which would otherwise fail every accept at once.
const ACCEPT_RETRY: Duration = Duration::from_secs(1);

/// The hint the `no-users` log line gives.
const NO_USERS_HINT: &str = "create one with: spareline users add NAME";

/// Why the service couldn't start or stopped with an error. Each shows as the message the
/// binary prints before it exits 1.
#[derive(Debug, Error)]
#[must_use]
pub enum ServiceError {
    /// The policy profile couldn't be read or doesn't match the schema.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// The database couldn't be opened or migrated, or a startup query failed.
    #[error("cannot open the database {path}: {source}")]
    Database {
        /// The database file.
        path: PathBuf,
        /// What failed.
        #[source]
        source: DatabaseError,
    },
    /// The task opening the database ended without an answer, as when it panics.
    #[error("cannot open the database {path}: the task opening it failed: {source}")]
    DatabaseTask {
        /// The database file.
        path: PathBuf,
        /// How the task ended.
        #[source]
        source: JoinError,
    },
    /// A startup query failed.
    #[error("the database failed at startup: {0}")]
    Startup(#[source] DatabaseError),
    /// The HTTP port couldn't be bound.
    #[error("cannot listen on {address}: {source}")]
    Listen {
        /// The `HOST:PORT` asked for.
        address: String,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// The signal handlers couldn't be installed.
    #[error("the HTTP server failed: {0}")]
    Serve(#[source] io::Error),
    /// The task serving HTTP ended without an answer, as when it panics.
    #[error("the HTTP server failed: {0}")]
    ServeTask(#[source] JoinError),
}

/// Which signal stopped the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopSignal {
    /// `SIGTERM`, as podman and systemd stop the container.
    Terminate,
    /// `SIGINT`, as Ctrl-C in a terminal.
    Interrupt,
}

impl StopSignal {
    /// The signal's name as the `stopping` log line gives it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Terminate => "SIGTERM",
            Self::Interrupt => "SIGINT",
        }
    }

    /// Waits for SIGTERM or SIGINT.
    ///
    /// # Errors
    ///
    /// Fails when the signal handlers can't be installed.
    pub async fn wait() -> io::Result<Self> {
        let mut terminate = unix::signal(SignalKind::terminate())?;
        let mut interrupt = unix::signal(SignalKind::interrupt())?;
        let terminated = pin::pin!(terminate.recv());
        let interrupted = pin::pin!(interrupt.recv());
        Ok(match future::select(terminated, interrupted).await {
            future::Either::Left(_) => Self::Terminate,
            future::Either::Right(_) => Self::Interrupt,
        })
    }
}

/// The started service: every part is running and the port is bound, but no request is served
/// until [`Service::serve`].
#[derive(Debug)]
pub struct Service {
    state: AppState,
    listener: TcpListener,
    prune: JoinHandle<()>,
    manager: DeviceManager,
    sms: SmsIngest,
}

impl Service {
    /// Starts the service: reads the policy profile and binds the listening socket, so a second
    /// instance that can't bind the port stops before it touches the database or a phone; then,
    /// in spareline-old's order, the database and its migrations (each logged as
    /// `migration-applied`), the phone manager, text ingest, the monitor, the screens, which
    /// feed the monitor's screen sessions, the session prune at start and every 6 h, and the
    /// `no-users` line when nobody can sign in yet; then starts ingest, which subscribes before
    /// the phone manager runs so it sees the first phone, the monitor and the phone manager; and
    /// last writes the `listening` line.
    ///
    /// Because the port is bound first, a client that connects while the migrations run waits in
    /// the listen queue until [`Service::serve`] starts accepting, rather than being refused.
    ///
    /// # Errors
    ///
    /// Fails when the profile is invalid, the port can't be bound, or the database can't be
    /// opened.
    pub async fn start(
        config: Config,
        version: String,
        log: EventLog,
    ) -> Result<Self, ServiceError> {
        let clock = Clock::system();
        let policy = load_policy(config.policy_profile.as_deref()).await?;
        let policy_name = policy.as_ref().map(|policy| policy.name.clone());
        let address = format!("{}:{}", config.host, config.port);
        let listener = TcpListener::bind((config.host.as_str(), config.port))
            .await
            .map_err(|source| ServiceError::Listen {
                address: address.clone(),
                source,
            })?;
        let path = config.database_path();
        let db = open_database(&path, &clock, &log).await?;
        let buses = Buses::new();
        let mut manager = DeviceManager::new(DeviceOptions::new(
            AdbClient::new(config.adb_server.clone()),
            db.clone(),
            buses.clone(),
            config.site.clone(),
            log.clone(),
            clock.clone(),
            config.health_interval,
        ));
        let devices = manager.devices();
        let mut sms = SmsIngest::new(SmsIngestOptions::new(
            devices.clone(),
            db.clone(),
            buses.clone(),
            AttachmentStore::new(config.data_dir.join("attachments")),
            log.clone(),
            clock.clone(),
        ));
        let monitor = Monitor::new(
            MonitorSettings::new(&config, policy),
            MonitorContext {
                devices: devices.clone(),
                db: db.clone(),
                buses: buses.clone(),
                clock: clock.clone(),
                log: log.clone(),
            },
        );
        let screens = Screens::new(
            devices.clone(),
            db.clone(),
            clock.clone(),
            log.clone(),
            monitor.screen_sessions(),
        );
        prune_sessions(&db, &buses, &clock)
            .await
            .map_err(ServiceError::Startup)?;
        let has_users = db
            .call(|db| db.accounts().has_users())
            .await
            .map_err(ServiceError::Startup)?;
        if !has_users {
            log.event("no-users").field("hint", NO_USERS_HINT).write();
        }
        sms.start().await.map_err(ServiceError::Startup)?;
        monitor.start();
        manager.start();
        log.event("listening")
            .field("url", &format!("http://{address}"))
            .field("adb", &config.adb_server.to_string())
            .field("data", &absolute(&config.data_dir))
            .field("web", &absolute(&config.web_dir))
            .field("policy", &policy_name)
            .field("version", &version)
            .write();
        let prune = task::spawn(prune_every(
            db.clone(),
            buses.clone(),
            clock.clone(),
            log.clone(),
        ));
        let state = AppState::new(AppParts {
            config,
            version,
            db,
            buses,
            clock,
            log,
            devices,
            monitor,
            screens,
        });
        Ok(Self {
            state,
            listener,
            prune,
            manager,
            sms,
        })
    }

    /// The state the routes share.
    #[must_use]
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The address the service listens on.
    ///
    /// # Errors
    ///
    /// Fails when the socket can't say, which a bound socket doesn't.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves requests until `stop` resolves, then shuts down in spareline-old's order: logs
    /// `stopping`, closes every screen viewer with 1001 and stops the scrcpy servers, stops the
    /// HTTP server, stops text ingest, stops the monitor (which writes its partial health
    /// buckets), stops the phone manager and the session prune, and closes the database.
    ///
    /// The HTTP server stops accepting, ends every event stream and answers every waiting long
    /// poll with its empty page, and lets open requests and screen sockets finish for up to 5 s
    /// before it drops them. It returns only once every connection's task and every socket's
    /// task has ended, so none outlives the server with the service's state.
    ///
    /// # Errors
    ///
    /// Fails when `stop` does, or the HTTP server's task panics.
    pub async fn serve<F>(self, stop: F) -> Result<(), ServiceError>
    where
        F: Future<Output = io::Result<StopSignal>> + Send + 'static,
    {
        let Self {
            state,
            listener,
            prune,
            manager,
            sms,
        } = self;
        let router = http::router(state.clone());
        let server = task::spawn(serve_http(listener, router, state.clone()));
        let signal = match stop.await {
            Ok(signal) => signal,
            Err(error) => {
                state.shut_down();
                server.abort();
                prune.abort();
                return Err(ServiceError::Serve(error));
            }
        };
        state
            .log()
            .event("stopping")
            .field("signal", signal.name())
            .write();
        state.screens().shutdown().await;
        state.shut_down();
        server.await.map_err(ServiceError::ServeTask)?;
        sms.stop().await;
        state.monitor().stop().await;
        manager.stop().await;
        prune.abort();
        drop(state);
        Ok(())
    }
}

/// Serves `router` on every connection `listener` accepts until `state` is shut down; then stops
/// accepting, waits up to [`DRAIN`] for the open connections and then the upgraded sockets to
/// finish, and drops the rest. It returns only once every one of their tasks has ended.
async fn serve_http(listener: TcpListener, router: Router, state: AppState) {
    let mut connections = JoinSet::new();
    let mut stopping = pin::pin!(state.stopping());
    loop {
        let accepted = {
            let accept = pin::pin!(listener.accept());
            match future::select(accept, stopping.as_mut()).await {
                future::Either::Left((accepted, _)) => accepted,
                future::Either::Right(((), _)) => break,
            }
        };
        match accepted {
            Ok((stream, _)) => {
                connections.spawn(serve_connection(stream, router.clone(), state.stopping()));
            }
            Err(error) => accept_failed(state.log(), &error, state.stopping()).await,
        }
        while connections.try_join_next().is_some() {}
    }
    drop(listener);
    let deadline = Instant::now() + DRAIN;
    let drained = time::timeout_at(deadline, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    match drained {
        Ok(()) | Err(_) => {}
    }
    connections.shutdown().await;
    state.close_sockets(deadline).await;
}

/// Serves HTTP/1.1, and WebSocket upgrades, on one connection until the client closes it or,
/// once `stopping` resolves, until its request in flight is answered. A client that takes more
/// than [`HEADER_READ_TIMEOUT`] to send a request's headers is disconnected.
async fn serve_connection<F>(stream: TcpStream, router: Router, stopping: F)
where
    F: Future<Output = ()>,
{
    let service = TowerToHyperService::new(router);
    let connection = Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
    let mut connection = pin::pin!(connection);
    let stopping = pin::pin!(stopping);
    let stopped = matches!(
        future::select(connection.as_mut(), stopping).await,
        future::Either::Right(_)
    );
    if stopped {
        connection.as_mut().graceful_shutdown();
        match connection.await {
            Ok(()) | Err(_) => {}
        }
    }
}

/// Logs a failed accept as `accept-failed`. A client that reset the connection first is
/// nothing to wait for; anything else, such as running out of file descriptors, waits
/// [`ACCEPT_RETRY`] before the next accept, or until `stopping` resolves.
async fn accept_failed<F>(log: &EventLog, error: &io::Error, stopping: F)
where
    F: Future<Output = ()>,
{
    let client_gone = matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    );
    if client_gone {
        return;
    }
    log.event("accept-failed")
        .field("error", &error.to_string())
        .write();
    let retry = pin::pin!(time::sleep(ACCEPT_RETRY));
    let stopping = pin::pin!(stopping);
    match future::select(retry, stopping).await {
        future::Either::Left(_) | future::Either::Right(_) => {}
    }
}

/// Reads and checks the policy profile `path` names, or `None` without one.
async fn load_policy(path: Option<&Path>) -> Result<Option<Policy>, ProfileError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let shown = path.to_string_lossy().into_owned();
    let text = fs::read_to_string(path)
        .await
        .map_err(|error| ProfileError::unreadable(&shown, &error.to_string()))?;
    Policy::load(&shown, &text).map(Some)
}

/// Opens and migrates the database on a blocking thread, logging each migration it applies.
async fn open_database(
    path: &Path,
    clock: &Clock,
    log: &EventLog,
) -> Result<DbHandle, ServiceError> {
    let owned = path.to_owned();
    let clock = clock.clone();
    let log = log.clone();
    let opened = task::spawn_blocking(move || {
        Database::open(
            &owned,
            || clock.now(),
            |applied| {
                log.event("migration-applied")
                    .field("file", applied.file())
                    .write();
            },
        )
    })
    .await
    .map_err(|source| ServiceError::DatabaseTask {
        path: path.to_owned(),
        source,
    })?;
    opened
        .map(DbHandle::new)
        .map_err(|source| ServiceError::Database {
            path: path.to_owned(),
            source,
        })
}

/// Deletes the sessions that expired, and tells open streams to check theirs when any did.
async fn prune_sessions(db: &DbHandle, buses: &Buses, clock: &Clock) -> Result<(), DatabaseError> {
    let now = clock.now();
    let pruned = db.call(move |db| db.accounts().prune_sessions(now)).await?;
    if pruned > 0 {
        buses.publish_auth_change();
    }
    Ok(())
}

/// Prunes the expired sessions every [`PRUNE_INTERVAL`], logging a failure as
/// `session-prune-failed` and carrying on.
async fn prune_every(db: DbHandle, buses: Buses, clock: Clock, log: EventLog) {
    let mut ticks = time::interval_at(Instant::now() + PRUNE_INTERVAL, PRUNE_INTERVAL);
    loop {
        ticks.tick().await;
        if let Err(error) = prune_sessions(&db, &buses, &clock).await {
            log.event("session-prune-failed")
                .field("error", &error.to_string())
                .write();
        }
    }
}

/// `path` made absolute against the working directory, as Node's `path.resolve` does, without
/// following links.
fn absolute(path: &Path) -> String {
    path::absolute(path)
        .unwrap_or_else(|_| path.to_owned())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use std::{env, fs, process};

    use rusqlite::Connection;
    use serde_json::Value;
    use spareline_core::Timestamp;
    use tokio::runtime;

    use super::*;

    fn block_on<F: Future>(future: F) -> F::Output {
        runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn a_failed_prune_is_logged_as_a_session_prune_failure() {
        block_on(async {
            let directory = env::temp_dir().join(format!("spareline-prune-test-{}", process::id()));
            fs::create_dir_all(&directory).unwrap();
            let path = directory.join("spareline.db");
            let clock = Clock::fixed(Timestamp::from_millis(0));
            let db = open_database(&path, &clock, &EventLog::discard())
                .await
                .unwrap();
            Connection::open(&path)
                .unwrap()
                .execute_batch("DROP TABLE sessions")
                .unwrap();
            let log = EventLog::memory(clock.clone());
            let prune = task::spawn(prune_every(db, Buses::new(), clock, log.clone()));
            time::sleep(PRUNE_INTERVAL).await;
            let mut lines = log.lines();
            for _ in 0..1000 {
                if !lines.is_empty() {
                    break;
                }
                time::sleep(Duration::from_millis(10)).await;
                lines = log.lines();
            }
            prune.abort();
            fs::remove_dir_all(&directory).unwrap();
            assert_eq!(lines.len(), 1, "{lines:?}");
            let line: Value = serde_json::from_str(&lines[0]).unwrap();
            assert_eq!(line["event"], "session-prune-failed");
            assert!(
                line["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("sessions") && !error.contains("startup")),
                "{line}"
            );
        });
    }

    #[test]
    fn a_failed_accept_waits_a_second_unless_the_service_is_stopping() {
        block_on(async {
            let log = EventLog::memory(Clock::fixed(Timestamp::from_millis(0)));
            let error = io::Error::other("too many open files");
            let started = Instant::now();
            accept_failed(&log, &error, future::pending()).await;
            assert_eq!(
                started.elapsed(),
                ACCEPT_RETRY,
                "it waited before the next accept"
            );
            let stopping = Instant::now();
            accept_failed(&log, &error, future::ready(())).await;
            assert_eq!(
                stopping.elapsed(),
                Duration::ZERO,
                "a stopping service doesn't wait"
            );
            let reset = io::Error::from(io::ErrorKind::ConnectionReset);
            accept_failed(&log, &reset, future::pending()).await;
            assert_eq!(log.lines().len(), 2, "a client's reset isn't logged");
        });
    }

    #[test]
    fn a_failed_task_opening_the_database_is_reported_as_such() {
        block_on(async {
            let source = task::spawn_blocking(|| panic!("the open panicked"))
                .await
                .unwrap_err();
            let error = ServiceError::DatabaseTask {
                path: PathBuf::from("/data/spareline.db"),
                source,
            };
            let message = error.to_string();
            assert!(
                message.starts_with(
                    "cannot open the database /data/spareline.db: the task opening it failed: "
                ),
                "{message}"
            );
        });
    }
}
