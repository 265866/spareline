//! What the screen tests share: a phone online through [`Devices`], a database with the phone's
//! row, the [`Screens`] under test, and viewers that read their queues without a socket.

use std::path::PathBuf;
use std::time::Duration;
use std::{env, fs, io};

use rusqlite::Connection;
use serde_json::Value;
use spareline_db::{Database, PhoneState, PhoneStateUpdate};
use tokio::runtime::Builder;
use tokio::sync::mpsc;

use crate::clock::Clock;
use crate::db_handle::DbHandle;
use crate::devices::{Devices, PhoneEvent};
use crate::event_log::EventLog;
use crate::fake_adb::{B30, START};
use crate::monitor::ScreenSessions;
use crate::screen::fake_scrcpy::FakePhone;
use crate::screen::screen_command::ScreenCommand;
use crate::screen::screen_timings::ScreenTimings;
use crate::screen::screens::{Screens, ViewerHandle};
use crate::screen::viewer_close::ViewerClose;
use crate::screen::viewer_frame::ViewerFrame;

/// Runs `future` on a current-thread runtime whose clock runs in real time, for a test that
/// moves megabytes through loopback sockets and asserts no timing: delivering them takes real
/// time, which a paused clock outruns.
pub(crate) fn block_on_real_time<F: Future>(future: F) -> F::Output {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime builds")
        .block_on(future)
}

/// The jar the tests push; the stand-in only counts pushes.
pub(crate) const JAR: &[u8] = &[1, 2, 3];

/// The wake command as the stand-in logs it.
pub(crate) const WAKE: &str = "exec:input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard";

/// The timings spareline-old's manager tests used: a short idle time and restart budget, and
/// no stats or wake checks unless a test asks.
pub(crate) fn test_timings() -> ScreenTimings {
    ScreenTimings {
        idle: Duration::from_millis(100),
        restart_delays: vec![Duration::from_millis(10), Duration::from_millis(10)],
        stats_interval: Duration::from_mins(1),
        wake_check: Duration::from_mins(1),
        connect_timeout: Duration::from_millis(500),
        ..ScreenTimings::default()
    }
}

/// A screen relay over a stand-in phone, with everything a test inspects.
pub(crate) struct Harness {
    pub(crate) phone: FakePhone,
    pub(crate) devices: Devices,
    pub(crate) screens: Screens,
    pub(crate) log: EventLog,
    /// The monitor's screen sessions the screens feed.
    pub(crate) monitor_sessions: ScreenSessions,
    dir: PathBuf,
}

impl Harness {
    /// The B30 online, with a phone row, and screens with `timings`.
    pub(crate) async fn start(timings: ScreenTimings) -> Self {
        Self::with_clock(timings, Clock::tokio(START)).await
    }

    /// As [`Harness::start`], with the wall clock `clock`.
    pub(crate) async fn with_clock(timings: ScreenTimings, clock: Clock) -> Self {
        let phone = FakePhone::start().await;
        let devices = Devices::new(phone.client(), clock.clone());
        online(&devices, B30);
        let dir = temporary_dir();
        let database_clock = clock.clone();
        let mut database = Database::open(
            &dir.join("spareline.db"),
            move || database_clock.now(),
            |_| {},
        )
        .unwrap();
        database
            .inventory()
            .record_phone_state(&PhoneStateUpdate {
                serial: B30.to_owned(),
                state: PhoneState::Device,
                site: "test".to_owned(),
                at: clock.now(),
                raw_state: None,
            })
            .unwrap();
        let log = EventLog::memory(clock.clone());
        let monitor_sessions = ScreenSessions::new();
        let screens = Screens::with_timings(
            devices.clone(),
            DbHandle::new(database),
            clock,
            log.clone(),
            monitor_sessions.clone(),
            timings,
            JAR,
        );
        Self {
            phone,
            devices,
            screens,
            log,
            monitor_sessions,
            dir,
        }
    }

    /// A viewer of the B30 signed in as `login`.
    pub(crate) async fn watch(&self, login: &str) -> Watcher {
        let handle = self
            .screens
            .join(B30, login.to_owned())
            .await
            .expect("the phone is online");
        Watcher::new(handle)
    }

    /// The phone goes offline, as the phone manager reports an unplug.
    pub(crate) fn unplug(&self) {
        let now = self.devices.clock().now();
        {
            let mut table = self.devices.table();
            table.apply_state(B30, PhoneState::Offline, None, now);
            table.end_session(B30);
        }
        self.phone.unplug();
        self.devices.emit(PhoneEvent::Disconnected {
            serial: B30.to_owned(),
        });
    }

