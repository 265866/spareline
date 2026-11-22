//! The whole service over TCP: startup in the old order with its log lines, requests, and the
//! stop.

use std::collections::HashMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::{env, fs, io, process};

use futures_util::future;
use rusqlite::Connection;
use serde_json::Value;
use spareline_core::Timestamp;
use spareline_server::{Clock, Config, EventLog, Service, ServiceError, StopSignal};
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpStream;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::oneshot::{self, Sender};
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};

use crate::test_app::{self, PASSWORD, PASSWORD_HASH, START};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// How long a test waits for the service to answer before it calls the service stuck: far past
/// the service's 5 s drain, on tokio's paused clock.
const STUCK: Duration = Duration::from_secs(60);

/// A temporary directory for one service's data and web UI, removed on drop.
struct Directory {
    path: PathBuf,
}

impl Directory {
    fn new() -> Self {
        Self {
            path: env::temp_dir().join(format!(
                "spareline-service-test-{}-{}",
                process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )),
        }
    }

    /// The service's settings over this directory, on `port`, with an adb server nobody
    /// answers at.
    fn config(&self, port: u16) -> Config {
        let variables: HashMap<&str, OsString> = HashMap::from([
            ("DATA_DIR", self.path.join("data").into_os_string()),
            ("SPARELINE_WEB_DIR", self.path.join("web").into_os_string()),
            ("ADB_SERVER", OsString::from("127.0.0.1:1")),
        ]);
        let mut config =
            Config::from_env(|name| variables.get(name).cloned()).expect("valid settings");
        config.port = port;
        config
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            assert!(
                error.kind() == io::ErrorKind::NotFound,
                "cannot remove {}: {error}",
                self.path.display()
            );
        }
    }
}

/// A running service and the means to stop it.
struct Running {
    address: SocketAddr,
    stop: Sender<()>,
    serving: JoinHandle<Result<(), ServiceError>>,
}

impl Running {
    /// Starts the service on `config`, logging to `log`, and serves it.
    async fn start(config: Config, log: &EventLog) -> Self {
        Self::start_with(config, log, |_| {}).await
    }

    /// [`Running::start`], running `prepare` on the started service before it serves.
    async fn start_with<P: FnOnce(&Service)>(config: Config, log: &EventLog, prepare: P) -> Self {
        let service = Service::start(config, "0.1.0".to_owned(), log.clone())
            .await
            .expect("the service starts");
        prepare(&service);
        let address = service.local_addr().expect("a bound address");
        let (stop, stopped) = oneshot::channel::<()>();
        let serving = tokio::spawn(service.serve(async move {
            stopped.await.map_err(io::Error::other)?;
            Ok(StopSignal::Terminate)
        }));
        Self {
            address,
            stop,
            serving,
        }
    }

    /// Sends SIGTERM's stand-in and waits for the service to stop cleanly.
    async fn stop(self) {
        self.stop.send(()).expect("the service waits for the stop");
        time::timeout(STUCK, self.serving)
            .await
            .expect("the service stops within the drain")
            .expect("the service task finishes")
            .expect("the service stops cleanly");
    }
}

/// A memory log on a fixed clock.
fn memory_log() -> EventLog {
    EventLog::memory(Clock::fixed(Timestamp::from_millis(0)))
}

/// The `event` names of `log`'s lines, in order.
fn event_names(log: &EventLog) -> Vec<String> {
    log.lines()
        .iter()
        .map(|line| {
            let line: Value = serde_json::from_str(line).expect("each line is JSON");
            line["event"].as_str().expect("an event name").to_owned()
        })
        .collect()
}

/// Connects to `address` and sends `request`.
async fn send(address: SocketAddr, request: &str) -> TcpStream {
    use tokio::io::AsyncWriteExt as _;

    let mut stream = TcpStream::connect(address)
        .await
        .expect("the service accepts");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("the request is sent");
    stream
}

/// Everything `stream` receives until the service closes it, or what it received before a reset.
async fn read_to_close(stream: &mut TcpStream) -> String {
    let mut received = Vec::new();
    let mut buffer = [0_u8; 4096];
    let reading = async {
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => received.extend_from_slice(&buffer[..read]),
            }
        }
    };
    time::timeout(STUCK, reading)
        .await
        .expect("the service closes the connection");
    String::from_utf8(received).expect("the answer is UTF-8")
}

