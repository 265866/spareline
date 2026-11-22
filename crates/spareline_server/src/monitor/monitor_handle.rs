//! The monitor's handle, which the routes and the service hold, and the runtime behind it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::value::RawValue;
use spareline_core::monitor::{
    self, Alert, AlertSet, ApplyPlan, Busy, CheckContext, CheckFinish, CheckFinished, CheckReason,
    CheckReport, CommandResult, DataOffCause, DataOffFailure, HealthRow, HealthTracker,
    LOCK_SCREEN_DEX_PHONE_PATH, MobileBytes, MonitorEvent, PhoneSample, Policy, ProfileSummary,
    ProvisionReport, RuleResult, SIM_LOG_COMMAND, SIM_STATE_COMMAND, SimChange, SimLogScanner,
    SimSignal, SimStatePoll, Snapshot,
};
use spareline_core::{PhoneStatus, Timestamp};
use spareline_db::{DatabaseError, NewEvent};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::AbortHandle;
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::monitor::monitor_context::MonitorContext;
use crate::monitor::monitor_error::MonitorError;
use crate::monitor::monitor_settings::MonitorSettings;
use crate::monitor::monitor_status::MonitorStatus;
use crate::monitor::phone_runtime::{PhoneRuntime, SequencedFailure};
use crate::monitor::screen_sessions::ScreenSessions;
use crate::monitor::work_tracker::WorkTracker;
use crate::{DeviceError, LivePhone, PhoneEvent, embedded, shell_text};

/// The batched policy read's timeout.
const READ_TIMEOUT: Duration = Duration::from_mins(1);
/// The apply script's timeout.
const APPLY_TIMEOUT: Duration = Duration::from_mins(2);
/// The timeout of `dumpsys isub` after a SIM signal.
const ISUB_TIMEOUT: Duration = Duration::from_secs(20);
/// The timeout of opening the radio log stream.
const SIM_LOG_SETUP_TIMEOUT: Duration = Duration::from_secs(10);
/// The timeout of the SIM state poll.
const SIM_STATE_TIMEOUT: Duration = Duration::from_secs(5);
/// The mode the policy's files are pushed with.
const PUSH_MODE: u32 = 0o644;

/// Looks after every online phone: health aggregates and alerts, and SIM hotplug for the
/// inventory. With a policy it also runs the policy: a drift check on connect and every
/// `POLICY_CHECK_INTERVAL_S` (radio and data drift reverted on sight, which is all a phone gets
/// automatically), Set up phone on request, and, when the policy keeps radios off, the data watch
/// and data-off on SIM changes. Work on one phone runs one step at a time, except the data-off a
/// SIM signal triggers.
///
/// Every command that changes a setting goes through an `ApplyPlan`, whose radio guard refuses
/// anything that could turn a radio or mobile data on; the reads are fixed read-only commands.
/// Without a policy it changes nothing on a phone: it reads the SIM state, the radio log and
/// isub, and asks the phone manager for an identity refresh.
///
/// Cloning is cheap and every clone is the same monitor.
#[derive(Clone, Debug)]
pub struct Monitor {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the routes, the service and the monitor's own per-phone tasks all hold the \
                      one monitor, and those tasks outlive any borrow"
        )
    )]
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    settings: MonitorSettings,
    context: MonitorContext,
    screens: ScreenSessions,
    state: Mutex<State>,
    work: WorkTracker,
    alert_turn: AsyncMutex<()>,
}

#[derive(Debug, Default)]
struct State {
    running: bool,
    runtimes: Vec<PhoneRuntime>,
    alerts: AlertSet,
    health: HealthTracker,
    sequence: u64,
    background: Vec<AbortHandle>,
}

impl State {
    fn runtime(&mut self, serial: &str) -> &mut PhoneRuntime {
        let index = self
            .runtimes
            .iter()
            .position(|runtime| runtime.serial == serial)
            .unwrap_or_else(|| {
                self.runtimes.push(PhoneRuntime::new(serial));
                self.runtimes.len() - 1
            });
        &mut self.runtimes[index]
    }

    fn existing(&mut self, serial: &str) -> Option<&mut PhoneRuntime> {
        self.runtimes
            .iter_mut()
            .find(|runtime| runtime.serial == serial)
    }

    fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }
}

/// Clears a phone's busy mark when its step ends, however it ends, unless another step waits
/// in the phone's queue: that step sets its own mark as it starts, so the phone never shows idle
/// between two queued steps and the periodic check doesn't slip in between.
struct BusyMark {
    monitor: Monitor,
    serial: String,
}

impl Drop for BusyMark {
    fn drop(&mut self) {
        if let Some(runtime) = self.monitor.state().existing(&self.serial)
            && runtime.waiting == 0
        {
            runtime.busy = None;
        }
    }
}

/// Counts a step waiting for its turn in a phone's queue, until it starts or is dropped.
struct Waiting {
    monitor: Monitor,
    serial: String,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let mut state = self.monitor.state();
        let runtime = state.runtime(&self.serial);
        runtime.waiting = runtime.waiting.saturating_sub(1);
        if runtime.waiting == 0 && runtime.busy.is_some() && runtime.queue.try_lock().is_ok() {
            runtime.busy = None;
        }
    }
}

/// A read of the policy and how long it took.
struct PolicyRead {
    snapshot: Snapshot,
    millis: u64,
}

/// The rule verdicts after a check's fix, and the read they came from.
struct AfterFix {
    results: Vec<RuleResult>,
    snapshot: Snapshot,
}

/// An event on its way to the database.
struct PendingEvent {
    serial: String,
    kind: String,
    detail: Box<RawValue>,
}

impl Monitor {
    /// A monitor with `settings` over the service's phones, database, buses, clock and log. It
    /// watches nothing until [`Monitor::start`].
    #[must_use]
    pub fn new(settings: MonitorSettings, context: MonitorContext) -> Self {
        Self {
            shared: Arc::new(Shared {
                settings,
                context,
                screens: ScreenSessions::new(),
                state: Mutex::new(State::default()),
                work: WorkTracker::default(),
                alert_turn: AsyncMutex::new(()),
            }),
        }
    }

