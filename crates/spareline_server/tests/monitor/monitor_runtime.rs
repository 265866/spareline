//! The monitor's tests and the setup they share.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant as StdInstant};
use std::{env, fs, process};

use rusqlite::Connection;
use serde_json::{Value, json};
use spareline_adb::AdbClient;
use spareline_core::Timestamp;
use spareline_core::monitor::{
    Alert, AlertKind, CheckReason, CheckReport, LOCK_SCREEN_DEX_PHONE_PATH, LockScreenNotice,
    PhonePolicy, Policy, SIM_STATE_COMMAND, Severity,
};
use spareline_db::{Database, DeviceEvent};
use spareline_server::monitor::{Monitor, MonitorContext, MonitorError, MonitorSettings};
use spareline_server::{
    Buses, Clock, DbHandle, DeviceManager, DeviceOptions, Devices, EventLog, embedded,
};
use tokio::runtime::Builder;
use tokio::sync::broadcast::Receiver;
use tokio::task;
use tokio::time::{self, Instant};

use crate::adb_stand_in::AdbStandIn;
use crate::monitor_phone::{LOG_BUFFER_COMMAND, MonitorPhone, Pushed};

const B30: &str = "SPARELINE00001";
const B30_PROFILE: &str = include_str!("../fixtures/nuu-b30-no-data.json");
const HEALTH_IN_SERVICE: &str = include_str!("../fixtures/nuu-s6702x/health-in-service.txt");
const HEALTH_OUT_OF_SERVICE: &str = include_str!("../fixtures/nuu-s6702x/health.txt");
const PRESET: [&str; 4] = [
    "settings put global mobile_data2 0",
    "settings put global mobile_data3 0",
    "settings put global mobile_data4 0",
    "settings put global mobile_data5 0",
];
const OFF_NOW: [&str; 2] = ["svc data disable", "settings put global mobile_data 0"];
const PLUG_IN: &str = "D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_IN [PHONE1]";
const STATUS_CHANGED: &str =
    "D/RILJ    ( 1937): [UNSL]< UNSOL_RESPONSE_SIM_STATUS_CHANGED [PHONE0]";
const AN_HOUR: Duration = Duration::from_hours(1);

fn block_on<F: Future>(future: F) -> F::Output {
    Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("a current-thread runtime builds")
        .block_on(future)
}

/// Waits until `check` holds, letting the paused clock run in 5 ms steps, at most 10 s of it.
async fn until(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if check() {
            return;
        }
        time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Holds tokio's paused clock still until it is dropped. A blocking task in flight stops a
/// current-thread runtime from advancing a paused clock by itself, so the clock then moves only
/// through `time::advance`. Without it, each moment the runtime waits on the stand-in's real TCP
/// traffic lets the clock jump to the next timer, and under load those jumps carry a test past
/// the window it checks.
struct FrozenClock {
    _release: Sender<()>,
}

impl FrozenClock {
    fn new() -> Self {
        let (release, released) = mpsc::channel::<()>();
        drop(task::spawn_blocking(move || match released.recv() {
            Ok(()) | Err(_) => {}
        }));
        Self { _release: release }
    }
}

/// Waits until `check` holds while the clock is frozen, letting the tasks and the stand-in's
/// traffic run, for at most 10 s of real time.
async fn until_frozen(what: &str, mut check: impl FnMut() -> bool) {
    let began = StdInstant::now();
    while !check() {
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        task::yield_now().await;
    }
}

/// Lets every task that can run, run, while the clock is frozen.
async fn run_what_is_ready() {
    for _ in 0..200 {
        task::yield_now().await;
    }
}

/// A database file of its own for each test, removed when the test ends, so a test can read
/// back the `health_5m` rows, which spareline itself never reads.
struct DataDir {
    path: PathBuf,
}

impl DataDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = env::temp_dir().join(format!(
            "spareline-monitor-test-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Self { path }
    }

    fn database(&self) -> PathBuf {
        self.path.join("spareline.db")
    }

    /// The `health_5m` rows as `bucket samples battery_pct service_json`, oldest first.
    fn health_rows(&self) -> Vec<String> {
        let connection = Connection::open(self.database()).expect("the database opens");
        let mut rows = connection
            .prepare(
                "SELECT bucket || ' ' || samples || ' ' || coalesce(battery_pct, 'null') || ' ' || \
                 service_json FROM health_5m ORDER BY bucket",
            )
            .expect("the query is valid");
        rows.query_map([], |row| row.get(0))
            .expect("the query runs")
            .collect::<Result<Vec<String>, _>>()
            .expect("the rows are text")
    }
}

impl Drop for DataDir {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.path) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// A clock the test moves by hand, apart from tokio's timers.
#[derive(Clone)]
struct TestClock {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the test moves the time that the clock handed to the service reads from \
                      its own tasks"
        )
    )]
    now: Arc<Mutex<Timestamp>>,
}

impl TestClock {
    fn new() -> Self {
        Self {
            now: Arc::new(Mutex::new(
                Timestamp::parse_iso("2026-10-22T08:00:00.000Z").expect("a valid time"),
            )),
        }
    }

    fn clock(&self) -> Clock {
        let now = Arc::clone(&self.now);
        Clock::from_fn(move || *now.lock().expect("the clock's lock isn't poisoned"))
    }

    fn advance(&self, by: Duration) {
        let mut now = self.now.lock().expect("the clock's lock isn't poisoned");
        *now = now.plus(by);
    }
}

fn b30_policy() -> Policy {
    Policy::load("nuu-b30-no-data.json", B30_PROFILE).expect("the example profile loads")
}

/// The B30 profile without its `radiosOff` block: no radio rules, no data watch and no data-off.
fn b30_policy_leaving_radios_alone() -> Policy {
    let text = B30_PROFILE.replacen(
        "\t\"radiosOff\": {\n\t\t\"dataThresholdBytes\": 1000000\n\t},\n",
        "",
        1,
    );
    assert!(!text.contains("radiosOff"), "the block was removed");
    Policy::load("nuu-b30-no-data.json", &text).expect("the edited profile loads")
}

fn b30_policy_with_threshold(bytes: u64) -> Policy {
    let text = B30_PROFILE.replacen(
        "\"dataThresholdBytes\": 1000000",
        &format!("\"dataThresholdBytes\": {bytes}"),
        1,
    );
    Policy::load("nuu-b30-no-data.json", &text).expect("the edited profile loads")
}

/// How a test sets the monitor up.
struct Options {
    policy: Option<Policy>,
    check_interval: Duration,
    sim_poll: Duration,
    sim_debounce: Duration,
    prepare: fn(&MonitorPhone),
    data: Option<DataDir>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            policy: Some(b30_policy()),
            check_interval: AN_HOUR,
            sim_poll: AN_HOUR,
            sim_debounce: Duration::from_millis(5),
            prepare: |_| {},
            data: None,
        }
    }
}

/// A phone manager and a monitor over the B30 stand-in, with the B30 online and its first check
/// done.
struct Setup {
    stand_in: AdbStandIn,
    phone: MonitorPhone,
    manager: DeviceManager,
    devices: Devices,
    monitor: Monitor,
    data: DataDir,
    events: Receiver<DeviceEvent>,
    seen: Vec<DeviceEvent>,
    clock: TestClock,
    log: EventLog,
    at_connect: Vec<String>,
    reads_at_connect: usize,
}

