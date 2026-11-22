//! The phone manager: the one track-devices stream, the health round, and their shutdown.

use std::collections::HashSet;
use std::time::Duration;

use spareline_db::DatabaseError;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::devices::device_options::DeviceOptions;
use crate::devices::handle::Devices;
use crate::devices::manager_context::ManagerContext;
use crate::devices::probe_reason::ProbeReason;

/// How long reaching the adb server for its version may take before the attempt fails.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// The first wait between attempts to reach the adb server; it doubles with each failure.
const FIRST_BACKOFF: Duration = Duration::from_millis(500);

/// The single owner of device state, as spareline-old's `DeviceManager` was. It holds the one
/// `host:track-devices-l` stream, records each phone's state in the database, opens a session
/// for each phone that comes online and probes its identity and health, and publishes every
/// recorded event after it commits.
///
/// Create it with [`DeviceManager::new`], hand [`DeviceManager::devices`] to the rest of the
/// service, subscribe what follows phones, then [`DeviceManager::start`] it.
#[derive(Debug)]
pub struct DeviceManager {
    context: ManagerContext,
    health_interval: Duration,
    max_backoff: Duration,
    tasks: Vec<JoinHandle<()>>,
}

impl DeviceManager {
    /// A manager that hasn't started. Call it inside the tokio runtime.
    #[must_use]
    pub fn new(options: DeviceOptions) -> Self {
        Self {
            context: ManagerContext {
                devices: Devices::new(options.adb, options.clock),
                db: options.db,
                buses: options.buses,
                site: options.site,
                log: options.log,
                identity_retry: options.identity_retry,
            },
            health_interval: options.health_interval,
            max_backoff: options.max_backoff,
            tasks: Vec::new(),
        }
    }

    /// The handle on the phones this manager fills.
    #[must_use]
    pub fn devices(&self) -> Devices {
        self.context.devices.clone()
    }