    /// The policy from the profile, or `None` when the service runs without one.
    #[must_use]
    pub fn policy(&self) -> Option<&Policy> {
        self.shared.settings.policy.as_ref()
    }

    /// Internet mobile bytes per SIM per trailing 24 h before data is turned off again and the
    /// phone goes red, or `None` without a radios-off profile.
    #[must_use]
    pub fn data_threshold_bytes(&self) -> Option<u64> {
        self.policy().and_then(Policy::data_threshold_bytes)
    }

    /// The profile part of `GET /api/v1/status`, or `None` without a profile.
    #[must_use]
    pub fn profile(&self) -> Option<ProfileSummary> {
        self.policy().map(Policy::summary)
    }

    /// The screen sessions the `stay_on_off` rule accepts. Hand this to the screens module,
    /// which records each session's start and end in it.
    #[must_use]
    pub fn screen_sessions(&self) -> ScreenSessions {
        self.shared.screens.clone()
    }

    /// Starts watching: every phone online now and each one that comes online gets the SIM
    /// watch and, with a policy, a drift check, then one every check interval; health is
    /// sampled every health interval. Call it inside the runtime, before the phone manager
    /// starts, so no phone's connection is missed. A second call does nothing.
    pub fn start(&self) {
        {
            let mut state = self.state();
            if state.running {
                return;
            }
            state.running = true;
            self.shared.work.open();
        }
        let mut events = self.shared.context.devices.subscribe();
        let monitor = self.clone();
        let follow = tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;

            loop {
                match events.recv().await {
                    Ok(PhoneEvent::Connected { serial }) => monitor.connected(&serial),
                    Ok(PhoneEvent::Disconnected { serial }) => monitor.disconnected(&serial),
                    Ok(PhoneEvent::Ready { .. } | PhoneEvent::SubsChanged { .. }) => {}
                    Err(RecvError::Lagged(_)) => monitor.resync(),
                    Err(RecvError::Closed) => return,
                }
            }
        });
        let mut background = vec![follow.abort_handle()];
        if self.policy().is_some() {
            let monitor = self.clone();
            let period = self.shared.settings.check_interval;
            background.push(every(period, move || monitor.interval_checks()));
        }
        let monitor = self.clone();
        let period = self.shared.settings.health_interval;
        background.push(every(period, move || {
            let worker = monitor.clone();
            drop(
                monitor
                    .shared
                    .work
                    .spawn_while_open(async move { worker.sample_health().await }),
            );
        }));
        self.state().background.extend(background);
        for phone in self.shared.context.devices.list() {
            if phone.status == PhoneStatus::Online {
                self.connected(&phone.serial);
            }
        }
    }

    /// Stops watching, waits for the work in flight, then writes the partial health buckets.
    /// No background work starts once it has begun: a timer or a phone event handled at that
    /// moment either got its work counted first, which this waits for, or is refused. The
    /// tracker closes under the state lock, as [`Monitor::start`] opens it, so an overlapping
    /// start and stop leave the two in step.
    pub async fn stop(&self) {
        {
            let mut state = self.state();
            state.running = false;
            for task in state.background.drain(..) {
                task.abort();
            }
            for runtime in &mut state.runtimes {
                runtime.stop_watch();
            }
            self.shared.work.close();
        }
        self.settled().await;
        let rows = {
            let mut state = self.state();
            let usage = mobile_bytes_by_serial(&state.runtimes);
            state.health.flush(|serial| usage.get(serial).copied())
        };
        self.write_health(rows, None).await;
    }

    /// Waits until no check, Set up phone, SIM reaction or data-off is queued or running.
    pub async fn settled(&self) {
        self.shared.work.settled().await;
    }

    /// What `GET /api/v1/status` needs from the monitor. A phone's last Set up phone run from
    /// before a restart is read from its `provision` event the first time it is shown.
    pub async fn status(&self) -> MonitorStatus {
        let unloaded: Vec<String> = self
            .state()
            .runtimes
            .iter()
            .filter(|runtime| !runtime.provision_loaded)
            .map(|runtime| runtime.serial.clone())
            .collect();
        for serial in unloaded {
            let found = match self.last_provision(&serial).await {
                Ok(found) => found,
                Err(error) => {
                    self.shared
                        .context
                        .log
                        .event("monitor-step-failed")
                        .field("serial", &serial)
                        .field("step", "last-provision")
                        .field("error", &error.to_string())
                        .write();
                    continue;
                }
            };
            if let Some(runtime) = self.state().existing(&serial)
                && !runtime.provision_loaded
            {
                runtime.provision_loaded = true;
                if runtime.provision.is_none() {
                    runtime.provision = found;
                }
            }
        }
        let state = self.state();
        MonitorStatus {
            phones: state.runtimes.iter().map(PhoneRuntime::view).collect(),
            alerts: state.alerts.alerts().to_vec(),
            data_threshold_bytes: self.data_threshold_bytes(),
            profile: self.profile(),
        }
    }

    /// The open alerts.
    #[must_use]
    pub fn alerts(&self) -> Vec<Alert> {
        self.state().alerts.alerts().to_vec()
    }

    /// How many alerts are open, for `GET /api/v1/health`.
    #[must_use]
    pub fn open_alerts(&self) -> usize {
        self.state().alerts.alerts().len()
    }

    /// Runs the drift check on `serial` now, queued behind the phone's other work, and returns
    /// its report. A check that fails on the phone is a report with `error` set, not an `Err`.
    /// The check runs to its end even when the caller stops waiting.
    ///
    /// # Errors
    ///
    /// [`MonitorError::NoPolicy`] without a profile, then [`MonitorError::NotOnline`] when the
    /// phone isn't online.
    ///
    /// # Panics
    ///
    /// Panics when the check panicked, a bug; release builds abort first.
    pub async fn check(&self, serial: &str) -> Result<CheckReport, MonitorError> {
        let policy = self.required()?.clone();
        self.online(serial)?;
        let monitor = self.clone();
        let serial = serial.to_owned();
        let work = self.shared.work.spawn(async move {
            monitor
                .queued(
                    &serial,
                    Busy::Check,
                    Box::pin(monitor.run_check(&serial, &policy, CheckReason::Manual, &[])),
                )
                .await
        });
        Ok(work.await.expect("a drift check doesn't panic"))
    }

    /// Sets the phone up to match the policy, queued behind its other work: applies every
    /// enforceable rule, removing apps and setting the default SMS app included, then verifies.
    /// Only on request, since it removes apps and their data. A run that fails on the phone is a
    /// report with `error` set, not an `Err`. It runs to its end even when the caller stops
    /// waiting.
    ///
    /// # Errors
    ///
    /// [`MonitorError::NoPolicy`] without a profile, then [`MonitorError::NotOnline`] when the
    /// phone isn't online.
    ///
    /// # Panics
    ///
    /// Panics when the run panicked, a bug; release builds abort first.
    pub async fn provision(&self, serial: &str) -> Result<ProvisionReport, MonitorError> {
        let policy = self.required()?.clone();
        self.online(serial)?;
        let monitor = self.clone();
        let serial = serial.to_owned();
        let work = self.shared.work.spawn(async move {
            monitor
                .queued(
                    &serial,
                    Busy::Provision,
                    Box::pin(monitor.run_provision(&serial, &policy)),
                )
                .await
        });
        Ok(work.await.expect("Set up phone doesn't panic"))
    }

    /// Samples every phone's latest health into the 5-minute buckets, writes the buckets that
    /// closed, prunes old ones once a day, and recomputes the alerts. The health timer calls it
    /// every health interval.
    pub async fn sample_health(&self) {
        let phones = self.shared.context.devices.list();
        let at = self.now();
        let output = {
            let mut state = self.state();
            let usage = mobile_bytes_by_serial(&state.runtimes);
            let samples = phone_samples(&phones);
            state
                .health
                .sample(&samples, at, |serial| usage.get(serial).copied())
        };
        self.write_health(output.rows, output.prune_before).await;
        self.refresh_alerts().await;
    }

    fn required(&self) -> Result<&Policy, MonitorError> {
        self.policy().ok_or(MonitorError::NoPolicy)
    }

    fn online(&self, serial: &str) -> Result<(), MonitorError> {
        let online = self
            .shared
            .context
            .devices
            .get(serial)
            .is_some_and(|phone| phone.status == PhoneStatus::Online);
        if online {
            self.state().runtime(serial);
            Ok(())
        } else {
            Err(MonitorError::NotOnline {
                serial: serial.to_owned(),
            })
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .expect("no monitor task panicked while holding its state")
    }

    fn now(&self) -> Timestamp {
        self.shared.context.clock.now()
    }

    fn running(&self) -> bool {
        self.state().running
    }

    /// Runs `work` once the phone's earlier steps are done, marked busy with `busy` meanwhile.
    async fn queued<T>(&self, serial: &str, busy: Busy, work: impl Future<Output = T>) -> T {
        let queue = {
            let mut state = self.state();
            let runtime = state.runtime(serial);
            runtime.waiting += 1;
            runtime.queue.clone()
        };
        let waiting = Waiting {
            monitor: self.clone(),
            serial: serial.to_owned(),
        };
        let _turn = queue.lock().await;
        self.state().runtime(serial).busy = Some(busy);
        drop(waiting);
        let _mark = BusyMark {
            monitor: self.clone(),
            serial: serial.to_owned(),
        };
        work.await
    }

    /// Queues a drift check of `serial` for `reason`, when there is a policy.
    fn enqueue_check(&self, serial: &str, reason: CheckReason) {
        let Some(policy) = self.policy().cloned() else {
            return;
        };
        let monitor = self.clone();
        let serial = serial.to_owned();
        drop(self.shared.work.spawn_while_open(async move {
            monitor
                .queued(
                    &serial,
                    Busy::Check,
                    Box::pin(monitor.run_check(&serial, &policy, reason, &[])),
                )
                .await;
        }));
    }

    /// The periodic check: every online phone that isn't busy gets one.
    fn interval_checks(&self) {
        for phone in self.shared.context.devices.list() {
            if phone.status != PhoneStatus::Online {
                continue;
            }
            let busy = self.state().runtime(&phone.serial).busy.is_some();
            if !busy {
                self.enqueue_check(&phone.serial, CheckReason::Interval);
            }
        }
    }

    /// Catches up with the phone list after missing phone events.
    fn resync(&self) {
        let phones = self.shared.context.devices.list();
        let watched: Vec<String> = self
            .state()
            .runtimes
            .iter()
            .filter(|runtime| !runtime.watch.is_empty())
            .map(|runtime| runtime.serial.clone())
            .collect();
        for phone in &phones {
            let online = phone.status == PhoneStatus::Online;
            let is_watched = watched.contains(&phone.serial);
            if online && !is_watched {
                self.connected(&phone.serial);
            } else if !online && is_watched {
                self.disconnected(&phone.serial);
            }
        }
    }

    /// Starts the SIM watch of a phone that came online and queues its connect check. The
    /// watch starts under the state lock that [`Monitor::stop`] takes to stop every watch, so
    /// one that stop missed can't start after it.
    fn connected(&self, serial: &str) {
        {
            let mut state = self.state();
            if !state.running {
                return;
            }
            let watch = vec![self.follow_sim_log(serial), self.poll_sim_state(serial)];
            let runtime = state.runtime(serial);
            runtime.slots = None;
            runtime.stop_watch();
            runtime.watch = watch;
        }
        self.enqueue_check(serial, CheckReason::Connect);
    }

    fn disconnected(&self, serial: &str) {
        {
            let mut state = self.state();
            let Some(runtime) = state.existing(serial) else {
                return;
            };
            runtime.stop_watch();
            runtime.slots = None;
        }
        let monitor = self.clone();
        drop(
            self.shared
                .work
                .spawn_while_open(async move { monitor.refresh_alerts().await }),
        );
    }

    /// Follows the radio log for SIM lines, reopening the stream a while after it ends.
    fn follow_sim_log(&self, serial: &str) -> AbortHandle {
        let monitor = self.clone();
        let serial = serial.to_owned();
        tokio::spawn(async move {
            let devices = monitor.shared.context.devices.clone();
            while monitor.running() {
                match devices
                    .open_shell(&serial, SIM_LOG_COMMAND, SIM_LOG_SETUP_TIMEOUT)
                    .await
                {
                    Ok(mut stream) => {
                        let mut scanner = SimLogScanner::default();
                        loop {
                            match stream.next_stdout().await {
                                Ok(Some(bytes)) => {
                                    if scanner.push(&bytes) {
                                        monitor.sim_signal(&serial, SimSignal::Logcat);
                                    }
                                }
                                Ok(None) | Err(DeviceError::Disconnected { .. }) => break,
                                Err(error) => {
                                    monitor.log_sim_log_failed(&serial, &error);
                                    break;
                                }
                            }
                        }
                    }
                    Err(DeviceError::Disconnected { .. }) => {}
                    Err(error) => monitor.log_sim_log_failed(&serial, &error),
                }
                if !monitor.running() {
                    return;
                }
                time::sleep(monitor.shared.settings.sim_log_retry).await;
            }
        })
        .abort_handle()
    }

    fn log_sim_log_failed(&self, serial: &str, error: &DeviceError) {
        if self.running() {
            self.shared
                .context
                .log
                .event("sim-log-failed")
                .field("serial", serial)
                .field("error", &error.to_string())
                .write();
        }
    }

    /// Polls `gsm.sim.state`, in case the radio log misses a change: at once, then every poll
    /// interval. A failed poll is skipped; the watch stops when the phone disconnects.
    fn poll_sim_state(&self, serial: &str) -> AbortHandle {
        let monitor = self.clone();
        let serial = serial.to_owned();
        tokio::spawn(async move {
            let devices = monitor.shared.context.devices.clone();
            let mut poll = SimStatePoll::default();
            let mut ticks = time::interval(monitor.shared.settings.sim_poll);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                if !monitor.running() {
                    return;
                }
                if let Ok(output) = devices
                    .shell(&serial, SIM_STATE_COMMAND, SIM_STATE_TIMEOUT)
                    .await
                    && poll.observe(&shell_text::shell_text(&output.stdout))
                {
                    monitor.sim_signal(&serial, SimSignal::SimState);
                }
            }
        })
        .abort_handle()
    }

    /// A SIM may have changed: data off at once with radios off, then, after the debounce, a
    /// fresh isub read and whatever it calls for.
    fn sim_signal(&self, serial: &str, reason: SimSignal) {
        if self
            .policy()
            .is_some_and(|policy| policy.radios_off.is_some())
        {
            self.data_off_now(serial);
        }
        let monitor = self.clone();
        let owned = serial.to_owned();
        let debounce = self.shared.settings.sim_debounce;
        let timer = tokio::spawn(async move {
            time::sleep(debounce).await;
            let watched = {
                let mut state = monitor.state();
                state.running
                    && state
                        .existing(&owned)
                        .is_some_and(|runtime| !runtime.watch.is_empty())
            };
            if !watched {
                return;
            }
            let worker = monitor.clone();
            drop(monitor.shared.work.spawn_while_open(async move {
                worker
                    .queued(
                        &owned,
                        Busy::Sim,
                        Box::pin(worker.sim_check(&owned, reason)),
                    )
                    .await;
            }));
        })
        .abort_handle();
        let replaced = match self.state().existing(serial) {
            Some(runtime) => runtime.sim_timer.replace(timer),
            None => Some(timer),
        };
        if let Some(stale) = replaced {
            stale.abort();
        }
    }

    /// Turns data off now, without waiting behind queued work: a new SIM's data may already be
    /// coming up. Running beside a check is safe, since both only turn data off. One runs at a
    /// time per phone; a signal that arrives meanwhile gets its own run afterwards, since its
    /// SIM may have come up after the running one's commands.
    fn data_off_now(&self, serial: &str) {
        {
            let mut state = self.state();
            let runtime = state.runtime(serial);
            if runtime.data_off_running {
                runtime.data_off_again = true;
                return;
            }
            runtime.data_off_running = true;
        }
        let monitor = self.clone();
        let owned = serial.to_owned();
        let spawned = self.shared.work.spawn_while_open(async move {
            let serial = owned;
            loop {
                monitor.state().runtime(&serial).data_off_again = false;
                let commands = monitor::data_off_commands(&[]);
                if let Err(error) = monitor.apply_commands(&serial, commands).await {
                    monitor
                        .data_off_failed(&serial, DataOffCause::Sim, &error)
                        .await;
                }
                let mut state = monitor.state();
                let running = state.running;
                let runtime = state.runtime(&serial);
                if !(runtime.data_off_again && running) {
                    runtime.data_off_running = false;
                    return;
                }
            }
        });
        if spawned.is_none() {
            self.state().runtime(serial).data_off_running = false;
        }
    }

    async fn data_off_failed(&self, serial: &str, cause: DataOffCause, error: &str) {
        let at = self.now();
        {
            let mut state = self.state();
            let sequence = state.next_sequence();
            state.runtime(serial).data_off_failure = Some(SequencedFailure {
                failure: DataOffFailure {
                    at,
                    cause,
                    error: error.to_owned(),
                },
                sequence,
            });
        }
        self.shared
            .context
            .log
            .event("data-off-failed")
            .field("serial", serial)
            .field("cause", cause.as_str())
            .field("error", error)
            .write();
        self.refresh_alerts().await;
    }

    /// Records which sub is in each slot and returns what changed since the last read of this
    /// connection.
    fn observe_slots(&self, serial: &str, after: BTreeMap<u32, i64>) -> Vec<SimChange> {
        let mut state = self.state();
        let runtime = state.runtime(serial);
        let changes = runtime
            .slots
            .as_ref()
            .map_or_else(Vec::new, |before| monitor::diff_slots(before, &after));
        runtime.slots = Some(after);
        changes
    }

    async fn sim_check(&self, serial: &str, reason: SimSignal) {
        let at = self.now();
        let devices = &self.shared.context.devices;
        let read = devices
            .shell(serial, "dumpsys isub", ISUB_TIMEOUT)
            .await
            .map_err(|error| error.to_string())
            .and_then(|output| {
                let isub = shell_text::shell_text(&output.stdout);
                monitor::check_isub(&isub).map_err(|error| error.to_string())?;
                Ok(monitor::slot_map(&spareline_core::parse_isub(&isub).active))
            });
        let changes = match read {
            Ok(slots) => self.observe_slots(serial, slots),
            Err(error) => {
                if self.policy().is_some() {
                    self.fail(serial, CheckReason::Sim, at, &error, None).await;
                } else {
                    self.shared
                        .context
                        .log
                        .event("sim-read-failed")
                        .field("serial", serial)
                        .field("error", &error)
                        .write();
                }
                return;
            }
        };
        self.shared
            .context
            .log
            .event("sim-signal")
            .field("serial", serial)
            .field("reason", reason.as_str())
            .field("changes", &changes.len())
            .write();
        let inserted: Vec<i64> = changes.iter().filter_map(|change| change.to).collect();
        let policy = self.policy().cloned();
        let radios_off = policy
            .as_ref()
            .is_some_and(|policy| policy.radios_off.is_some());
        if radios_off && !inserted.is_empty() {
            let commands = monitor::data_off_commands(&inserted);
            if let Err(error) = self.apply_commands(serial, commands).await {
                self.data_off_failed(serial, DataOffCause::Sim, &error)
                    .await;
            }
        }
        match policy {
            Some(policy) if !changes.is_empty() => {
                self.run_check(serial, &policy, CheckReason::Sim, &changes)
                    .await;
            }
            Some(_) | None => devices.refresh(serial),
        }
    }

    async fn read(&self, serial: &str, policy: &Policy) -> Result<PolicyRead, String> {
        let started = Instant::now();
        let output = self
            .shared
            .context
            .devices
            .shell(serial, &policy.read_script, READ_TIMEOUT)
            .await
            .map_err(|error| error.to_string())?;
        let millis = elapsed_millis(started);
        let snapshot = policy
            .parse_read(
                &shell_text::shell_text(&output.stdout),
                i64::from(output.exit_code),
                &shell_text::shell_text(&output.stderr),
            )
            .map_err(|error| error.to_string())?;
        Ok(PolicyRead { snapshot, millis })
    }

    async fn apply_commands(
        &self,
        serial: &str,
        commands: Vec<String>,
    ) -> Result<Vec<CommandResult>, String> {
        let plan = ApplyPlan::try_new(commands, Vec::new()).map_err(|error| error.to_string())?;
        self.apply(serial, plan).await
    }

    /// Pushes the files the batch needs, then runs its commands in one shell call; the radio
    /// guard passed every one of them when the plan was built. A failed push skips only the
    /// commands that need its file, with the push's error as their output; the radio and data
    /// commands in the same batch still run.
    async fn apply(&self, serial: &str, mut plan: ApplyPlan) -> Result<Vec<CommandResult>, String> {
        let devices = &self.shared.context.devices;
        let log = &self.shared.context.log;
        let mut skipped: Vec<String> = Vec::new();
        let uploads = plan.uploads().to_vec();
        for (index, upload) in uploads.iter().enumerate() {
            for file in &upload.files {
                let pushed = match phone_file(file.path) {
                    Ok(data) => devices
                        .push(serial, data, file.path, PUSH_MODE)
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error),
                };
                if let Err(error) = pushed {
                    log.event("policy-push-failed")
                        .field("serial", serial)
                        .field("path", file.path)
                        .field("error", &error)
                        .write();
                    plan.push_failed(index, file.path, &error);
                    skipped.extend(upload.commands.iter().cloned());
                    break;
                }
            }
        }
        let Some(script) = plan.script() else {
            return Ok(plan.results(""));
        };
        let commands: Vec<&String> = plan
            .commands()
            .iter()
            .filter(|command| !skipped.contains(command))
            .collect();
        log.event("policy-apply")
            .field("serial", serial)
            .field("commands", &commands)
            .write();
        let output = devices
            .shell(serial, &script, APPLY_TIMEOUT)
            .await
            .map_err(|error| error.to_string())?;
        Ok(plan.results(&shell_text::shell_text(&output.stdout)))
    }

    /// Reads every rule, reverts radio and data drift, and, when the policy keeps radios off,
    /// turns data off for SIMs over the threshold or new ones. Never fails: a failure is
    /// recorded on the report and raises an alert.
    async fn run_check(
        &self,
        serial: &str,
        policy: &Policy,
        reason: CheckReason,
        known: &[SimChange],
    ) -> CheckReport {
        let at = self.now();
        let started = self.state().next_sequence();
        let mut cause = (policy.radios_off.is_some()
            && known.iter().any(|change| change.to.is_some()))
        .then_some(DataOffCause::Sim);
        let checked = self
            .try_check(
                serial,
                policy,
                CheckStart {
                    reason,
                    at,
                    started,
                },
                known,
                &mut cause,
            )
            .await;
        match checked {
            Ok(report) => report,
            Err(error) => self.fail(serial, reason, at, &error, cause).await,
        }
    }

    async fn try_check(
        &self,
        serial: &str,
        policy: &Policy,
        start: CheckStart,
        known: &[SimChange],
        cause: &mut Option<DataOffCause>,
    ) -> Result<CheckReport, String> {
        let CheckStart {
            reason,
            at,
            started,
        } = start;
        let context = CheckContext {
            screen_session: self.shared.screens.is_active(serial),
        };
        let first = self.read(serial, policy).await?;
        let mut changes = known.to_vec();
        changes.extend(self.observe_slots(serial, first.snapshot.slot_map()));
        let inserted = if policy.radios_off.is_some() {
            monitor::inserted_sims(&changes)
        } else {
            Vec::new()
        };
        let before = monitor::evaluate(&first.snapshot, context, &policy.rules);
        let usage = policy.usage(&first.snapshot, self.now());
        let mut data_off_subs: Vec<i64> = usage
            .iter()
            .filter(|sim| sim.over)
            .map(|sim| sim.sub_id)
            .collect();
        data_off_subs.extend(inserted.iter().filter_map(|change| change.to));
        if !inserted.is_empty() {
            *cause = Some(DataOffCause::Sim);
        } else if usage.iter().any(|sim| sim.over) {
            cause.get_or_insert(DataOffCause::DataThreshold);
        }
        let batch = policy
            .drift_batch(&first.snapshot, context, reason, &data_off_subs)
            .map_err(|error| error.to_string())?;
        let after = match batch.apply {
            Some(plan) => {
                self.apply(serial, plan).await?;
                let second = self.read(serial, policy).await?;
                AfterFix {
                    results: monitor::evaluate(&second.snapshot, context, &policy.rules),
                    snapshot: second.snapshot,
                }
            }
            None => AfterFix {
                results: before.clone(),
                snapshot: first.snapshot,
            },
        };
        let alarmed = self.state().runtime(serial).alarmed.clone();
        let finished = policy.finish_check(&CheckFinish {
            reason,
            at,
            read_ms: first.millis,
            steps: &batch.steps,
            before: &before,
            after: &after.results,
            after_snapshot: &after.snapshot,
            usage,
            inserted: &inserted,
            alarmed: &alarmed,
        });
        self.record_check(serial, reason, started, &finished).await;
        if !changes.is_empty() {
            self.shared.context.devices.refresh(serial);
        }
        Ok(finished.report)
    }

    /// Records a finished check: clears an older data-off failure when data is off, records a
    /// new one when it isn't, writes the events and log lines, keeps the report and the alarm
    /// set, and recomputes the alerts.
    async fn record_check(
        &self,
        serial: &str,
        reason: CheckReason,
        started: u64,
        finished: &CheckFinished,
    ) {
        if finished.data_off {
            if let Some(runtime) = self.state().existing(serial)
                && runtime
                    .data_off_failure
                    .as_ref()
                    .is_some_and(|failure| failure.sequence < started)
            {
                runtime.data_off_failure = None;
            }
        } else if let Some(note) = &finished.data_off_failure {
            self.data_off_failed(serial, note.cause, &note.error).await;
        }
        self.publish(serial, &finished.events).await;
        let log = &self.shared.context.log;
        if !finished.drifted.is_empty() {
            log.event("drift")
                .field("serial", serial)
                .field("reason", reason.as_str())
                .field("reverted", &finished.drifted)
                .write();
        }
        for fix in &finished.routine {
            log.event(&fix.event.replace('_', "-"))
                .field("serial", serial)
                .field("reason", reason.as_str())
                .field("rule", &fix.revert.rule)
                .field("before", &fix.revert.before)
                .field("after", &fix.revert.after)
                .field("fixed", &fix.revert.fixed)
                .write();
        }
        {
            let mut state = self.state();
            let runtime = state.runtime(serial);
            runtime.check = Some(finished.report.clone());
            runtime.alarmed.clone_from(&finished.alarmed);
        }
        self.refresh_alerts().await;
    }

    /// Records a failed check, keeping the last known rule results, and raises its alert; a
    /// failure while data had to go off raises a critical one too.
    async fn fail(
        &self,
        serial: &str,
        reason: CheckReason,
        at: Timestamp,
        error: &str,
        cause: Option<DataOffCause>,
    ) -> CheckReport {
        if let Some(cause) = cause {
            self.data_off_failed(serial, cause, error).await;
        }
        let report = {
            let mut state = self.state();
            let runtime = state.runtime(serial);
            let report = CheckReport::failed(at, reason, error, runtime.check.as_ref());
            runtime.check = Some(report.clone());
            report
        };
        self.shared
            .context
            .log
            .event("policy-check-failed")
            .field("serial", serial)
            .field("reason", reason.as_str())
            .field("error", error)
            .write();
        self.refresh_alerts().await;
        report
    }

    async fn run_provision(&self, serial: &str, policy: &Policy) -> ProvisionReport {
        let started = Instant::now();
        let at = self.now();
        let report = match self.try_provision(serial, policy, at, started).await {
            Ok(report) => report,
            Err(error) => ProvisionReport::failed(at, elapsed_millis(started), &error),
        };
        {
            let mut state = self.state();
            let runtime = state.runtime(serial);
            runtime.provision = Some(report.clone());
            runtime.provision_loaded = true;
        }
        self.publish(
            serial,
            &[MonitorEvent::Provision {
                report: report.clone(),
            }],
        )
        .await;
        let changed: Vec<&str> = report.steps.iter().map(|step| step.rule.as_str()).collect();
        self.shared
            .context
            .log
            .event("provision")
            .field("serial", serial)
            .field("ok", &report.ok)
            .field("changed", &changed)
            .field("error", &report.error)
            .write();
        self.refresh_alerts().await;
        report
    }

    async fn try_provision(
        &self,
        serial: &str,
        policy: &Policy,
        at: Timestamp,
        started: Instant,
    ) -> Result<ProvisionReport, String> {
        let context = CheckContext {
            screen_session: self.shared.screens.is_active(serial),
        };
        let first = self.read(serial, policy).await?;
        let changes = self.observe_slots(serial, first.snapshot.slot_map());
        let batch = policy
            .provision_batch(&first.snapshot, context)
            .map_err(|error| error.to_string())?;
        let ran = match batch.apply {
            Some(plan) => self.apply(serial, plan).await?,
            None => Vec::new(),
        };
        if !changes.is_empty() {
            self.shared.context.devices.refresh(serial);
        }
        let after = self.read(serial, policy).await?;
        let results = monitor::evaluate(&after.snapshot, context, &policy.rules);
        let usage = policy.usage(&after.snapshot, self.now());
        let report = ProvisionReport::finished(
            at,
            elapsed_millis(started),
            &batch.steps,
            &ran,
            results.clone(),
        );
        self.state().runtime(serial).check = Some(CheckReport {
            at,
            reason: CheckReason::Manual,
            read_ms: 0,
            results,
            usage,
            reverted: Vec::new(),
            error: None,
        });
        Ok(report)
    }

    /// Recomputes the alerts, the health alerts followed by the policy alerts, and records the
    /// ones raised and cleared. One update records its events before the next computes, so the
    /// events of a key stay in order.
    async fn refresh_alerts(&self) {
        let _turn = self.shared.alert_turn.lock().await;
        let phones = self.shared.context.devices.list();
        let now = self.now();
        let changes = {
            let mut state = self.state();
            let samples = phone_samples(&phones);
            let mut next = state.health.alerts(&samples, now);
            let sources: Vec<_> = state
                .runtimes
                .iter()
                .map(|runtime| {
                    let online = phones.iter().any(|phone| {
                        phone.serial == runtime.serial && phone.status == PhoneStatus::Online
                    });
                    runtime.alert_source(online)
                })
                .collect();
            next.extend(Alert::for_policy(&sources, self.policy()));
            state.alerts.update(next)
        };
        let mut events: Vec<MonitorEvent> = changes
            .raised
            .into_iter()
            .map(|alert| MonitorEvent::AlertRaised { alert })
            .collect();
        events.extend(
            changes
                .cleared
                .into_iter()
                .map(|alert| MonitorEvent::AlertCleared { alert }),
        );
        self.publish("", &events).await;
    }

    /// Writes `events` for the phone with `serial`, alert events for the phone they name, all
    /// at the current time in one transaction, and publishes them once committed, while the
    /// database is still held, so events reach the bus in the order of their ids whichever task
    /// writes them, the phone manager's included: a stream drops an id at or below its cursor,
    /// and would lose one published late.
    pub(crate) async fn publish(&self, serial: &str, events: &[MonitorEvent]) {
        if events.is_empty() {
            return;
        }
        let mut pending: Vec<PendingEvent> = Vec::with_capacity(events.len());
        for event in events {
            match RawValue::from_string(event.detail_json()) {
                Ok(detail) => pending.push(PendingEvent {
                    serial: event.alert_serial().unwrap_or(serial).to_owned(),
                    kind: event.kind().to_owned(),
                    detail,
                }),
                Err(error) => self
                    .shared
                    .context
                    .log
                    .event("monitor-step-failed")
                    .field("serial", serial)
                    .field("step", event.kind())
                    .field("error", &error.to_string())
                    .write(),
            }
        }
        let at = self.now();
        let buses = self.shared.context.buses.clone();
        let recorded = self
            .shared
            .context
            .db
            .call(move |db| {
                let mut rows = Vec::with_capacity(pending.len());
                for event in pending {
                    rows.push(NewEvent {
                        phone_id: db.inventory().phone_id(&event.serial)?,
                        kind: event.kind,
                        detail: event.detail,
                    });
                }
                let recorded = db.device_events().insert(&rows, at)?;
                buses.publish_device_events(&recorded);
                Ok::<(), DatabaseError>(())
            })
            .await;
        if let Err(error) = recorded {
            self.shared
                .context
                .log
                .event("monitor-step-failed")
                .field("serial", serial)
                .field("step", "events")
                .field("error", &error.to_string())
                .write();
        }
    }

    async fn write_health(&self, rows: Vec<HealthRow>, prune_before: Option<Timestamp>) {
        if rows.is_empty() && prune_before.is_none() {
            return;
        }
        let written = self
            .shared
            .context
            .db
            .call(move |db| {
                let mut health = db.health();
                for row in &rows {
                    health.write_row(row)?;
                }
                if let Some(before) = prune_before {
                    health.prune(before)?;
                }
                Ok::<(), DatabaseError>(())
            })
            .await;
        if let Err(error) = written {
            self.shared
                .context
                .log
                .event("monitor-step-failed")
                .field("step", "health")
                .field("error", &error.to_string())
                .write();
        }
    }

    /// The phone's last Set up phone run from its `provision` event, `Ok(None)` when it never
    /// ran, or `Err` when the database failed, which leaves it to be read again next time.
    async fn last_provision(&self, serial: &str) -> Result<Option<ProvisionReport>, DatabaseError> {
        let owned = serial.to_owned();
        let found = self
            .shared
            .context
            .db
            .call(move |db| db.device_events().last_provision(&owned))
            .await?;
        Ok(found.map(|last| {
            let at = Timestamp::parse_iso(&last.at).unwrap_or_else(|| {
                self.shared
                    .context
                    .log
                    .event("monitor-step-failed")
                    .field("serial", serial)
                    .field("step", "last-provision")
                    .field(
                        "error",
                        &format!("the provision event's time {:?} isn't an ISO time", last.at),
                    )
                    .write();
                self.now()
            });
            let detail = serde_json::json!({
                "ok": last.ok,
                "changed": last.changed,
                "durationMs": last.duration_ms,
                "error": last.error,
            });
            ProvisionReport::from_event(at, &detail)
        }))
    }
}