impl Setup {
    /// Brings the B30 online and waits for its first check, or with no policy for it to be
    /// online, with the clock frozen: none of it waits on a timer, and the phone manager's and
    /// the monitor's real TCP traffic then can't let the clock run on to a timeout.
    async fn start(options: Options) -> Self {
        let frozen = FrozenClock::new();
        let clock = TestClock::new();
        let phone = MonitorPhone::recorded();
        if let Some(policy) = &options.policy {
            phone.set_read_script(&policy.read_script);
        }
        let stand_in = AdbStandIn::start(phone.clone()).await;
        let data = options.data.unwrap_or_else(DataDir::new);
        let database = Database::open(&data.database(), || Timestamp::from_millis(0), |_| {})
            .expect("the database opens");
        let db = DbHandle::new(database);
        let buses = Buses::new();
        let events = buses.device_events();
        let log = EventLog::memory(clock.clock());
        let mut device_options = DeviceOptions::new(
            AdbClient::new(stand_in.endpoint()),
            db.clone(),
            buses.clone(),
            "home".to_owned(),
            log.clone(),
            clock.clock(),
            AN_HOUR,
        );
        device_options.identity_retry = Vec::new();
        let mut manager = DeviceManager::new(device_options);
        let devices = manager.devices();
        let has_policy = options.policy.is_some();
        let monitor = Monitor::new(
            MonitorSettings {
                policy: options.policy,
                check_interval: options.check_interval,
                health_interval: AN_HOUR,
                sim_poll: options.sim_poll,
                sim_log_retry: Duration::from_secs(30),
                sim_debounce: options.sim_debounce,
            },
            MonitorContext {
                devices: devices.clone(),
                db: db.clone(),
                buses,
                clock: clock.clock(),
                log: log.clone(),
            },
        );
        (options.prepare)(&phone);
        monitor.start();
        manager.start();
        stand_in.plug_in(B30);
        let mut setup = Self {
            stand_in,
            phone,
            manager,
            devices,
            monitor,
            data,
            events,
            seen: Vec::new(),
            clock,
            log,
            at_connect: Vec::new(),
            reads_at_connect: 0,
        };
        if has_policy {
            let began = StdInstant::now();
            while setup
                .policy()
                .await
                .and_then(|policy| policy.check)
                .is_none()
            {
                assert!(
                    began.elapsed() < Duration::from_secs(10),
                    "timed out waiting for the first check"
                );
                task::yield_now().await;
            }
        } else {
            let devices = setup.devices.clone();
            until_frozen("the B30 online", || {
                devices
                    .get(B30)
                    .is_some_and(|phone| phone.status == spareline_core::PhoneStatus::Online)
            })
            .await;
        }
        setup.settled().await;
        setup.at_connect = setup.phone.take_applied();
        setup.reads_at_connect = setup.phone.take_reads();
        drop(frozen);
        setup
    }

    async fn settled(&self) {
        self.manager.settled().await;
        self.monitor.settled().await;
        self.manager.settled().await;
    }

    async fn policy(&self) -> Option<PhonePolicy> {
        self.monitor.status().await.policy(B30).cloned()
    }

    async fn check(&self) -> CheckReport {
        self.monitor
            .check(B30)
            .await
            .expect("the B30 is online and the profile loaded")
    }

    /// The events published so far, in order.
    fn events(&mut self) -> &[DeviceEvent] {
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event);
        }
        &self.seen
    }

    /// The details of the events of `kind`.
    fn details(&mut self, kind: &str) -> Vec<Value> {
        self.events()
            .iter()
            .filter(|event| event.kind == kind)
            .map(|event| serde_json::from_str(event.detail.get()).expect("details are JSON"))
            .collect()
    }

    /// The log lines named `event`, without their time.
    fn log_lines(&self, event: &str) -> Vec<Value> {
        self.log
            .lines()
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).expect("log lines are JSON"))
            .filter(|line| line["event"] == event)
            .map(|mut line| {
                if let Some(object) = line.as_object_mut() {
                    object.remove("at");
                }
                line
            })
            .collect()
    }

    fn alerts(&self) -> Vec<Alert> {
        self.monitor.alerts()
    }

    fn has_alert(&self, kind: AlertKind) -> bool {
        self.alerts().iter().any(|alert| alert.kind == kind)
    }

    fn identity_reads(&self) -> usize {
        self.phone.count(&spareline_core::identity_command())
    }

    async fn wait_for_log_stream(&self) {
        let mut opened = self.phone.log_streams_opened();
        let waited = time::timeout(
            Duration::from_secs(10),
            opened.wait_for(|opened| *opened >= 1),
        )
        .await;
        assert!(matches!(waited, Ok(Ok(_))), "the radio log stream opened");
    }

    async fn unplug(&self) {
        self.stand_in.unplug();
        let devices = self.devices.clone();
        until("the B30 offline", || {
            devices
                .get(B30)
                .is_some_and(|phone| phone.status != spareline_core::PhoneStatus::Online)
        })
        .await;
    }

    async fn stop(self) -> DataDir {
        self.monitor.stop().await;
        self.manager.stop().await;
        self.data
    }
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|&text| text.to_owned()).collect()
}

/// An alert's key and severity.
#[derive(Debug, PartialEq, Eq)]
struct KeyAndSeverity {
    key: String,
    severity: Severity,
}

fn keys_and_severities(alerts: &[Alert]) -> Vec<KeyAndSeverity> {
    alerts
        .iter()
        .map(|alert| KeyAndSeverity {
            key: alert.key.clone(),
            severity: alert.severity,
        })
        .collect()
}

fn key_and_severity(key: String, severity: Severity) -> KeyAndSeverity {
    KeyAndSeverity { key, severity }
}

#[test]
fn a_phone_coming_online_is_checked_and_gets_its_log_buffer_and_the_next_sims_data_keys() {
    block_on(async {
        let mut setup = Setup::start(Options::default()).await;
        let mut expected = strings(&PRESET);
        expected.push(LOG_BUFFER_COMMAND.to_owned());
        assert_eq!(setup.at_connect, expected);
        assert_eq!(setup.reads_at_connect, 2, "read, apply, read again");
        let check = setup.policy().await.unwrap().check.unwrap();
        assert_eq!(check.reason, CheckReason::Connect);
        assert_eq!(check.error, None);
        assert_eq!(
            serde_json::to_value(&check.reverted).unwrap(),
            json!([{"rule": "log_buffer", "before": "256 KiB", "after": "8 MiB", "fixed": true}])
        );
        assert!(check.results.iter().all(|result| result.ok));
        assert_eq!(
            setup.details("log_buffer_set"),
            [json!({
                "rule": "log_buffer",
                "before": "256 KiB",
                "after": "8 MiB",
                "fixed": true,
                "reason": "connect",
            })]
        );
        assert_eq!(setup.details("drift"), Vec::<Value>::new());
        assert_eq!(setup.details("alert.raised"), Vec::<Value>::new());
        assert_eq!(setup.alerts(), []);
        assert_eq!(setup.monitor.open_alerts(), 0);
        drop(setup.stop().await);
    });
}

#[test]
fn the_status_holds_the_phones_policy_state_the_threshold_and_the_profile() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        let status = setup.monitor.status().await;
        let phone = status.policy(B30).unwrap();
        assert_eq!(phone.busy, None);
        assert!(!phone.data_alarm);
        assert_eq!(
            phone.provision, None,
            "no Set up phone ran on this database"
        );
        assert_eq!(status.data_threshold_bytes, Some(1_000_000));
        assert!(status.policy("OTHER").is_none());
        let profile = status.profile.unwrap();
        assert_eq!(profile.name, "nuu-b30-no-data");
        assert!(profile.removes_apps);
        drop(setup.stop().await);
    });
}

