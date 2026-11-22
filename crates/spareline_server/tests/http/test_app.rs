//! The real router over a real database in a temporary directory, for the route tests.
//!
//! Every test runs on a current-thread runtime with tokio's clock paused, and the service's
//! clock follows it, so a test moves time with `tokio::time::advance` instead of sleeping.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::{self, Formatter};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::{env, fs, io, process};

use axum::Router;
use axum::body::{self, Body, Bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::Response;
use rusqlite::Connection;
use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::Value;
use serde_json::value::RawValue;
use spareline_adb::{AdbClient, Endpoint};
use spareline_core::Timestamp;
use spareline_db::{Database, NewEvent};
use spareline_server::monitor::{Monitor, MonitorContext, MonitorSettings};
use spareline_server::{
    AppParts, AppState, Buses, Clock, Config, DbHandle, DeviceManager, DeviceOptions, EventLog,
    Screens, http,
};
use tokio::runtime::Builder;
use tokio::time;

use crate::b30_stand_in::{B30, B30StandIn};

/// When every test's clock starts.
pub(crate) const START: &str = "2026-11-22T00:00:00.000Z";

/// The password of every user [`TestApp::add_user`] adds.
pub(crate) const PASSWORD: &str = "a long test password";

/// [`PASSWORD`] hashed by `Bun.password.hash` with argon2id at 8 KiB and one pass, so a test
/// sign-in costs microseconds; the verifier reads the parameters from the hash, as Bun's did.
pub(crate) const PASSWORD_HASH: &str = "$argon2id$v=19$m=8,t=1,p=1$B6V2WX9ifDRRMSvvqGXERAEynwGZ+THQSvBwj/d0u60$N/hVmX23Iu03HluMjcodbMS2kLR38PKbQdbJ65+cgxA";

/// Tells apart the directories of tests that run at once in this process.
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// Runs `future` on a current-thread runtime with tokio's clock paused.
pub(crate) fn run<F: Future>(future: F) -> F::Output {
    Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("a current-thread runtime builds")
        .block_on(future)
}

/// Whether a test's monitor runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Monitoring {
    /// It watches the phones.
    Running,
    /// It was built but not started.
    Stopped,
}

/// The app under test: its router and state, and a second connection to its database for the
/// test's own reads and writes.
pub(crate) struct TestApp {
    pub(crate) router: Router,
    pub(crate) state: AppState,
    pub(crate) sql: Connection,
    pub(crate) web_dir: PathBuf,
    directory: PathBuf,
    manager: DeviceManager,
    _stand_in: Option<B30StandIn>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.directory) {
            assert!(
                error.kind() == io::ErrorKind::NotFound,
                "cannot remove {}: {error}",
                self.directory.display()
            );
        }
    }
}

/// A response with its body read.
pub(crate) struct TestResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

impl TestResponse {
    /// The body as JSON.
    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "the body is JSON ({error}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// The body as text.
    pub(crate) fn text(&self) -> String {
        String::from_utf8(self.body.to_vec()).expect("the body is UTF-8")
    }

    /// The header `name`, or `None`.
    pub(crate) fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .map(|value| value.to_str().expect("a text header").to_owned())
    }

    /// The `name=value` part of the `Set-Cookie` header, to send back as `Cookie`.
    pub(crate) fn session(&self) -> String {
        let cookie = self.header("set-cookie").expect("a Set-Cookie header");
        cookie.split(';').next().expect("a cookie value").to_owned()
    }
}

impl TestApp {
    /// The app with an empty, migrated database, a built web UI, and an adb server nobody
    /// answers at, so no phone is ever online. Call it inside [`run`].
    pub(crate) fn start() -> Self {
        Self::start_with(MonitorSettings::default())
    }

    /// [`TestApp::start`] with the monitor's settings.
    pub(crate) fn start_with(monitor_settings: MonitorSettings) -> Self {
        Self::build(monitor_settings, None)
    }

    /// The app with the B30 online behind a stand-in adb server, its identity and health read
    /// by the real phone manager. The monitor isn't started, so no policy state is shown.
    pub(crate) async fn with_b30() -> Self {
        Self::with_b30_and(MonitorSettings::default(), Monitoring::Stopped).await
    }