/// Why and when a check started, and its place on the monitor's sequence.
#[derive(Clone, Copy, Debug)]
struct CheckStart {
    reason: CheckReason,
    at: Timestamp,
    started: u64,
}

/// Runs `tick` every `period`, the first time one period from now, as `setInterval` did.
fn every(period: Duration, tick: impl Fn() + Send + 'static) -> AbortHandle {
    tokio::spawn(async move {
        let mut ticks = time::interval_at(Instant::now() + period, period);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            tick();
        }
    })
    .abort_handle()
}

/// The phones as the health tracker reads them.
fn phone_samples(phones: &[LivePhone]) -> Vec<PhoneSample<'_>> {
    phones
        .iter()
        .map(|phone| PhoneSample {
            serial: &phone.serial,
            status: phone.status,
            since: phone.since,
            health: phone.health.as_ref(),
        })
        .collect()
}

/// The total mobile bytes of each phone's last check, for its `health_5m` rows.
fn mobile_bytes_by_serial(runtimes: &[PhoneRuntime]) -> BTreeMap<String, MobileBytes> {
    runtimes
        .iter()
        .filter_map(|runtime| {
            let check = runtime.check.as_ref()?;
            Some((runtime.serial.clone(), check.mobile_bytes()))
        })
        .collect()
}