#[test]
fn a_phone_that_ignores_the_buffer_size_gets_a_quiet_card_item_retried_next_check() {
    block_on(async {
        let mut setup = Setup::start(Options {
            prepare: |phone| phone.stick("logd"),
            ..Options::default()
        })
        .await;
        setup.check().await;
        setup.check().await;
        assert_eq!(
            setup.phone.applied(),
            strings(&[LOG_BUFFER_COMMAND, LOG_BUFFER_COMMAND])
        );
        assert_eq!(setup.details("log_buffer_set"), Vec::<Value>::new());
        assert_eq!(setup.details("drift"), Vec::<Value>::new());
        assert_eq!(
            keys_and_severities(&setup.alerts()),
            [key_and_severity(
                format!("policy:{B30}:log_buffer"),
                Severity::Info
            )]
        );
        assert!(
            setup.alerts()[0].message.ends_with(
                "The phone ignored the buffer size; it will be retried on the next check."
            )
        );
        setup.phone.unstick("logd");
        setup.check().await;
        assert_eq!(setup.phone.applied().len(), 3);
        assert_eq!(setup.alerts(), []);
        assert_eq!(
            setup.details("log_buffer_set"),
            [json!({
                "rule": "log_buffer",
                "before": "256 KiB",
                "after": "8 MiB",
                "fixed": true,
                "reason": "manual",
            })]
        );
        drop(setup.stop().await);
    });
}

#[test]
fn the_server_log_names_a_log_buffer_fix_log_buffer_set_not_drift() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        assert_eq!(setup.log_lines("drift"), Vec::<Value>::new());
        assert_eq!(
            setup.log_lines("log-buffer-set"),
            [json!({
                "event": "log-buffer-set",
                "serial": B30,
                "reason": "connect",
                "rule": "log_buffer",
                "before": "256 KiB",
                "after": "8 MiB",
                "fixed": true,
            })]
        );
        let applies = setup.log_lines("policy-apply");
        let mut commands = strings(&PRESET);
        commands.push(LOG_BUFFER_COMMAND.to_owned());
        assert_eq!(
            applies,
            [json!({"event": "policy-apply", "serial": B30, "commands": commands})]
        );
        drop(setup.stop().await);
    });
}

#[test]
fn first_sight_applies_only_what_the_drift_checker_fixes_safety_first() {
    block_on(async {
        let mut setup = Setup::start(Options {
            prepare: |phone| {
                phone.set("global", "wifi_scan_always_enabled", "1");
                phone.set("system", "screen_off_timeout", "600000");
                phone.set_section("bloat", "package:com.android.vending");
                phone.set_section("smsrole", "com.android.messaging");
            },
            ..Options::default()
        })
        .await;
        let mut expected = strings(&["settings put global wifi_scan_always_enabled 0"]);
        expected.extend(strings(&PRESET));
        expected.push(LOG_BUFFER_COMMAND.to_owned());
        assert_eq!(setup.at_connect, expected);
        assert_eq!(setup.details("provision"), Vec::<Value>::new());
        let failing: Vec<String> = setup
            .policy()
            .await
            .unwrap()
            .check
            .unwrap()
            .results
            .iter()
            .filter(|result| !result.ok)
            .map(|result| result.id.clone())
            .collect();
        assert_eq!(failing, ["screen_timeout", "sms_role", "debloat"]);
        drop(setup.stop().await);
    });
}

#[test]
fn radio_drift_is_reverted_on_sight_and_logged_with_before_and_after() {
    block_on(async {
        let mut setup = Setup::start(Options::default()).await;
        setup.phone.set("global", "wifi_scan_always_enabled", "1");
        setup.phone.set("secure", "location_mode", "3");
        let report = setup.check().await;
        assert_eq!(
            setup.phone.applied(),
            strings(&[
                "settings put global wifi_scan_always_enabled 0",
                "settings put secure location_mode 0",
            ])
        );
        assert_eq!(
            setup
                .phone
                .get("global", "wifi_scan_always_enabled")
                .as_deref(),
            Some("0")
        );
        assert_eq!(
            serde_json::to_value(&report.reverted).unwrap(),
            json!([
                {"rule": "wifi_scan_off", "before": "1", "after": "0", "fixed": true},
                {"rule": "location_off", "before": "3", "after": "0", "fixed": true},
            ])
        );
        assert_eq!(
            setup.details("drift"),
            [
                json!({"rule": "wifi_scan_off", "before": "1", "after": "0", "fixed": true, "reason": "manual"}),
                json!({"rule": "location_off", "before": "3", "after": "0", "fixed": true, "reason": "manual"}),
            ]
        );
        assert_eq!(setup.phone.take_reads(), 2, "read, apply, read again");
        assert_eq!(
            setup.log_lines("drift"),
            [json!({
                "event": "drift",
                "serial": B30,
                "reason": "manual",
                "reverted": [
                    {"rule": "wifi_scan_off", "before": "1", "after": "0", "fixed": true},
                    {"rule": "location_off", "before": "3", "after": "0", "fixed": true},
                ],
            })]
        );
        drop(setup.stop().await);
    });
}

#[test]
fn other_drift_raises_an_alert_and_changes_nothing() {
    block_on(async {
        let mut setup = Setup::start(Options::default()).await;
        setup.phone.set("system", "screen_brightness", "200");
        setup
            .phone
            .set("global", "ota_disable_automatic_update", "0");
        setup.check().await;
        assert_eq!(setup.phone.applied(), Vec::<String>::new());
        let raised: Vec<Value> = setup
            .details("alert.raised")
            .into_iter()
            .map(|detail| detail["key"].clone())
            .collect();
        assert_eq!(
            raised,
            [
                json!(format!("policy:{B30}:ota_off")),
                json!(format!("policy:{B30}:brightness_low")),
            ]
        );
        let raised_event = setup
            .events()
            .iter()
            .find(|event| event.kind == "alert.raised")
            .cloned()
            .unwrap();
        assert_eq!(
            raised_event.serial.as_deref(),
            Some(B30),
            "the event is the phone's"
        );
        setup.phone.set("system", "screen_brightness", "10");
        setup.check().await;
        assert_eq!(
            setup.details("alert.cleared"),
            [json!({"key": format!("policy:{B30}:brightness_low"), "kind": "policy"})]
        );
        drop(setup.stop().await);
    });
}

#[test]
fn a_radio_that_will_not_turn_off_raises_a_critical_alert() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.phone.set("global", "bluetooth_on", "1");
        setup.phone.stick("global:bluetooth_on");
        let report = setup.check().await;
        assert_eq!(
            serde_json::to_value(&report.reverted).unwrap(),
            json!([{"rule": "bluetooth_off", "before": "1", "after": "1", "fixed": false}])
        );
        let alert = setup
            .alerts()
            .into_iter()
            .find(|alert| alert.kind == AlertKind::DriftUnfixed)
            .unwrap();
        assert_eq!(alert.key, format!("drift_unfixed:{B30}:bluetooth_off"));
        assert_eq!(alert.severity, Severity::Critical);
        drop(setup.stop().await);
    });
}

#[test]
fn a_check_whose_commands_fail_records_the_error_and_raises_an_alert() {
    block_on(async {
        let mut setup = Setup::start(Options::default()).await;
        setup.phone.set("global", "wifi_on", "1");
        setup.phone.fail_apply(Some("device offline"));
        let report = setup.check().await;
        let error = report.error.clone().unwrap();
        assert!(error.ends_with("device offline"), "{error}");
        assert_eq!(
            setup.policy().await.unwrap().check.unwrap().error,
            Some(error.clone())
        );
        let alert = setup
            .alerts()
            .into_iter()
            .find(|alert| alert.kind == AlertKind::CheckFailed)
            .unwrap();
        assert_eq!(alert.key, format!("check_failed:{B30}"));
        assert_eq!(alert.message, format!("Policy check failed: {error}"));
        assert_eq!(setup.details("drift"), Vec::<Value>::new());
        assert_eq!(
            setup.log_lines("policy-check-failed"),
            [
                json!({"event": "policy-check-failed", "serial": B30, "reason": "manual", "error": error})
            ]
        );
        setup.phone.fail_apply(None);
        let next = setup.check().await;
        assert_eq!(next.error, None);
        assert!(!setup.has_alert(AlertKind::CheckFailed));
        drop(setup.stop().await);
    });
}