/// Reads from `stream` until what it received holds `needle`.
async fn read_until(stream: &mut TcpStream, needle: &str) -> String {
    let mut received = String::new();
    let mut buffer = [0_u8; 4096];
    while !received.contains(needle) {
        let read = time::timeout(STUCK, stream.read(&mut buffer))
            .await
            .expect("the service answers")
            .expect("the stream reads");
        assert!(
            read > 0,
            "the connection closed before {needle:?}: {received}"
        );
        received.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
    received
}

/// Adds alice to the service's database and signs her in, returning her `Cookie` header value.
async fn sign_in_alice(directory: &Directory, address: SocketAddr) -> String {
    let sql = Connection::open(directory.path.join("data/spareline.db")).expect("the database");
    sql.execute(
        "INSERT INTO users (name, password_hash, created_at, password_changed_at) VALUES ('alice', ?, ?, ?)",
        (PASSWORD_HASH, START, START),
    )
    .expect("alice is added");
    let body = serde_json::json!({ "name": "alice", "password": PASSWORD }).to_string();
    let mut stream = send(
        address,
        &format!(
            "POST /api/v1/auth/login HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
    .await;
    let answer = read_to_close(&mut stream).await;
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    answer
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .and_then(|cookie| cookie.split(';').next())
        .expect("a session cookie")
        .to_owned()
}

#[test]
fn the_service_starts_in_the_old_order_answers_and_stops() {
    test_app::run(async {
        let directory = Directory::new();
        let log = memory_log();
        let running = Running::start(directory.config(0), &log).await;
        let mut stream = send(
            running.address,
            "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;
        let answer = read_to_close(&mut stream).await;
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert!(answer.ends_with("{\"ok\":false}"), "{answer}");
        running.stop().await;
        let names = event_names(&log);
        let migrations = names
            .iter()
            .filter(|name| *name == "migration-applied")
            .count();
        assert_eq!(migrations, 3, "{names:?}");
        let order: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|name| ["no-users", "listening", "stopping"].contains(name))
            .collect();
        assert_eq!(order, ["no-users", "listening", "stopping"]);
        let lines = log.lines();
        let line = |event: &str| -> Value {
            let raw = lines
                .iter()
                .find(|line| line.contains(&format!("\"event\":\"{event}\"")))
                .unwrap_or_else(|| panic!("a {event} line"));
            serde_json::from_str(raw).expect("the line is JSON")
        };
        assert_eq!(
            line("no-users")["hint"],
            "create one with: spareline users add NAME"
        );
        let listening_raw = lines
            .iter()
            .find(|line| line.contains("\"event\":\"listening\""))
            .expect("listening");
        assert_eq!(
            test_app::ordered_keys(listening_raw, &[]),
            [
                "at", "event", "url", "adb", "data", "web", "policy", "version"
            ],
            "the listening line keeps spareline-old's field order"
        );
        let listening = line("listening");
        assert_eq!(listening["adb"], "127.0.0.1:1");
        assert_eq!(listening["policy"], Value::Null);
        assert_eq!(listening["version"], "0.1.0");
        assert_eq!(line("stopping")["signal"], "SIGTERM");
    });
}

#[test]
fn a_second_instance_that_cannot_bind_the_port_never_opens_the_database() {
    test_app::run(async {
        let first_directory = Directory::new();
        let first = Running::start(first_directory.config(0), &memory_log()).await;
        let second_directory = Directory::new();
        let second_config = second_directory.config(first.address.port());
        let database = second_config.database_path();
        let second_log = memory_log();
        let refused = Service::start(second_config, "0.1.0".to_owned(), second_log.clone()).await;
        assert!(
            matches!(refused, Err(ServiceError::Listen { .. })),
            "{refused:?}"
        );
        assert!(
            !database.exists(),
            "the second instance created {}",
            database.display()
        );
        assert_eq!(
            event_names(&second_log),
            Vec::<String>::new(),
            "no migration, no-users or listening line"
        );
        first.stop().await;
    });
}

#[test]
fn stopping_ends_open_event_streams_and_answers_pending_long_polls_at_once() {
    test_app::run(async {
        let directory = Directory::new();
        let running = Running::start(directory.config(0), &memory_log()).await;
        let cookie = sign_in_alice(&directory, running.address).await;
        let mut events = send(
            running.address,
            &format!("GET /api/v1/events HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\n\r\n"),
        )
        .await;
        let opening = read_until(&mut events, "retry: 3000").await;
        assert!(opening.starts_with("HTTP/1.1 200 OK\r\n"), "{opening}");
        let mut poll = send(
            running.address,
            &format!(
                "GET /api/v1/messages?after=0&wait=30 HTTP/1.1\r\nHost: localhost\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        time::sleep(Duration::from_millis(100)).await;
        let stopping = running.stop();
        let reading = async {
            let rest = read_to_close(&mut events).await;
            let answer = read_to_close(&mut poll).await;
            (rest, answer)
        };
        let ((), (rest, answer)) = future::join(stopping, reading).await;
        assert!(
            rest.ends_with("0\r\n\r\n"),
            "the stream ended with its last chunk rather than being cut: {rest:?}"
        );
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert!(
            answer.ends_with(r#"{"messages":[],"cursor":0}"#),
            "the long poll answered its empty page: {answer}"
        );
    });
}

#[test]
fn a_request_still_open_after_the_drain_is_dropped_before_serve_returns() {
    test_app::run(async {
        let directory = Directory::new();
        let running = Running::start(directory.config(0), &memory_log()).await;
        let mut stuck = send(
            running.address,
            "POST /api/v1/auth/login HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{",
        )
        .await;
        time::sleep(Duration::from_millis(100)).await;
        running.stop().await;
        let answer = time::timeout(Duration::from_millis(100), read_to_close(&mut stuck))
            .await
            .expect("the connection was closed when serve returned");
        assert_eq!(answer, "", "the request never got an answer");
    });
}

#[test]
fn a_client_that_never_finishes_its_headers_is_disconnected_after_30_s() {
    test_app::run(async {
        let directory = Directory::new();
        let running = Running::start(directory.config(0), &memory_log()).await;
        let started = Instant::now();
        let mut slow = send(
            running.address,
            "GET /healthz HTTP/1.1\r\nHost: localhost\r\n",
        )
        .await;
        let answer = read_to_close(&mut slow).await;
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_secs(30) && waited < STUCK,
            "closed after {waited:?}"
        );
        assert!(
            !answer.contains("200 OK"),
            "the request was never answered: {answer}"
        );
        running.stop().await;
    });
}

#[test]
fn a_screen_socket_upgrades_through_the_service_and_closes_with_4001_for_an_unknown_phone() {
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    test_app::run(async {
        let directory = Directory::new();
        let running = Running::start(directory.config(0), &memory_log()).await;
        let cookie = sign_in_alice(&directory, running.address).await;
        let mut request = format!("ws://{}/api/v1/phones/NOPE/screen", running.address)
            .into_client_request()
            .expect("a valid request");
        request.headers_mut().insert(
            "cookie",
            HeaderValue::from_str(&cookie).expect("a header value"),
        );
        let stream = TcpStream::connect(running.address)
            .await
            .expect("the service accepts");
        let (mut socket, response) = tokio_tungstenite::client_async(request, stream)
            .await
            .expect("the upgrade succeeds");
        assert_eq!(response.status().as_u16(), 101);
        let close = time::timeout(STUCK, async {
            while let Some(message) = socket.next().await {
                if let Ok(Message::Close(frame)) = message {
                    return frame;
                }
            }
            None
        })
        .await
        .expect("the socket closes")
        .expect("a close frame with a code");
        assert_eq!(u16::from(close.code), 4001, "{close:?}");
        running.stop().await;
    });
}

#[test]
fn serve_returns_only_once_its_socket_tasks_have_ended() {
    test_app::run(async {
        let directory = Directory::new();
        let (held, released) = oneshot::channel::<()>();
        let mut state = None;
        let running = Running::start_with(directory.config(0), &memory_log(), |service| {
            let shared = service.state().clone();
            shared.spawn_socket(async move {
                let _held = held;
                future::pending::<()>().await;
            });
            state = Some(shared);
        })
        .await;
        let mut released = released;
        running.stop().await;
        assert_eq!(
            released.try_recv(),
            Err(TryRecvError::Closed),
            "the socket's task was dropped before serve returned, though the state lives on"
        );
        drop(state);
    });
}