/// The bytes of a file the policy pushes, checked against the digest the rule names.
fn phone_file(path: &str) -> Result<&'static [u8], String> {
    if path != LOCK_SCREEN_DEX_PHONE_PATH {
        return Err(format!("no file for {path} is built in"));
    }
    monitor::verify_lock_screen_dex(path, embedded::LOCK_SCREEN_DEX)
        .map_err(|mismatch| mismatch.to_string())?;
    Ok(embedded::LOCK_SCREEN_DEX)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::pin::{self, Pin};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll, Waker};

    use spareline_adb::{AdbClient, Endpoint};
    use spareline_db::{Database, PhoneState};
    use tokio::runtime::Builder;
    use tokio::sync::{mpsc, oneshot};
    use tokio::task;

    use super::*;
    use crate::fake_adb::{self, B30};
    use crate::{Buses, Clock, DbHandle, Devices, EventLog};

    /// A monitor with `policy` over the B30, online with a session, whose calls reach no adb
    /// server and so fail at once.
    fn monitor(policy: Option<Policy>) -> Monitor {
        let clock = Clock::tokio(fake_adb::START);
        let devices = Devices::new(
            AdbClient::new(Endpoint::new("127.0.0.1".to_owned(), 9)),
            clock.clone(),
        );
        {
            let mut table = devices.table();
            table.apply_state(B30, PhoneState::Device, Some("1-1"), clock.now());
            table
                .start_session(B30, mpsc::unbounded_channel().0)
                .unwrap();
        }
        let database_clock = clock.clone();
        Monitor::new(
            MonitorSettings {
                policy,
                ..MonitorSettings::default()
            },
            MonitorContext {
                devices,
                db: DbHandle::new(Database::open_in_memory(move || database_clock.now()).unwrap()),
                buses: Buses::new(),
                clock,
                log: EventLog::discard(),
            },
        )
    }

    fn b30_policy() -> Policy {
        Policy::load(
            "nuu-b30-no-data.json",
            &fake_adb::fixture("nuu-b30-no-data.json"),
        )
        .unwrap()
    }

    fn busy(monitor: &Monitor) -> Option<Busy> {
        monitor
            .state()
            .existing(B30)
            .and_then(|runtime| runtime.busy)
    }

    fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn a_phone_stays_busy_from_one_queued_step_to_the_next() {
        fake_adb::block_on(async {
            let monitor = monitor(None);
            let (finish, finished) = oneshot::channel::<()>();
            let mut first = pin::pin!(monitor.queued(B30, Busy::Check, async {
                finished.await.unwrap();
            }));
            assert!(poll(first.as_mut()).is_pending());
            assert_eq!(busy(&monitor), Some(Busy::Check));
            let mut second = pin::pin!(monitor.queued(B30, Busy::Provision, async {}));
            assert!(poll(second.as_mut()).is_pending(), "the second step waits");
            finish.send(()).unwrap();
            assert!(poll(first.as_mut()).is_ready());
            assert_eq!(
                busy(&monitor),
                Some(Busy::Check),
                "never idle between two queued steps, so the periodic check can't slip in"
            );
            assert!(poll(second.as_mut()).is_ready());
            assert_eq!(busy(&monitor), None, "idle once the last step ends");
        });
    }

    #[test]
    fn a_step_that_stops_waiting_after_the_one_before_ended_leaves_the_phone_idle() {
        fake_adb::block_on(async {
            let monitor = monitor(None);
            let (finish, finished) = oneshot::channel::<()>();
            let mut first = pin::pin!(monitor.queued(B30, Busy::Check, async {
                finished.await.unwrap();
            }));
            assert!(poll(first.as_mut()).is_pending());
            let mut second = Box::pin(monitor.queued(B30, Busy::Provision, async {}));
            assert!(poll(second.as_mut()).is_pending());
            finish.send(()).unwrap();
            assert!(poll(first.as_mut()).is_ready());
            assert_eq!(busy(&monitor), Some(Busy::Check));
            drop(second);
            assert_eq!(
                busy(&monitor),
                None,
                "nothing runs or waits, so the phone shows idle"
            );
        });
    }

    #[test]
    fn the_monitor_runs_exactly_while_its_tracker_admits_background_work() {
        Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let monitor = monitor(None);
                monitor.start();
                let done = Arc::new(AtomicBool::new(false));
                let checker = {
                    let monitor = monitor.clone();
                    let done = Arc::clone(&done);
                    tokio::spawn(async move {
                        let mut checks = 0_u32;
                        while !done.load(Ordering::Acquire) {
                            {
                                let state = monitor.state();
                                let admits =
                                    monitor.shared.work.spawn_while_open(async {}).is_some();
                                assert_eq!(
                                    state.running, admits,
                                    "the state and the tracker change together, under the \
                                     state lock"
                                );
                            }
                            checks += 1;
                            task::yield_now().await;
                        }
                        checks
                    })
                };
                let starter = {
                    let monitor = monitor.clone();
                    tokio::spawn(async move {
                        for _ in 0..20_000 {
                            monitor.start();
                            task::yield_now().await;
                        }
                    })
                };
                for _ in 0..20_000 {
                    monitor.stop().await;
                    task::yield_now().await;
                }
                starter.await.unwrap();
                done.store(true, Ordering::Release);
                assert!(checker.await.unwrap() > 0, "the checker ran");
                monitor.stop().await;
            });
    }

    #[test]
    fn once_stopped_the_monitor_admits_no_periodic_check() {
        fake_adb::block_on(async {
            let monitor = monitor(Some(b30_policy()));
            monitor.start();
            monitor.settled().await;
            monitor.stop().await;
            let connect = monitor.status().await.policy(B30).unwrap().check.clone();
            assert_eq!(
                connect.map(|check| check.reason),
                Some(CheckReason::Connect),
                "the connect check ran, and failed, since no adb server answers"
            );
            monitor.interval_checks();
            time::sleep(Duration::from_mins(5)).await;
            monitor.settled().await;
            let last = monitor.status().await.policy(B30).unwrap().check.clone();
            assert_eq!(
                last.map(|check| check.reason),
                Some(CheckReason::Connect),
                "no check started after stop"
            );
        });
    }
}