#[test]
fn a_read_with_no_settings_is_a_failed_check_not_drift() {
    block_on(async {
        let mut setup = Setup::start(Options::default()).await;
        setup.phone.set_section("settings:global", "");
        let report = setup.check().await;
        assert_eq!(
            report.error.as_deref(),
            Some("policy read returned no global settings")
        );
        assert_eq!(setup.phone.applied(), Vec::<String>::new());
        assert_eq!(setup.details("drift"), Vec::<Value>::new());
        assert!(setup.has_alert(AlertKind::CheckFailed));
        drop(setup.stop().await);
    });
}

#[test]
fn scrcpys_stay_awake_is_left_alone_while_a_screen_session_is_active() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        let sessions = setup.monitor.screen_sessions();
        sessions.started(B30);
        setup.phone.set("global", "stay_on_while_plugged_in", "3");
        setup.check().await;
        setup.monitor.provision(B30).await.unwrap();
        assert!(
            !setup
                .phone
                .applied()
                .contains(&"settings put global stay_on_while_plugged_in 0".to_owned())
        );
        let stay_on = |alerts: &[Alert]| {
            alerts
                .iter()
                .any(|alert| alert.key.ends_with(":stay_on_off"))
        };
        assert!(!stay_on(&setup.alerts()));
        sessions.ended(B30);
        setup.check().await;
        assert!(stay_on(&setup.alerts()));
        drop(setup.stop().await);
    });
}

#[test]
fn stay_awake_raises_no_alert_while_the_session_runs_and_is_never_changed_by_a_check() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        let sessions = setup.monitor.screen_sessions();
        let stay_on = |alerts: &[Alert]| {
            alerts
                .iter()
                .any(|alert| alert.key.ends_with(":stay_on_off"))
        };
        sessions.started(B30);
        setup.phone.set("global", "stay_on_while_plugged_in", "7");
        setup.check().await;
        assert!(!stay_on(&setup.alerts()));
        assert_eq!(setup.phone.applied(), Vec::<String>::new());
        sessions.ended(B30);
        setup.phone.set("global", "stay_on_while_plugged_in", "0");
        setup.check().await;
        assert!(!stay_on(&setup.alerts()));
        setup.phone.set("global", "stay_on_while_plugged_in", "7");
        setup.check().await;
        assert!(stay_on(&setup.alerts()), "left on after the session");
        assert_eq!(setup.phone.applied(), Vec::<String>::new());
        drop(setup.stop().await);
    });
}

#[test]
fn set_up_applies_every_enforceable_rule_once_and_a_second_run_changes_nothing() {
    block_on(async {
        let mut setup = Setup::start(Options::default()).await;
        setup.phone.set("global", "wifi_on", "1");
        setup.phone.set("system", "screen_off_timeout", "600000");
        setup.phone.big_radio_buffer();
        setup
            .phone
            .set_section("datasaver", "Restrict background status: disabled");
        setup.phone.set_section(
            "bloat",
            "package:com.android.vending\npackage:com.google.android.youtube",
        );
        let first = setup.monitor.provision(B30).await.unwrap();
        assert!(first.ok);
        assert_eq!(first.error, None);
        assert_eq!(
            setup.phone.applied(),
            strings(&[
                "svc wifi disable",
                "settings put system screen_off_timeout 15000",
                "cmd netpolicy set restrict-background true",
                "pm uninstall --user 0 com.android.vending",
                "pm uninstall --user 0 com.google.android.youtube",
            ])
        );
        let rules: Vec<&str> = first.steps.iter().map(|step| step.rule.as_str()).collect();
        assert_eq!(
            rules,
            ["wifi_off", "screen_timeout", "data_saver", "debloat"]
        );
        let check = setup.policy().await.unwrap().check.unwrap();
        assert_eq!(check.reason, CheckReason::Manual);
        assert_eq!(check.read_ms, 0);
        let second = setup.monitor.provision(B30).await.unwrap();
        assert!(second.ok);
        assert_eq!(second.steps, []);
        assert_eq!(setup.phone.applied().len(), 5);
        let changed: Vec<Value> = setup
            .details("provision")
            .into_iter()
            .map(|detail| detail["changed"].clone())
            .collect();
        assert_eq!(
            changed,
            [
                json!(["wifi_off", "screen_timeout", "data_saver", "debloat"]),
                json!([]),
            ]
        );
        assert_eq!(setup.alerts(), []);
        assert_eq!(
            setup.log_lines("provision")[0],
            json!({
                "event": "provision",
                "serial": B30,
                "ok": true,
                "changed": ["wifi_off", "screen_timeout", "data_saver", "debloat"],
                "error": null,
            })
        );

        setup.unplug().await;
        setup.stand_in.plug_in(B30);
        let monitor = setup.monitor.clone();
        let mut reconnected = false;
        for _ in 0..2000 {
            let phone = monitor.status().await.policy(B30).cloned().unwrap();
            if phone
                .check
                .is_some_and(|check| check.reason == CheckReason::Connect)
                && phone.busy.is_none()
            {
                reconnected = true;
                break;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
        assert!(reconnected, "a reconnect checks");
        setup.settled().await;
        assert_eq!(
            setup.details("provision").len(),
            2,
            "it doesn't set up again"
        );
        drop(setup.stop().await);
    });
}

#[test]
fn the_last_set_up_survives_a_restart_through_its_event() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.phone.set("global", "wifi_on", "1");
        let first = setup.monitor.provision(B30).await.unwrap();
        let data = setup.stop().await;

        let restarted = Setup::start(Options {
            data: Some(data),
            ..Options::default()
        })
        .await;
        let provision = restarted.policy().await.unwrap().provision.unwrap();
        assert_eq!(provision.at, first.at);
        assert!(provision.ok);
        assert_eq!(provision.duration_ms, first.duration_ms);
        assert_eq!(provision.error, None);
        assert_eq!(provision.steps.len(), 1);
        assert_eq!(provision.steps[0].rule, "wifi_off");
        assert_eq!(provision.steps[0].commands, Vec::<String>::new());
        assert_eq!(provision.results, []);
        drop(restarted.stop().await);
    });
}