    /// The phone goes offline in the phone manager's table without an event reaching the
    /// screens, as when an event is missed.
    pub(crate) fn go_offline_quietly(&self) {
        let now = self.devices.clock().now();
        let mut table = self.devices.table();
        table.apply_state(B30, PhoneState::Offline, None, now);
        table.end_session(B30);
    }

    /// Breaks the `control_sessions` table, so recording a viewer fails.
    pub(crate) fn break_audits(&self) {
        let connection = Connection::open(self.dir.join("spareline.db")).unwrap();
        connection
            .execute("DROP TABLE control_sessions", [])
            .unwrap();
    }

    /// The `control_sessions` rows, as `login inputs texts rejected ended`, with `ended` either
    /// `open` or `closed`.
    pub(crate) fn audits(&self) -> Vec<String> {
        let connection = Connection::open(self.dir.join("spareline.db")).unwrap();
        let mut statement = connection
            .prepare(
                "SELECT login, inputs, texts, rejected, ended_at IS NULL FROM control_sessions \
                 ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                let login: Option<String> = row.get(0)?;
                let inputs: i64 = row.get(1)?;
                let texts: i64 = row.get(2)?;
                let rejected: i64 = row.get(3)?;
                let open: bool = row.get(4)?;
                let ended = if open { "open" } else { "closed" };
                Ok(format!(
                    "{} {inputs} {texts} {rejected} {ended}",
                    login.unwrap_or_default()
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The events the screens logged with the name `event`, each as its JSON object.
    pub(crate) fn events(&self, event: &str) -> Vec<Value> {
        self.log
            .lines()
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|line| line["event"] == event)
            .collect()
    }

    /// How many times the stand-in ran the wake command.
    pub(crate) fn wakes(&self) -> usize {
        self.phone
            .log()
            .iter()
            .filter(|entry| *entry == WAKE)
            .count()
    }

    /// The pkills of one session's server by its id.
    pub(crate) fn scid_kills(&self) -> usize {
        self.phone
            .kills()
            .iter()
            .filter(|kill| kill.contains("scid="))
            .count()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.dir) {
            assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
        }
    }
}

/// Puts `serial` online with a session, as the phone manager does.
pub(crate) fn online(devices: &Devices, serial: &str) {
    let now = devices.clock().now();
    let mut table = devices.table();
    table.apply_state(serial, PhoneState::Device, Some("1-1"), now);
    let (probes, _ignored) = mpsc::unbounded_channel();
    table.start_session(serial, probes).unwrap();
}

fn temporary_dir() -> PathBuf {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).unwrap();
    let path = env::temp_dir().join(format!(
        "spareline-screens-test-{:016x}",
        u64::from_le_bytes(bytes)
    ));
    fs::create_dir(&path).unwrap();
    path
}

/// A viewer as spareline-old's `FakeSocket` saw it: every text and binary frame in order, and
/// the close. It reads its queue on [`Watcher::pump`], writing each frame at once, so its
/// backlog stays empty.
pub(crate) struct Watcher {
    handle: ViewerHandle,
    pub(crate) text: Vec<Value>,
    pub(crate) binary: Vec<Vec<u8>>,
    pub(crate) order: Vec<String>,
    pub(crate) closed: Option<ViewerClose>,
}

impl Watcher {
    pub(crate) fn new(handle: ViewerHandle) -> Self {
        Self {
            handle,
            text: Vec::new(),
            binary: Vec::new(),
            order: Vec::new(),
            closed: None,
        }
    }

    /// Reads every frame queued so far.
    pub(crate) fn pump(&mut self) {
        while let Some(frame) = self.handle.outbox.try_next() {
            self.handle.outbox.written(&frame);
            match frame {
                ViewerFrame::Text(text) => {
                    let message: Value = serde_json::from_str(&text).unwrap();
                    self.order.push(message["t"].as_str().unwrap().to_owned());
                    self.text.push(message);
                }
                ViewerFrame::Binary(bytes) => {
                    self.binary.push(bytes.to_vec());
                    self.order.push("binary".to_owned());
                }
                ViewerFrame::Close(close) => self.closed = Some(close),
            }
        }
    }

    /// The `hello` frames.
    pub(crate) fn hellos(&self) -> Vec<Value> {
        self.text
            .iter()
            .filter(|message| message["t"] == "hello")
            .cloned()
            .collect()
    }

    /// Sends a control message, as a binary frame from the browser.
    pub(crate) fn control(&self, message: &[u8]) {
        self.handle
            .relay
            .send(ScreenCommand::Control {
                viewer: self.handle.id,
                message: message.to_vec(),
            })
            .unwrap();
    }

    /// Leaves, as a closed socket does.
    pub(crate) fn detach(&self) {
        self.handle
            .relay
            .send(ScreenCommand::Leave {
                viewer: self.handle.id,
            })
            .unwrap();
    }
}