    /// [`TestApp::with_b30`] with the monitor's settings, and the monitor running or not.
    pub(crate) async fn with_b30_and(settings: MonitorSettings, monitoring: Monitoring) -> Self {
        let stand_in = B30StandIn::start().await;
        let mut app = Self::build(settings, Some(stand_in));
        if monitoring == Monitoring::Running {
            app.state.monitor().start();
        }
        app.manager.start();
        let devices = app.state.devices().clone();
        for _ in 0..2000 {
            let ready = devices
                .get(B30)
                .is_some_and(|phone| phone.health_at.is_some() && phone.identity_at.is_some());
            if ready {
                app.manager.settled().await;
                return app;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the B30 never came online with its identity and health");
    }

    fn build(monitor_settings: MonitorSettings, stand_in: Option<B30StandIn>) -> Self {
        let directory = env::temp_dir().join(format!(
            "spareline-http-test-{}-{}",
            process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let web_dir = directory.join("web");
        fs::create_dir_all(web_dir.join("assets")).expect("the web directory is created");
        fs::write(
            web_dir.join("index.html"),
            "<!doctype html><div id=root></div>",
        )
        .expect("the page is written");
        fs::write(web_dir.join("assets/main-abc123.js"), "console.log(1)")
            .expect("the asset is written");
        let variables: HashMap<&str, OsString> = HashMap::from([
            ("DATA_DIR", directory.join("data").into_os_string()),
            ("SPARELINE_WEB_DIR", web_dir.clone().into_os_string()),
            ("ADB_SERVER", OsString::from("127.0.0.1:1")),
        ]);
        let config = Config::from_env(|name| variables.get(name).cloned())
            .expect("the test settings are valid");
        let start = Timestamp::parse_iso(START).expect("START is a time");
        let clock = Clock::tokio(start);
        let open_clock = clock.clone();
        let database = Database::open(&config.database_path(), move || open_clock.now(), |_| {})
            .expect("the database opens");
        let sql = Connection::open(config.database_path()).expect("a second connection opens");
        sql.busy_timeout(Duration::from_secs(5))
            .expect("the busy timeout is set");
        let endpoint = stand_in.as_ref().map_or_else(
            || Endpoint::new("127.0.0.1".to_owned(), 1),
            B30StandIn::endpoint,
        );
        let db = DbHandle::new(database);
        let buses = Buses::new();
        let log = EventLog::discard();
        let mut options = DeviceOptions::new(
            AdbClient::new(endpoint),
            db.clone(),
            buses.clone(),
            "home".to_owned(),
            log.clone(),
            clock.clone(),
            config.health_interval,
        );
        options.identity_retry = Vec::new();
        let manager = DeviceManager::new(options);
        let devices = manager.devices();
        let monitor = Monitor::new(
            monitor_settings,
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
        let state = AppState::new(AppParts {
            config,
            version: "test".to_owned(),
            db,
            buses,
            clock,
            log,
            devices,
            monitor,
            screens,
        });
        Self {
            router: http::router(state.clone()),
            state,
            sql,
            web_dir,
            directory,
            manager,
            _stand_in: stand_in,
        }
    }

    /// A phone with SIM 1 in slot 0 on sub 1, bound to `+15125550101` as the SIM reports it.
    pub(crate) fn add_phone(&self) {
        self.sql
            .execute_batch(
                "INSERT INTO phones (id, serial, site, manufacturer, model, android_version, sdk, imei1, imei2, last_state, first_seen_at, last_online_at)
                    VALUES (1, 'SPARELINE00001', 'home', 'NUU', 'S6702X', '14', 34, '350000000000006', '350000000000014', 'device', '2026-10-22T00:00:00.000Z', '2026-10-22T00:00:00.000Z');
                 INSERT INTO sims (id, iccid, carrier, first_seen_at, last_seen_at)
                    VALUES (1, '8901555000000000001', 'T-Mobile', '2026-10-22T00:00:00.000Z', '2026-10-22T00:00:00.000Z');
                 INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (1, 1, 0, 1, '2026-10-22T00:00:00.000Z');
                 INSERT INTO numbers (id, e164, e164_source, created_at) VALUES (1, '+15125550101', 'detected', '2026-10-22T00:00:00.000Z');
                 INSERT INTO number_bindings (number_id, sim_id, from_at, detected_e164) VALUES (1, 1, '2026-10-22T00:00:00.000Z', '+15125550101');",
            )
            .expect("the phone is added");
    }

    /// Adds a user named `name` whose password is [`PASSWORD`], and returns its id.
    pub(crate) fn add_user(&self, name: &str) -> i64 {
        self.sql
            .execute(
                "INSERT INTO users (name, password_hash, created_at, password_changed_at) VALUES (?, ?, ?, ?)",
                (name, PASSWORD_HASH, START, START),
            )
            .expect("the user is added");
        self.sql.last_insert_rowid()
    }

    /// Sends a request. `body`, when given, goes as JSON.
    pub(crate) async fn call(
        &self,
        method: &str,
        path: &str,
        headers: &[[&str; 2]],
        body: Option<&str>,
    ) -> TestResponse {
        let response = self.send(method, path, headers, body).await;
        let status = response.status();
        let headers = response.headers().clone();
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("the body is read");
        TestResponse {
            status,
            headers,
            body,
        }
    }

    /// Sends a request and returns the response with its body unread, for streams.
    pub(crate) async fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[[&str; 2]],
        body: Option<&str>,
    ) -> Response {
        use tower::ServiceExt as _;

        let method: Method = method.parse().expect("a method");
        let mut request = Request::builder().method(method).uri(path);
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        for [name, value] in headers {
            request = request.header(*name, *value);
        }
        let request = request
            .body(body.map_or_else(Body::empty, |text| Body::from(text.to_owned())))
            .expect("the request is valid");
        self.router
            .clone()
            .oneshot(request)
            .await
            .expect("the router answers")
    }

    /// Signs in as `name` with `password`.
    pub(crate) async fn sign_in(&self, name: &str, password: &str) -> TestResponse {
        let body = serde_json::json!({ "name": name, "password": password }).to_string();
        self.call("POST", "/api/v1/auth/login", &[], Some(&body))
            .await
    }

    /// Adds `alice` and signs her in, returning her session cookie.
    pub(crate) async fn alice(&self) -> String {
        self.add_user("alice");
        let response = self.sign_in("alice", PASSWORD).await;
        assert_eq!(response.status, StatusCode::OK, "alice signs in");
        response.session()
    }

    /// Makes an API key through the API with the session `cookie`, and returns it.
    pub(crate) async fn create_key(&self, cookie: &str, name: &str, scope: &str) -> String {
        let body = serde_json::json!({ "name": name, "scope": scope }).to_string();
        let response = self
            .call(
                "POST",
                "/api/v1/api-keys",
                &[["cookie", cookie]],
                Some(&body),
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.text());
        response.json()["key"].as_str().expect("the key").to_owned()
    }

    /// Records `count` device events, all `adb.reachable` with `{"i": n}`, and returns their
    /// ids without publishing them.
    pub(crate) async fn store_events(&self, count: usize, kind: &'static str) -> Vec<i64> {
        let events: Vec<NewEvent> = (0..count)
            .map(|i| {
                NewEvent::try_new(None, kind, &serde_json::json!({ "i": i }))
                    .expect("the detail is JSON")
            })
            .collect();
        let at = Timestamp::parse_iso("2026-10-22T00:00:00.000Z").expect("a time");
        let recorded = self
            .state
            .db()
            .call(move |db| db.device_events().insert(&events, at))
            .await
            .expect("the events are recorded");
        recorded.into_iter().map(|event| event.id).collect()
    }
}

/// The keys of the JSON object at `path` inside the JSON `text`, in the order the text has them.
/// Each step of `path` is an object key, or an array index in digits.
pub(crate) fn ordered_keys(text: &str, path: &[&str]) -> Vec<String> {
    let mut raw: Box<RawValue> = serde_json::from_str(text).expect("the text is JSON");
    for step in path {
        raw = if let Ok(index) = step.parse::<usize>() {
            let mut items: Vec<Box<RawValue>> = serde_json::from_str(raw.get()).expect("an array");
            items.swap_remove(index)
        } else {
            let mut fields: HashMap<String, Box<RawValue>> =
                serde_json::from_str(raw.get()).expect("an object");
            fields.remove(*step).expect("the key is there")
        };
    }
    let KeyOrder(keys) = serde_json::from_str(raw.get()).expect("an object");
    keys
}

/// The keys of a JSON object, in order, with their values skipped.
struct KeyOrder(Vec<String>);

impl<'de> Deserialize<'de> for KeyOrder {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(KeyVisitor)
    }
}

struct KeyVisitor;

impl<'de> Visitor<'de> for KeyVisitor {
    type Value = KeyOrder;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<KeyOrder, A::Error> {
        let mut keys = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            map.next_value::<IgnoredAny>()?;
            keys.push(key);
        }
        Ok(KeyOrder(keys))
    }
}