#[test]
fn a_changed_lock_screen_is_an_alert_until_set_up_pushes_the_program_and_draws_the_notice_again() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup
            .phone
            .set("secure", "lock_screen_show_notifications", "1");
        setup.phone.set_lock_wallpaper(None);
        setup.check().await;
        assert_eq!(setup.phone.applied(), Vec::<String>::new());
        assert_eq!(setup.phone.pushes(), []);
        assert_eq!(
            keys_and_severities(&setup.alerts()),
            [
                key_and_severity(
                    format!("policy:{B30}:lock_screen_notifications_off"),
                    Severity::Warning
                ),
                key_and_severity(format!("policy:{B30}:lock_screen_notice"), Severity::Info),
            ]
        );
        let report = setup.monitor.provision(B30).await.unwrap();
        assert!(report.ok);
        assert_eq!(report.error, None);
        let rules: Vec<&str> = report.steps.iter().map(|step| step.rule.as_str()).collect();
        assert_eq!(
            rules,
            ["lock_screen_notifications_off", "lock_screen_notice"]
        );
        assert_eq!(
            setup.phone.pushes(),
            [Pushed {
                path: LOCK_SCREEN_DEX_PHONE_PATH.to_owned(),
                size: embedded::LOCK_SCREEN_DEX.len(),
            }]
        );
        assert_eq!(
            setup.phone.applied(),
            [
                "settings put secure lock_screen_show_notifications 0".to_owned(),
                LockScreenNotice::default().command(),
            ]
        );
        assert_eq!(setup.alerts(), []);
        setup.monitor.provision(B30).await.unwrap();
        assert_eq!(setup.phone.pushes().len(), 1, "already drawn");
        assert_eq!(setup.phone.applied().len(), 2);
        drop(setup.stop().await);
    });
}

#[test]
fn a_failed_push_skips_only_the_notice_and_the_radio_fix_still_runs() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.phone.set("global", "wifi_on", "1");
        setup.phone.set_lock_wallpaper(None);
        setup.phone.leave_file(LOCK_SCREEN_DEX_PHONE_PATH);
        setup.phone.fail_push(Some("No space left on device"));
        let report = setup.monitor.provision(B30).await.unwrap();
        assert_eq!(setup.phone.applied(), strings(&["svc wifi disable"]));
        assert_eq!(setup.phone.get("global", "wifi_on").as_deref(), Some("0"));
        assert!(!report.ok);
        assert_eq!(report.steps[0].rule, "wifi_off");
        assert_eq!(report.steps[0].failed, []);
        assert_eq!(report.steps[1].rule, "lock_screen_notice");
        let failed = &report.steps[1].failed;
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].command, LockScreenNotice::default().command());
        let push_error = format!("push {LOCK_SCREEN_DEX_PHONE_PATH}: No space left on device");
        assert!(
            failed[0].output.starts_with(&format!(
                "not run: pushing {LOCK_SCREEN_DEX_PHONE_PATH} failed: "
            )) && failed[0].output.ends_with(&push_error),
            "{}",
            failed[0].output
        );
        let failing: Vec<&str> = report
            .results
            .iter()
            .filter(|result| !result.ok)
            .map(|result| result.id.as_str())
            .collect();
        assert_eq!(failing, ["lock_screen_notice"]);
        let lines = setup.log_lines("policy-push-failed");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["serial"], B30);
        assert_eq!(lines[0]["path"], LOCK_SCREEN_DEX_PHONE_PATH);
        assert!(lines[0]["error"].as_str().unwrap().ends_with(&push_error));
        drop(setup.stop().await);
    });
}

#[test]
fn internet_data_above_the_threshold_turns_data_off_again_and_turns_the_phone_red() {
    block_on(async {
        let mut setup = Setup::start(Options {
            policy: Some(b30_policy_with_threshold(1_000_000)),
            ..Options::default()
        })
        .await;
        setup.phone.add_metered_traffic(1_500_000, 10101);
        let report = setup.check().await;
        assert_eq!(
            serde_json::to_value(&report.usage).unwrap(),
            json!([{
                "subId": 1,
                "slot": 0,
                "rx": 1_500_000,
                "tx": 0,
                "uids": [{"uid": 10101, "bytes": 1_500_000}],
                "over": true,
            }])
        );
        assert_eq!(
            setup.phone.applied(),
            strings(&[
                "svc data disable",
                "settings put global mobile_data 0",
                "settings put global mobile_data1 0",
            ])
        );
        assert_eq!(
            setup.details("data_threshold"),
            [json!({
                "subId": 1,
                "slot": 0,
                "rx": 1_500_000,
                "tx": 0,
                "thresholdBytes": 1_000_000,
                "uids": [{"uid": 10101, "bytes": 1_500_000}],
                "dataOff": true,
            })]
        );
        assert!(setup.policy().await.unwrap().data_alarm);
        assert!(setup.has_alert(AlertKind::DataThreshold));
        setup.check().await;
        assert_eq!(setup.phone.applied().len(), 6, "data is turned off again");
        assert_eq!(setup.details("data_threshold").len(), 1, "logged once");
        drop(setup.stop().await);
    });
}

#[test]
fn mms_and_video_call_traffic_of_the_phone_process_does_not_count() {
    block_on(async {
        let setup = Setup::start(Options {
            policy: Some(b30_policy_with_threshold(1_000)),
            ..Options::default()
        })
        .await;
        setup.phone.add_metered_traffic(5_000_000, 1001);
        let report = setup.check().await;
        assert_eq!(report.usage, []);
        assert_eq!(setup.phone.applied(), Vec::<String>::new());
        assert!(!setup.policy().await.unwrap().data_alarm);
        drop(setup.stop().await);
    });
}