    /// Starts following the adb server and the health round. Starting again does nothing.
    pub fn start(&mut self) {
        if !self.tasks.is_empty() {
            return;
        }
        self.tasks
            .push(tokio::spawn(track(self.context.clone(), self.max_backoff)));
        let devices = self.context.devices.clone();
        let interval = self.health_interval;
        self.tasks.push(tokio::spawn(async move {
            let mut ticks = time::interval_at(Instant::now() + interval, interval);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                health_round(&devices);
            }
        }));
    }

    /// Runs the health round now rather than at the next interval: a health probe of every
    /// online phone with no probe queued or running.
    pub fn tick(&self) {
        health_round(&self.context.devices);
    }

    /// Finishes once no phone has a probe queued or running, for tests and shutdown.
    pub async fn settled(&self) {
        self.context.devices.probes_settled().await;
    }

    /// Stops following the adb server and the health round, ends every phone's session, which
    /// cancels its calls, and waits for the probe workers to stop.
    pub async fn stop(self) {
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks {
            match task.await {
                Ok(()) | Err(_) => {}
            }
        }
        let workers = self.context.devices.table().end_all_sessions();
        self.context.devices.probe_done();
        for worker in workers {
            match worker.await {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// Queues a health probe of every online phone with no probe queued or running.
fn health_round(devices: &Devices) {
    let idle = devices.table().idle_online();
    for serial in idle {
        devices.table().schedule(&serial, ProbeReason::Health);
    }
}

/// Follows the adb server's device list until stopped. Each time the server can't be reached
/// or the stream ends, adb is marked unreachable, every phone gone, and the next attempt waits
/// `min(max_backoff, 500 ms * 2^attempt)`; a list that arrives resets the attempts.
async fn track(context: ManagerContext, max_backoff: Duration) {
    let mut attempt: u32 = 0;
    loop {
        let failure = follow(&context, &mut attempt).await;
        if let Err(error) = context.set_adb(false, None, Some(failure)).await {
            record_failed(&context, &error);
        }
        if let Err(error) = context.mark_unseen_gone(&HashSet::new()).await {
            record_failed(&context, &error);
        }
        let factor = 2_u32.checked_pow(attempt).unwrap_or(u32::MAX);
        attempt = attempt.saturating_add(1);
        time::sleep(FIRST_BACKOFF.saturating_mul(factor).min(max_backoff)).await;
    }
}

/// One attempt: the server's version, then its device list until the stream fails. Returns why
/// it stopped.
async fn follow(context: &ManagerContext, attempt: &mut u32) -> String {
    let adb = context.devices.adb_client().clone();
    let version = match adb
        .clone()
        .with_call_timeout(VERSION_TIMEOUT)
        .version()
        .await
    {
        Ok(version) => version,
        Err(error) => return error.to_string(),
    };
    if let Err(error) = context.set_adb(true, Some(version), None).await {
        return error.to_string();
    }
    let mut tracker = match adb.track_devices().await {
        Ok(tracker) => tracker,
        Err(error) => return error.to_string(),
    };
    loop {
        match tracker.next().await {
            Ok(Some(list)) => {
                *attempt = 0;
                if let Err(error) = context.apply_list(&list).await {
                    return error.to_string();
                }
            }
            Ok(None) => return "the adb server closed the device stream".to_owned(),
            Err(error) => return error.to_string(),
        }
    }
}

/// Logs a state the manager couldn't record while handling a lost adb server, which
/// spareline-old let end its tracking loop; here tracking goes on and retries.
fn record_failed(context: &ManagerContext, error: &DatabaseError) {
    context
        .log
        .event("device-record-failed")
        .field("error", &error.to_string())
        .write();
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::Value;
    use spareline_adb::DeviceEntry;
    use spareline_core::PhoneStatus;
    use spareline_db::{Database, DeviceEvent, PhoneState, PhoneStateUpdate};
    use tokio::sync::broadcast::Receiver;

    use super::*;
    use crate::buses::Buses;
    use crate::clock::Clock;
    use crate::db_handle::DbHandle;
    use crate::devices::device_error::DeviceError;
    use crate::devices::phone_event::PhoneEvent;
    use crate::event_log::EventLog;
    use crate::fake_adb::{self, Answer, B30, FakeAdb, Push, ScratchDir};

    const TIMEOUT: Duration = Duration::from_secs(30);

    /// A manager on a B30 stand-in, an in-memory database, and the device events it publishes.
    struct Harness {
        adb: FakeAdb,
        db: DbHandle,
        manager: DeviceManager,
        events: Receiver<DeviceEvent>,
        log: EventLog,
    }

    impl Harness {
        async fn new() -> Self {
            let adb = FakeAdb::b30().await;
            let clock = Clock::tokio(fake_adb::START);
            let database_clock = clock.clone();
            let db = DbHandle::new(
                Database::open_in_memory(move || database_clock.now())
                    .expect("an in-memory database opens"),
            );
            let buses = Buses::new();
            let events = buses.device_events();
            let log = EventLog::memory(clock.clone());
            let options = DeviceOptions::new(
                adb.client(),
                db.clone(),
                buses,
                "home".to_owned(),
                log.clone(),
                clock,
                Duration::from_mins(1),
            );
            Self {
                adb,
                db,
                manager: DeviceManager::new(options),
                events,
                log,
            }
        }

        fn devices(&self) -> Devices {
            self.manager.devices()
        }

        /// The kinds of the device events published since the last call.
        fn kinds(&mut self) -> Vec<String> {
            self.published()
                .into_iter()
                .map(|event| event.kind)
                .collect()
        }

        fn published(&mut self) -> Vec<DeviceEvent> {
            let mut events = Vec::new();
            while let Ok(event) = self.events.try_recv() {
                events.push(event);
            }
            events
        }

        fn isub_calls(&self) -> usize {
            self.adb
                .calls()
                .iter()
                .filter(|call| call.contains("dumpsys isub"))
                .count()
        }

        fn log_events(&self) -> Vec<String> {
            self.log
                .lines()
                .iter()
                .map(|line| {
                    let line: Value = serde_json::from_str(line).unwrap();
                    line["event"].as_str().unwrap().to_owned()
                })
                .collect()
        }

        /// Records a phone the database last saw online, as a previous run would have.
        async fn stored_online(&self, serial: &str) {
            let update = PhoneStateUpdate {
                serial: serial.to_owned(),
                state: PhoneState::Device,
                site: "home".to_owned(),
                at: fake_adb::START,
                raw_state: None,
            };
            self.db
                .call(move |db| db.inventory().record_phone_state(&update).map(|_| ()))
                .await
                .unwrap();
        }

        async fn last_state(&self, serial: &str) -> String {
            let serial = serial.to_owned();
            self.db
                .call(move |db| db.inventory().phone(&serial))
                .await
                .unwrap()
                .unwrap()
                .last_state
        }

        async fn online(&mut self) {
            self.manager.start();
            self.adb.send_list(&fake_adb::recorded_track_lists()[0]);
            let devices = self.devices();
            fake_adb::until("the first probes", || {
                devices.get(B30).is_some_and(|phone| phone.health.is_some())
            })
            .await;
            self.manager.settled().await;
        }
    }

    #[test]
    fn a_phone_coming_online_is_probed_and_recorded() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.online().await;
            let live = harness.devices().get(B30).unwrap();
            assert_eq!(live.status, PhoneStatus::Online);
            assert_eq!(live.adb_state, PhoneState::Device);
            assert_eq!(live.usb.as_deref(), Some("1-1"));
            assert_eq!(live.identity.unwrap().model.as_deref(), Some("S6702X"));
            assert_eq!(live.health.unwrap().battery.unwrap().level, 85);
            assert_eq!(live.identity_at, Some(fake_adb::START));
            let adb = harness.devices().adb();
            assert!(adb.reachable, "{adb:?}");
            assert_eq!(adb.version, Some(41));
            assert_eq!(adb.error, None);
            let published = harness.published();
            let kinds: Vec<&str> = published.iter().map(|event| event.kind.as_str()).collect();
            assert_eq!(
                kinds,
                [
                    "phone.added",
                    "phone.online",
                    "sim.added",
                    "sim.placed",
                    "number.bound"
                ]
            );
            assert_eq!(published[0].serial.as_deref(), Some(B30));
            assert_eq!(
                published[1].detail.get(),
                r#"{"state":"device","rawState":"device"}"#
            );
            let phone = harness
                .db
                .call(|db| db.inventory().phone(B30))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                phone.placements[0].number.as_ref().unwrap().e164.as_deref(),
                Some("+15125550101")
            );
            assert_eq!(phone.model.as_deref(), Some("S6702X"));
            assert_eq!(phone.site, "home");
            let log = harness.log_events();
            assert!(
                !log.contains(&"adb-reachable".to_owned()),
                "reaching adb at startup isn't news: {log:?}"
            );
            assert_eq!(
                log.iter().filter(|event| *event == "phone-state").count(),
                1
            );
            harness.manager.stop().await;
        });
    }

    #[test]
    fn a_recorded_transport_reconnect_yields_one_offline_and_one_online_event() {
        fake_adb::block_on(async {
            let lists = fake_adb::recorded_track_lists();
            let states: Vec<String> = lists
                .iter()
                .map(|list| {
                    let states: Vec<&str> = DeviceEntry::parse_list(list)
                        .iter()
                        .map(|entry| entry.state.as_str())
                        .collect();
                    if states.is_empty() {
                        "none".to_owned()
                    } else {
                        states.join(",")
                    }
                })
                .collect();
            assert_eq!(
                states,
                [
                    "device",
                    "offline",
                    "none",
                    "offline",
                    "authorizing",
                    "offline",
                    "device"
                ]
            );
            let mut harness = Harness::new().await;
            harness.online().await;
            let mut phone_events = harness.devices().subscribe();
            harness.kinds();
            for list in &lists[1..] {
                harness.adb.send_list(list);
            }
            let devices = harness.devices();
            fake_adb::until("back online", || {
                devices.get(B30).is_some_and(|phone| {
                    phone.status == PhoneStatus::Online && phone.health_at.is_some()
                }) && !devices.table().any_probe_pending()
                    && phone_events.len() >= 2
            })
            .await;
            harness.manager.settled().await;
            assert_eq!(harness.kinds(), ["phone.offline", "phone.online"]);
            let mut seen = Vec::new();
            while let Ok(event) = phone_events.try_recv() {
                match event {
                    PhoneEvent::Connected { serial } => seen.push(format!("connected {serial}")),
                    PhoneEvent::Disconnected { serial } => {
                        seen.push(format!("disconnected {serial}"));
                    }
                    PhoneEvent::Ready { .. } | PhoneEvent::SubsChanged { .. } => {}
                }
            }
            assert_eq!(
                seen,
                [format!("disconnected {B30}"), format!("connected {B30}")]
            );
            assert_eq!(
                harness.isub_calls(),
                2,
                "reconnecting probes the phone again"
            );
            harness.manager.stop().await;
        });
    }

    #[test]
    fn an_unauthorized_phone_is_recorded_as_needing_auth_without_probes() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.manager.start();
            harness
                .adb
                .send_list("NEWPHONE\tunauthorized usb:1-2 transport_id:9\n");
            let devices = harness.devices();
            fake_adb::until("the new phone", || devices.get("NEWPHONE").is_some()).await;
            assert_eq!(
                devices.get("NEWPHONE").unwrap().status,
                PhoneStatus::NeedsAuth
            );
            fake_adb::until("its events", || harness.events.len() >= 2).await;
            assert_eq!(harness.kinds(), ["phone.added", "phone.needs_auth"]);
            assert!(harness.adb.calls().is_empty(), "no probe ran");
            let error = devices.exec("NEWPHONE", "id", TIMEOUT).await.unwrap_err();
            assert!(error.to_string().contains("not online"), "{error}");
            harness.manager.stop().await;
        });
    }

    #[test]
    fn losing_the_adb_server_marks_phones_offline_and_reconnects() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.online().await;
            harness.kinds();
            harness.adb.set_reachable(false);
            harness.adb.end_track();
            let devices = harness.devices();
            fake_adb::until("adb down", || !devices.adb().reachable).await;
            fake_adb::until("the phone offline", || {
                devices.get(B30).unwrap().status == PhoneStatus::Offline
            })
            .await;
            assert_eq!(
                devices.adb().error.as_deref(),
                Some("the adb server closed the device stream")
            );
            let calls = harness.adb.version_calls();
            fake_adb::until("retries", || harness.adb.version_calls() > calls + 1).await;
            harness.adb.set_reachable(true);
            harness.adb.send_list(&fake_adb::recorded_track_lists()[0]);
            fake_adb::until("online again", || {
                devices.get(B30).unwrap().status == PhoneStatus::Online
            })
            .await;
            harness.manager.settled().await;
            fake_adb::until("its events", || harness.events.len() >= 4).await;
            assert_eq!(
                harness.kinds(),
                [
                    "adb.unreachable",
                    "phone.offline",
                    "adb.reachable",
                    "phone.online"
                ]
            );
            let log = harness.log_events();
            assert!(log.contains(&"adb-unreachable".to_owned()), "{log:?}");
            assert!(log.contains(&"adb-reachable".to_owned()), "{log:?}");
            harness.manager.stop().await;
        });
    }

    #[test]
    fn backoff_doubles_from_half_a_second_up_to_the_cap() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.adb.set_reachable(false);
            let start = Instant::now();
            harness.manager.start();
            let mut attempts = Vec::new();
            for count in 1..=7 {
                fake_adb::until("the next attempt", || harness.adb.version_calls() >= count).await;
                attempts.push(start.elapsed().as_millis());
            }
            let gaps: Vec<u128> = attempts.windows(2).map(|pair| pair[1] - pair[0]).collect();
            assert_eq!(gaps, [500, 1000, 2000, 4000, 8000, 15_000]);
            harness.manager.stop().await;
        });
    }

    #[test]
    fn a_phone_the_database_last_saw_online_is_marked_offline_when_this_run_does_not_see_it() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.stored_online("OLD").await;
            harness.manager.start();
            harness.adb.send_list("");
            let devices = harness.devices();
            fake_adb::until("the stale phone", || devices.get("OLD").is_some()).await;
            assert_eq!(devices.get("OLD").unwrap().status, PhoneStatus::Offline);
            assert_eq!(harness.last_state("OLD").await, "gone");
            fake_adb::until("its event", || !harness.events.is_empty()).await;
            let published = harness.published();
            assert_eq!(published.len(), 1, "{published:?}");
            assert_eq!(published[0].kind, "phone.offline");
            assert_eq!(published[0].serial.as_deref(), Some("OLD"));
            assert_eq!(published[0].detail.get(), r#"{"state":"gone"}"#);
            harness.manager.stop().await;
        });
    }

    #[test]
    fn a_phone_the_database_last_saw_online_is_offline_when_adb_is_unreachable_at_startup() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.stored_online("OLD").await;
            harness.adb.set_reachable(false);
            harness.manager.start();
            let devices = harness.devices();
            fake_adb::until("the stale phone", || devices.get("OLD").is_some()).await;
            assert!(!devices.adb().reachable, "adb is down");
            assert_eq!(devices.get("OLD").unwrap().status, PhoneStatus::Offline);
            assert_eq!(harness.last_state("OLD").await, "gone");
            fake_adb::until("the events", || harness.events.len() >= 2).await;
            assert_eq!(harness.kinds(), ["adb.unreachable", "phone.offline"]);
            harness.manager.stop().await;
        });
    }

    #[test]
    fn a_sim_state_change_in_the_health_round_reads_the_identity_again() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.online().await;
            let health =
                fake_adb::b30_fixture("health.txt").replace("LOADED,ABSENT", "LOADED,LOADED");
            harness
                .adb
                .answer(&spareline_core::health_command(), &health);
            let before = harness.isub_calls();
            harness.manager.tick();
            harness.manager.settled().await;
            assert_eq!(harness.isub_calls(), before + 1);
            harness.manager.tick();
            harness.manager.settled().await;
            assert_eq!(
                harness.isub_calls(),
                before + 2,
                "the identity still shows LOADED,ABSENT"
            );
            harness.manager.stop().await;
        });
    }

    #[test]
    fn an_unchanged_health_round_reads_the_identity_again_only_after_30_minutes() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.online().await;
            let before = harness.isub_calls();
            harness.manager.tick();
            harness.manager.settled().await;
            assert_eq!(harness.isub_calls(), before);
            time::advance(Duration::from_mins(30)).await;
            fake_adb::until("the health round at 30 min", || {
                harness.isub_calls() == before + 1
            })
            .await;
            harness.manager.settled().await;
            assert_eq!(harness.isub_calls(), before + 1);
            harness.manager.stop().await;
        });
    }

    #[test]
    fn exec_and_push_go_to_online_phones_through_their_session() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.online().await;
            let devices = harness.devices();
            assert_eq!(
                devices.exec(B30, "true", TIMEOUT).await.unwrap(),
                b"exec-ok"
            );
            devices
                .push(B30, &[0; 10], "/data/local/tmp/x", 0o644)
                .await
                .unwrap();
            assert_eq!(
                harness.adb.pushes(),
                [Push {
                    serial: B30.to_owned(),
                    remote: "/data/local/tmp/x".to_owned(),
                    mode: 33188,
                    size: 10,
                }]
            );
            harness.manager.stop().await;
            assert!(
                matches!(
                    devices.exec(B30, "true", TIMEOUT).await,
                    Err(DeviceError::NotOnline { .. })
                ),
                "stopping ends every session"
            );
        });
    }

    #[test]
    fn a_phone_that_comes_back_while_its_last_probe_is_unwinding_is_probed_again() {
        fake_adb::block_on(async {
            let harness = Harness::new().await;
            let mut manager = harness.manager;
            let device = fake_adb::recorded_track_lists().remove(0);
            harness.adb.hold();
            manager.start();
            harness.adb.send_list(&device);
            let devices = manager.devices();
            let status = || devices.get(B30).map(|phone| phone.status);
            fake_adb::until("online", || status() == Some(PhoneStatus::Online)).await;
            fake_adb::until("the identity call", || !harness.adb.calls().is_empty()).await;
            harness.adb.send_list("");
            fake_adb::until("offline", || status() == Some(PhoneStatus::Offline)).await;
            harness.adb.send_list(&device);
            fake_adb::until("online again", || status() == Some(PhoneStatus::Online)).await;
            harness.adb.release();
            fake_adb::until("probed again", || {
                devices.get(B30).is_some_and(|phone| phone.health.is_some())
            })
            .await;
            manager.settled().await;
            let live = devices.get(B30).unwrap();
            assert_eq!(live.identity.unwrap().model.as_deref(), Some("S6702X"));
            assert_eq!(live.health.unwrap().battery.unwrap().level, 85);
            let isub = harness
                .adb
                .calls()
                .iter()
                .filter(|call| call.contains("dumpsys isub"))
                .count();
            assert_eq!(isub, 2);
            manager.stop().await;
        });
    }

    #[test]
    fn an_identity_that_keeps_failing_is_retried_then_the_phone_is_ready_without_it() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.adb.answer_output(
                &spareline_core::identity_command(),
                Answer {
                    stdout: Vec::new(),
                    stderr: b"telephony not ready".to_vec(),
                    exit_code: 2,
                },
            );
            let mut phone_events = harness.devices().subscribe();
            harness.manager.start();
            harness.adb.send_list(&fake_adb::recorded_track_lists()[0]);
            let start = Instant::now();
            let ready = loop {
                match phone_events.recv().await.unwrap() {
                    PhoneEvent::Ready { serial } => break serial,
                    PhoneEvent::Connected { .. }
                    | PhoneEvent::Disconnected { .. }
                    | PhoneEvent::SubsChanged { .. } => {}
                }
            };
            assert_eq!(ready, B30);
            assert_eq!(
                start.elapsed().as_secs(),
                65,
                "5 s, 15 s, then 45 s of retries"
            );
            harness.manager.settled().await;
            let live = harness.devices().get(B30).unwrap();
            assert_eq!(live.identity, None);
            assert_eq!(
                live.health, None,
                "a connect probe without identity stops there"
            );
            assert_eq!(
                live.probe_error.as_deref(),
                Some("probe script exited 2: telephony not ready")
            );
            let retries: Vec<Value> = harness
                .log
                .lines()
                .iter()
                .map(|line| serde_json::from_str(line).unwrap())
                .filter(|line: &Value| line["event"] == "identity-retry")
                .map(|line| line["delayMs"].clone())
                .collect();
            assert_eq!(retries, [5000, 15000, 45000]);
            assert!(harness.log_events().contains(&"probe-failed".to_owned()));
            harness.manager.tick();
            harness.manager.settled().await;
            assert!(
                harness.devices().get(B30).unwrap().health.is_some(),
                "the health round still runs"
            );
            harness.manager.stop().await;
        });
    }

    #[test]
    fn a_failed_health_probe_keeps_its_error_until_one_succeeds() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.online().await;
            harness.adb.set_failing(Some("device offline (stand-in)"));
            harness.manager.tick();
            harness.manager.settled().await;
            let live = harness.devices().get(B30).unwrap();
            assert_eq!(
                live.probe_error.as_deref(),
                Some("shell,v2,raw on SPARELINE00001: device offline (stand-in)")
            );
            assert_eq!(
                live.health.unwrap().battery.unwrap().level,
                85,
                "the last health stays"
            );
            let failed: Vec<Value> = harness
                .log
                .lines()
                .iter()
                .map(|line| serde_json::from_str(line).unwrap())
                .filter(|line: &Value| line["event"] == "probe-failed")
                .collect();
            assert_eq!(failed.len(), 1);
            assert_eq!(failed[0]["reason"], "health");
            assert_eq!(failed[0]["serial"], B30);
            harness.adb.set_failing(None);
            harness.manager.tick();
            harness.manager.settled().await;
            assert_eq!(harness.devices().get(B30).unwrap().probe_error, None);
            harness.manager.stop().await;
        });
    }

    #[test]
    fn probes_settle_when_a_phone_is_unplugged_mid_probe() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.adb.hold();
            harness.manager.start();
            harness.adb.send_list(&fake_adb::recorded_track_lists()[0]);
            fake_adb::until("the identity call", || !harness.adb.calls().is_empty()).await;
            harness.adb.send_list("");
            let devices = harness.devices();
            fake_adb::until("offline", || {
                devices.get(B30).unwrap().status == PhoneStatus::Offline
            })
            .await;
            harness.adb.release();
            time::timeout(Duration::from_secs(60), harness.manager.settled())
                .await
                .expect("an ended session leaves no probe pending");
            harness.manager.stop().await;
        });
    }

    #[test]
    fn a_state_the_database_refuses_leaves_the_table_as_it_was_and_applies_on_the_next_list() {
        fake_adb::block_on(async {
            let dir = ScratchDir::new();
            let path = dir.path().join("spareline.db");
            let clock = Clock::tokio(fake_adb::START);
            let database_clock = clock.clone();
            let db =
                DbHandle::new(Database::open(&path, move || database_clock.now(), |_| {}).unwrap());
            let tamper = Connection::open(&path).unwrap();
            tamper
                .execute_batch("ALTER TABLE phones RENAME TO phones_away")
                .unwrap();
            let adb = FakeAdb::b30().await;
            let log = EventLog::memory(clock.clone());
            let mut manager = DeviceManager::new(DeviceOptions::new(
                adb.client(),
                db,
                Buses::new(),
                "home".to_owned(),
                log.clone(),
                clock,
                Duration::from_mins(1),
            ));
            manager.start();
            let list = fake_adb::recorded_track_lists().remove(0);
            adb.send_list(&list);
            let devices = manager.devices();
            fake_adb::until("the refused state", || {
                log.lines()
                    .iter()
                    .any(|line| line.contains("\"event\":\"device-record-failed\""))
            })
            .await;
            assert!(
                devices.adb().reachable,
                "the adb.unreachable event the database refused leaves the table as it was"
            );
            assert_eq!(devices.get(B30), None, "nothing recorded, nothing shown");
            tamper
                .execute_batch("ALTER TABLE phones_away RENAME TO phones")
                .unwrap();
            drop(tamper);
            adb.send_list(&list);
            fake_adb::until("probed once recorded", || {
                devices.get(B30).is_some_and(|phone| phone.health.is_some())
            })
            .await;
            assert_eq!(devices.get(B30).unwrap().status, PhoneStatus::Online);
            manager.settled().await;
            manager.stop().await;
        });
    }

    #[test]
    fn a_refresh_reads_the_identity_after_the_probe_in_flight() {
        fake_adb::block_on(async {
            let mut harness = Harness::new().await;
            harness.online().await;
            let before = harness.isub_calls();
            harness.devices().refresh(B30);
            harness.devices().refresh(B30);
            harness.devices().refresh(B30);
            harness.manager.settled().await;
            assert_eq!(
                harness.isub_calls(),
                before + 2,
                "the second refresh waits behind the first, and the third is dropped"
            );
            harness.manager.stop().await;
        });
    }
}