#[test]
fn a_new_sim_gets_data_off_at_once_then_its_own_key_then_a_verified_check_and_an_inventory_read() {
    block_on(async {
        let mut setup = Setup::start(Options::default()).await;
        setup.wait_for_log_stream().await;
        let identity_before = setup.identity_reads();
        let calls_before = setup.phone.calls().len();
        setup.phone.insert_sim(9);
        setup.phone.log_line(PLUG_IN);
        let mut secured = Vec::new();
        for _ in 0..2000 {
            secured = setup.details("sim.secured");
            if !secured.is_empty() {
                break;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
        setup.settled().await;
        assert_eq!(
            secured,
            [json!({"slot": 1, "subId": 9, "dataOff": true, "mmsWithDataOff": false})]
        );
        let mut expected = strings(&OFF_NOW);
        expected.extend(strings(&[
            "svc data disable",
            "settings put global mobile_data 0",
            "settings put global mobile_data9 0",
        ]));
        assert_eq!(setup.phone.applied()[..5], expected[..]);
        let read_script = b30_policy().read_script;
        let sequence: Vec<&str> = setup.phone.calls()[calls_before..]
            .iter()
            .filter_map(|call| {
                if *call == read_script {
                    Some("read")
                } else if call.contains("@@sl:run:") {
                    Some("apply")
                } else if call == "dumpsys isub" {
                    Some("isub")
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(sequence[..4], ["apply", "isub", "apply", "read"]);
        assert_eq!(
            setup.phone.get("global", "mobile_data9").as_deref(),
            Some("0")
        );
        let mms = setup
            .alerts()
            .into_iter()
            .find(|alert| alert.key.ends_with(":mms_with_data_off"))
            .unwrap();
        assert_eq!(mms.severity, Severity::Critical);
        let phone = setup.phone.clone();
        until("an identity refresh", || {
            phone.count(&spareline_core::identity_command()) > identity_before
        })
        .await;
        let signal = setup.log_lines("sim-signal");
        assert_eq!(
            signal,
            [json!({"event": "sim-signal", "serial": B30, "reason": "logcat", "changes": 1})]
        );
        drop(setup.stop().await);
    });
}

#[test]
fn the_data_off_on_a_sim_signal_does_not_wait_for_queued_work() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.wait_for_log_stream().await;
        let hold = setup.phone.hold();
        let monitor = setup.monitor.clone();
        let stuck = tokio::spawn(async move { monitor.check(B30).await });
        time::sleep(Duration::from_millis(5)).await;
        setup.phone.let_new_calls_through();
        setup.phone.insert_sim(9);
        setup.phone.log_line(STATUS_CHANGED);
        let phone = setup.phone.clone();
        until("the immediate data-off", || phone.applied().len() >= 2).await;
        assert_eq!(setup.phone.applied(), strings(&OFF_NOW));
        hold.send_replace(false);
        stuck.await.unwrap().unwrap();
        setup.settled().await;
        drop(setup.stop().await);
    });
}

#[test]
fn a_failed_data_off_for_a_new_sim_raises_a_critical_alert_until_data_is_off() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.wait_for_log_stream().await;
        setup.phone.fail_apply(Some("device offline"));
        setup.phone.insert_sim(9);
        setup.phone.log_line(PLUG_IN);
        let monitor = setup.monitor.clone();
        until("sim_unsecured", || {
            monitor
                .alerts()
                .iter()
                .any(|alert| alert.kind == AlertKind::SimUnsecured)
        })
        .await;
        for _ in 0..2000 {
            let reason = setup
                .policy()
                .await
                .and_then(|policy| policy.check)
                .map(|check| check.reason);
            if reason == Some(CheckReason::Sim) {
                break;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
        setup.settled().await;
        let alert = setup
            .alerts()
            .into_iter()
            .find(|alert| alert.kind == AlertKind::SimUnsecured)
            .unwrap();
        assert_eq!(alert.severity, Severity::Critical);
        assert_eq!(alert.key, format!("sim_unsecured:{B30}"));
        assert!(
            alert
                .message
                .starts_with("A SIM changed and turning mobile data off failed: "),
            "{}",
            alert.message
        );
        let check = setup.policy().await.unwrap().check.unwrap();
        assert_eq!(check.reason, CheckReason::Sim);
        assert!(check.error.unwrap().ends_with("device offline"));
        let failed = setup.log_lines("data-off-failed");
        assert_eq!(failed[0]["cause"], "sim");

        setup.phone.fail_apply(None);
        setup.check().await;
        assert!(!setup.has_alert(AlertKind::SimUnsecured));
        assert_eq!(
            setup.phone.get("global", "mobile_data9").as_deref(),
            Some("0")
        );
        drop(setup.stop().await);
    });
}

#[test]
fn the_sim_state_poll_catches_a_change_the_radio_log_missed() {
    block_on(async {
        let mut setup = Setup::start(Options {
            sim_poll: Duration::from_millis(10),
            ..Options::default()
        })
        .await;
        let phone = setup.phone.clone();
        until("a poll", || phone.count(SIM_STATE_COMMAND) > 0).await;
        setup.phone.insert_sim(9);
        let mut secured = Vec::new();
        for _ in 0..2000 {
            secured = setup.details("sim.secured");
            if !secured.is_empty() {
                break;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(secured.len(), 1);
        assert_eq!(setup.log_lines("sim-signal")[0]["reason"], "sim_state");
        drop(setup.stop().await);
    });
}

#[test]
fn a_removed_sim_gets_only_the_immediate_data_off_and_re_reads_inventory() {
    block_on(async {
        let mut setup = Setup::start(Options::default()).await;
        setup.wait_for_log_stream().await;
        let before = setup.identity_reads();
        setup.phone.remove_sim();
        setup
            .phone
            .log_line("D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_OUT [PHONE0]");
        let phone = setup.phone.clone();
        until("an identity refresh", || {
            phone.count(&spareline_core::identity_command()) > before
        })
        .await;
        setup.settled().await;
        assert_eq!(setup.phone.applied(), strings(&OFF_NOW));
        assert_eq!(setup.details("sim.secured"), Vec::<Value>::new());
        drop(setup.stop().await);
    });
}

#[test]
fn the_radio_log_stream_is_reopened_30_s_after_it_ends() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.wait_for_log_stream().await;
        let frozen = FrozenClock::new();
        let mut opened = setup.phone.log_streams_opened();
        opened.borrow_and_update();
        let ended = Instant::now();
        setup.phone.end_log_streams();
        let phone = setup.phone.clone();
        until_frozen("the monitor to close the ended stream", || {
            phone.log_streams_let_go() == 1
        })
        .await;
        time::advance(Duration::from_secs(30) - Duration::from_millis(1)).await;
        run_what_is_ready().await;
        assert_eq!(*opened.borrow(), 1, "not reopened before 30 s");
        time::advance(Duration::from_millis(1)).await;
        until_frozen("the reopen", || *opened.borrow() == 2).await;
        assert_eq!(ended.elapsed(), Duration::from_secs(30));
        drop(frozen);
        drop(setup.stop().await);
    });
}

#[test]
fn health_keeps_5_minute_aggregates_and_raises_the_offline_alert() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.monitor.sample_health().await;
        setup.clock.advance(Duration::from_mins(5));
        setup.monitor.sample_health().await;
        assert_eq!(
            setup.data.health_rows(),
            ["2026-10-22T08:00:00.000Z 1 85 {\"0\":0}"]
        );

        setup.unplug().await;
        setup.monitor.sample_health().await;
        assert!(!setup.has_alert(AlertKind::Offline));
        setup.clock.advance(Duration::from_mins(6));
        setup.monitor.sample_health().await;
        let kinds: Vec<AlertKind> = setup.alerts().iter().map(|alert| alert.kind).collect();
        assert_eq!(kinds, [AlertKind::Offline]);
        drop(setup.stop().await);
    });
}

#[test]
fn health_raises_the_hot_battery_alert_above_45_c_and_clears_it_below_43_c() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        let at = async |deci_c: u32| {
            setup.phone.set_health(&HEALTH_OUT_OF_SERVICE.replacen(
                "temperature: 221",
                &format!("temperature: {deci_c}"),
                1,
            ));
            setup.manager.tick();
            setup.manager.settled().await;
            setup.monitor.sample_health().await;
            setup
                .alerts()
                .into_iter()
                .find(|alert| alert.kind == AlertKind::BatteryHot)
        };
        assert_eq!(at(450).await, None);
        assert_eq!(
            at(468).await.unwrap().message,
            "Battery at 46.8 °C (over 45 °C)."
        );
        assert_eq!(
            at(440).await.unwrap().message,
            "Battery at 44.0 °C (over 45 °C)."
        );
        assert_eq!(at(429).await, None);
        drop(setup.stop().await);
    });
}

#[test]
fn health_alerts_after_30_minutes_out_of_service_and_three_in_service_samples_clear_it() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        let sample = async |text: &str, minutes: u64| {
            setup.phone.set_health(text);
            setup.manager.tick();
            setup.manager.settled().await;
            setup.clock.advance(Duration::from_mins(minutes));
            setup.monitor.sample_health().await;
            setup.has_alert(AlertKind::OutOfService)
        };
        assert!(!sample(HEALTH_OUT_OF_SERVICE, 0).await);
        assert!(sample(HEALTH_OUT_OF_SERVICE, 31).await);
        assert!(sample(HEALTH_IN_SERVICE, 1).await);
        assert!(sample(HEALTH_IN_SERVICE, 1).await);
        assert!(
            sample(HEALTH_OUT_OF_SERVICE, 1).await,
            "one sample out of service restarts the count, not the spell"
        );
        assert!(sample(HEALTH_IN_SERVICE, 1).await);
        assert!(sample(HEALTH_IN_SERVICE, 1).await);
        assert!(!sample(HEALTH_IN_SERVICE, 1).await);
        drop(setup.stop().await);
    });
}

#[test]
fn a_sim_whose_data_off_failed_keeps_its_alert_while_the_phone_is_offline() {
    block_on(async {
        let mut setup = Setup::start(Options::default()).await;
        setup.wait_for_log_stream().await;
        setup.phone.fail_apply(Some("device offline"));
        setup.phone.insert_sim(9);
        setup.phone.log_line(PLUG_IN);
        for _ in 0..2000 {
            let reason = setup
                .policy()
                .await
                .and_then(|policy| policy.check)
                .map(|check| check.reason);
            if reason == Some(CheckReason::Sim) {
                break;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
        setup.settled().await;
        setup.unplug().await;
        setup.monitor.sample_health().await;
        let alert = setup
            .alerts()
            .into_iter()
            .find(|alert| alert.kind == AlertKind::SimUnsecured)
            .unwrap();
        assert_eq!(alert.key, format!("sim_unsecured:{B30}"));
        assert_eq!(alert.severity, Severity::Critical);
        let cleared: Vec<Value> = setup
            .details("alert.cleared")
            .into_iter()
            .filter(|detail| detail["kind"] == "sim_unsecured")
            .collect();
        assert_eq!(cleared, Vec::<Value>::new());
        drop(setup.stop().await);
    });
}

#[test]
fn a_sim_signal_during_a_running_data_off_gets_a_data_off_of_its_own() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.wait_for_log_stream().await;
        let hold = setup.phone.hold();
        setup.phone.log_line(STATUS_CHANGED);
        time::sleep(Duration::from_millis(1)).await;
        setup.phone.log_line(STATUS_CHANGED);
        time::sleep(Duration::from_millis(1)).await;
        hold.send_replace(false);
        let phone = setup.phone.clone();
        until("the second data-off", || phone.applied().len() >= 4).await;
        setup.settled().await;
        let mut expected = strings(&OFF_NOW);
        expected.extend(strings(&OFF_NOW));
        assert_eq!(setup.phone.applied(), expected);
        drop(setup.stop().await);
    });
}

#[test]
fn a_check_that_read_before_a_data_off_failed_does_not_clear_it() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.wait_for_log_stream().await;
        let mut held = setup.phone.hold_next_read();
        let monitor = setup.monitor.clone();
        let stale = tokio::spawn(async move { monitor.check(B30).await });
        held.taken().await;
        setup.phone.fail_apply(Some("device offline"));
        setup.phone.log_line(PLUG_IN);
        let monitor = setup.monitor.clone();
        until("sim_unsecured", || {
            monitor
                .alerts()
                .iter()
                .any(|alert| alert.kind == AlertKind::SimUnsecured)
        })
        .await;
        setup.phone.fail_apply(None);
        held.release();
        let stale = stale.await.unwrap().unwrap();
        let mobile = stale
            .results
            .iter()
            .find(|result| result.id == "mobile_data")
            .unwrap();
        assert!(mobile.ok);
        assert!(
            setup.has_alert(AlertKind::SimUnsecured),
            "the check read before the failure"
        );
        setup.settled().await;
        setup.check().await;
        assert!(
            !setup.has_alert(AlertKind::SimUnsecured),
            "a check that started after the failure and finds data off clears it"
        );
        drop(setup.stop().await);
    });
}

#[test]
fn a_failed_data_off_for_data_over_the_threshold_says_so() {
    block_on(async {
        let setup = Setup::start(Options {
            policy: Some(b30_policy_with_threshold(1_000_000)),
            ..Options::default()
        })
        .await;
        setup.phone.add_metered_traffic(1_500_000, 10101);
        setup.phone.fail_apply(Some("device offline"));
        setup.check().await;
        let message = setup
            .alerts()
            .into_iter()
            .find(|alert| alert.kind == AlertKind::SimUnsecured)
            .unwrap()
            .message;
        assert!(
            message.starts_with(
                "Mobile data went over the threshold and turning mobile data off failed: "
            ) && message.ends_with("device offline"),
            "{message}"
        );
        drop(setup.stop().await);
    });
}

#[test]
fn the_periodic_check_runs_every_interval_and_skips_a_busy_phone() {
    block_on(async {
        let period = Duration::from_secs(30);
        let setup = Setup::start(Options {
            check_interval: period,
            ..Options::default()
        })
        .await;
        let frozen = FrozenClock::new();
        let read_script = b30_policy().read_script;
        let reads_before = setup.phone.count(&read_script);
        time::advance(period).await;
        let phone = setup.phone.clone();
        until_frozen("the interval check's read", || {
            phone.count(&read_script) > reads_before
        })
        .await;
        setup.settled().await;
        let check = setup.policy().await.unwrap().check.unwrap();
        assert_eq!(check.reason, CheckReason::Interval);
        assert_eq!(setup.phone.take_reads(), 1);

        let hold = setup.phone.hold();
        let monitor = setup.monitor.clone();
        let busy = tokio::spawn(async move { monitor.check(B30).await });
        let began = StdInstant::now();
        while setup
            .policy()
            .await
            .and_then(|policy| policy.busy)
            .is_none()
        {
            assert!(
                began.elapsed() < Duration::from_secs(10),
                "the manual check marks the phone busy"
            );
            task::yield_now().await;
        }
        time::advance(period).await;
        run_what_is_ready().await;
        hold.send_replace(false);
        let report = busy.await.unwrap().unwrap();
        assert_eq!(report.reason, CheckReason::Manual);
        assert_eq!(
            report.error, None,
            "the held read finished within its timeout"
        );
        setup.settled().await;
        assert_eq!(
            setup.phone.take_reads(),
            1,
            "no interval check was queued while the phone was busy"
        );
        drop(frozen);
        drop(setup.stop().await);
    });
}

#[test]
fn without_a_policy_nothing_is_read_or_changed_set_up_is_refused_and_health_alerts_still_rise() {
    block_on(async {
        let setup = Setup::start(Options {
            policy: None,
            ..Options::default()
        })
        .await;
        assert_eq!(setup.reads_at_connect, 0);
        assert_eq!(setup.at_connect, Vec::<String>::new());
        assert_eq!(setup.policy().await.and_then(|policy| policy.check), None);
        assert_eq!(setup.monitor.data_threshold_bytes(), None);
        assert_eq!(setup.monitor.status().await.profile, None);
        assert_eq!(
            setup.monitor.check(B30).await.unwrap_err(),
            MonitorError::NoPolicy
        );
        assert_eq!(
            setup.monitor.provision(B30).await.unwrap_err(),
            MonitorError::NoPolicy
        );

        setup.wait_for_log_stream().await;
        let before = setup.identity_reads();
        setup.phone.insert_sim(9);
        setup.phone.log_line(PLUG_IN);
        let phone = setup.phone.clone();
        until("an identity refresh", || {
            phone.count(&spareline_core::identity_command()) > before
        })
        .await;
        setup.settled().await;
        assert_eq!(setup.phone.applied(), Vec::<String>::new());
        assert_eq!(setup.phone.take_reads(), 0);
        let calls = setup.phone.calls();
        assert!(
            calls
                .iter()
                .all(|call| !call.contains("settings put") && !call.contains("svc ")),
            "nothing changes a setting: {calls:?}"
        );
        assert_eq!(setup.phone.pushes(), [], "nothing is pushed");

        setup.unplug().await;
        setup.clock.advance(Duration::from_mins(6));
        setup.monitor.sample_health().await;
        let kinds: Vec<AlertKind> = setup.alerts().iter().map(|alert| alert.kind).collect();
        assert_eq!(kinds, [AlertKind::Offline]);
        drop(setup.stop().await);
    });
}

#[test]
fn a_phone_that_is_not_online_is_refused() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        assert_eq!(
            setup.monitor.check("OTHER").await.unwrap_err(),
            MonitorError::NotOnline {
                serial: "OTHER".to_owned()
            }
        );
        setup.unplug().await;
        assert_eq!(
            setup.monitor.provision(B30).await.unwrap_err().to_string(),
            format!("phone {B30} is not online")
        );
        drop(setup.stop().await);
    });
}

#[test]
fn stopping_writes_the_partial_health_buckets() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        setup.monitor.sample_health().await;
        assert_eq!(setup.data.health_rows(), Vec::<String>::new());
        let data = setup.stop().await;
        assert_eq!(
            data.health_rows(),
            ["2026-10-22T08:00:00.000Z 1 85 {\"0\":0}"]
        );
    });
}

#[test]
fn health_buckets_older_than_90_days_are_pruned_at_the_first_sample_then_once_a_day() {
    block_on(async {
        let setup = Setup::start(Options::default()).await;
        let old = |bucket: &str| {
            let connection = Connection::open(setup.data.database()).unwrap();
            connection
                .execute(
                    "INSERT INTO health_5m (phone_id, bucket, samples, signal_json, service_json) \
                     SELECT id, ?, 1, '{}', '{}' FROM phones WHERE serial = ?",
                    (bucket, B30),
                )
                .unwrap();
        };
        old("2026-07-01T00:00:00.000Z");
        setup.monitor.sample_health().await;
        assert_eq!(
            setup.data.health_rows(),
            Vec::<String>::new(),
            "pruned at the first sample"
        );

        old("2026-07-01T00:00:00.000Z");
        setup.clock.advance(Duration::from_hours(23));
        setup.monitor.sample_health().await;
        assert_eq!(
            setup.data.health_rows(),
            [
                "2026-07-01T00:00:00.000Z 1 null {}",
                "2026-10-22T08:00:00.000Z 1 85 {\"0\":0}"
            ],
            "not again within a day"
        );
        setup
            .clock
            .advance(Duration::from_hours(1) + Duration::from_millis(1));
        setup.monitor.sample_health().await;
        assert_eq!(
            setup.data.health_rows(),
            [
                "2026-10-22T08:00:00.000Z 1 85 {\"0\":0}",
                "2026-10-23T07:00:00.000Z 1 85 {\"0\":0}"
            ],
            "pruned again a day after the last prune"
        );
        drop(setup.stop().await);
    });
}

#[test]
fn without_radios_off_a_new_sim_gets_no_data_off_and_no_sim_secured() {
    block_on(async {
        let mut setup = Setup::start(Options {
            policy: Some(b30_policy_leaving_radios_alone()),
            ..Options::default()
        })
        .await;
        assert_eq!(
            setup.at_connect,
            strings(&[LOG_BUFFER_COMMAND]),
            "no preset data keys"
        );
        setup.wait_for_log_stream().await;
        setup.phone.insert_sim(9);
        setup.phone.log_line(PLUG_IN);
        for _ in 0..2000 {
            let reason = setup
                .policy()
                .await
                .and_then(|policy| policy.check)
                .map(|check| check.reason);
            if reason == Some(CheckReason::Sim) {
                break;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
        setup.settled().await;
        assert_eq!(
            setup.policy().await.unwrap().check.unwrap().reason,
            CheckReason::Sim,
            "the SIM change ran a check"
        );
        setup.check().await;
        let data_off: Vec<String> = setup
            .phone
            .applied()
            .into_iter()
            .filter(|command| command == "svc data disable" || command.contains("mobile_data"))
            .collect();
        assert_eq!(
            data_off,
            Vec::<String>::new(),
            "no data-off reached the phone"
        );
        assert_eq!(setup.details("sim.secured"), Vec::<Value>::new());
        assert!(!setup.has_alert(AlertKind::SimUnsecured));
        assert_eq!(setup.log_lines("data-off-failed"), Vec::<Value>::new());
        drop(setup.stop().await);
    });
}

#[test]
fn without_radios_off_a_failed_check_after_a_new_sim_raises_no_data_off_alert() {
    block_on(async {
        let setup = Setup::start(Options {
            policy: Some(b30_policy_leaving_radios_alone()),
            ..Options::default()
        })
        .await;
        setup.wait_for_log_stream().await;
        setup.phone.insert_sim(9);
        setup.phone.set_section("settings:global", "");
        setup.phone.log_line(PLUG_IN);
        for _ in 0..2000 {
            let reason = setup
                .policy()
                .await
                .and_then(|policy| policy.check)
                .map(|check| check.reason);
            if reason == Some(CheckReason::Sim) {
                break;
            }
            time::sleep(Duration::from_millis(5)).await;
        }
        setup.settled().await;
        let check = setup.policy().await.unwrap().check.unwrap();
        assert_eq!(check.reason, CheckReason::Sim, "the SIM change ran a check");
        assert_eq!(
            check.error.as_deref(),
            Some("policy read returned no global settings")
        );
        assert!(setup.has_alert(AlertKind::CheckFailed));
        assert!(
            !setup.has_alert(AlertKind::SimUnsecured),
            "data never had to go off, so its failure can't be the reason"
        );
        assert_eq!(setup.log_lines("data-off-failed"), Vec::<Value>::new());
        drop(setup.stop().await);
    });
}

#[test]
fn an_unplugged_phones_sim_watch_stops_without_logging_a_failure() {
    block_on(async {
        let poll = Duration::from_secs(10);
        let setup = Setup::start(Options {
            sim_poll: poll,
            ..Options::default()
        })
        .await;
        time::sleep(poll * 2).await;
        assert!(
            setup.phone.count(SIM_STATE_COMMAND) >= 2,
            "the poll runs every 10 s while the phone is online"
        );
        setup.unplug().await;
        setup.settled().await;
        let polled = setup.phone.count(SIM_STATE_COMMAND);
        let streams = *setup.phone.log_streams_opened().borrow();
        time::sleep(Duration::from_secs(95)).await;
        assert_eq!(
            setup.phone.count(SIM_STATE_COMMAND),
            polled,
            "no poll after the unplug"
        );
        assert_eq!(
            *setup.phone.log_streams_opened().borrow(),
            streams,
            "the log stream isn't reopened"
        );
        assert_eq!(
            setup.log_lines("sim-log-failed"),
            Vec::<Value>::new(),
            "the stream ending with the phone's session is no failure, and a stopped watch \
             doesn't retry"
        );
        drop(setup.stop().await);
    });
}

#[test]
fn two_quick_sim_signals_read_isub_once_a_second_after_the_last() {
    block_on(async {
        let setup = Setup::start(Options {
            sim_debounce: Duration::from_secs(1),
            ..Options::default()
        })
        .await;
        setup.wait_for_log_stream().await;
        let frozen = FrozenClock::new();
        let phone = setup.phone.clone();
        let applied = phone.applied().len();
        setup.phone.log_line(STATUS_CHANGED);
        until_frozen("the first signal's data-off", || {
            phone.applied().len() >= applied + OFF_NOW.len()
        })
        .await;
        setup.monitor.settled().await;
        time::advance(Duration::from_millis(500)).await;
        setup.phone.log_line(STATUS_CHANGED);
        until_frozen("the second signal's data-off", || {
            phone.applied().len() >= applied + 2 * OFF_NOW.len()
        })
        .await;
        setup.monitor.settled().await;
        time::advance(Duration::from_millis(999)).await;
        run_what_is_ready().await;
        assert_eq!(
            setup.phone.count("dumpsys isub"),
            0,
            "the second signal restarted the debounce"
        );
        time::advance(Duration::from_millis(1)).await;
        until_frozen("the debounced isub read", || {
            phone.count("dumpsys isub") == 1
        })
        .await;
        setup.settled().await;
        time::advance(Duration::from_secs(2)).await;
        run_what_is_ready().await;
        setup.settled().await;
        assert_eq!(
            setup.phone.count("dumpsys isub"),
            1,
            "one read for the burst"
        );
        drop(frozen);
        drop(setup.stop().await);
    });
}
