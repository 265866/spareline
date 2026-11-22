//! The phone manager's state: every phone seen this run, its session and its probe queue, and
//! the adb server's reachability.

use spareline_core::{PhoneHealth, PhoneIdentity, PhoneStatus, Timestamp};
use spareline_db::PhoneState;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::devices::adb_status::AdbStatus;
use crate::devices::device_error::DeviceError;
use crate::devices::live_phone::LivePhone;
use crate::devices::probe_reason::ProbeReason;
use crate::devices::session::{Session, SessionToken};

/// Every phone the manager has seen this run, in the order it first saw them, and the adb
/// server's reachability. The manager is its only writer; [`Devices`](crate::Devices) reads it.
#[derive(Debug)]
pub(crate) struct PhoneTable {
    phones: Vec<PhoneEntry>,
    adb: AdbStatus,
    adb_known: bool,
    next_session: u64,
}

/// One phone: what the API shows, its session while online, and its probe queue.
///
/// Probes of a phone run one at a time, on its session's worker. While one is queued or running,
/// another request is dropped, except one identity refresh, which waits behind it: that probe
/// may have read the SIMs before the change the refresh is for.
#[derive(Debug)]
struct PhoneEntry {
    live: LivePhone,
    session: Option<Session>,
    pending: usize,
    refresh_queued: bool,
    worker: Option<JoinHandle<()>>,
}

/// What [`PhoneTable::apply_state`] changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StateChange {
    /// The status before.
    pub(crate) before: PhoneStatus,
    /// The status now.
    pub(crate) after: PhoneStatus,
    /// Whether the phone has a session, which an online phone needs.
    pub(crate) has_session: bool,
}

/// A session [`PhoneTable::start_session`] opened.
#[derive(Debug)]
pub(crate) struct SessionStart {
    /// The new session's token, for its worker.
    pub(crate) token: SessionToken,
    /// The previous session's worker, which the new one waits for so probes of a phone never
    /// overlap.
    pub(crate) previous_worker: Option<JoinHandle<()>>,
}

impl PhoneTable {
    /// An empty table, with the adb server not reached yet.
    pub(crate) fn new(now: Timestamp) -> Self {
        Self {
            phones: Vec::new(),
            adb: AdbStatus {
                reachable: false,
                version: None,
                since: now,
                error: Some("not connected yet".to_owned()),
            },
            adb_known: false,
            next_session: 1,
        }
    }

    /// Every phone, in the order the manager first saw them.
    pub(crate) fn list(&self) -> Vec<LivePhone> {
        self.phones.iter().map(|entry| entry.live.clone()).collect()
    }

    /// The phone with `serial`.
    pub(crate) fn get(&self, serial: &str) -> Option<LivePhone> {
        self.entry(serial).map(|entry| entry.live.clone())
    }

    /// The serials of every phone.
    pub(crate) fn serials(&self) -> Vec<String> {
        self.phones
            .iter()
            .map(|entry| entry.live.serial.clone())
            .collect()
    }

    /// The adb server's reachability.
    pub(crate) fn adb(&self) -> AdbStatus {
        self.adb.clone()
    }

    /// Whether an attempt to reach the adb server that found it `reachable` is news to log and
    /// record: reaching it at startup isn't, failing to reach it or recovering is.
    pub(crate) fn adb_news(&self, reachable: bool) -> bool {
        let first = !self.adb_known;
        let changed = first || reachable != self.adb.reachable;
        changed && !(first && reachable)
    }

    /// Records an attempt to reach the adb server. `since` moves only when `reachable` changes.
    pub(crate) fn set_adb(
        &mut self,
        reachable: bool,
        version: Option<u32>,
        error: Option<String>,
        now: Timestamp,
    ) {
        let changed = !self.adb_known || reachable != self.adb.reachable;
        self.adb_known = true;
        self.adb = AdbStatus {
            reachable,
            version,
            since: if changed { now } else { self.adb.since },
            error,
        };
    }

    /// A token of the session of the online phone `serial`.
    ///
    /// # Errors
    ///
    /// [`DeviceError::NotOnline`] when the phone isn't online.
    pub(crate) fn token(&self, serial: &str) -> Result<SessionToken, DeviceError> {
        let entry = self.entry(serial);
        match entry.and_then(|entry| {
            (entry.live.status == PhoneStatus::Online)
                .then_some(entry.session.as_ref())
                .flatten()
        }) {
            Some(session) => Ok(session.token()),
            None => Err(DeviceError::NotOnline {
                serial: serial.to_owned(),
                state: entry
                    .map_or("unknown", |entry| entry.live.adb_state.as_str())
                    .to_owned(),
            }),
        }
    }

    /// Records that adb reports `serial` in `state`, adding the phone on first sight. Returns
    /// `None` when the phone was already in that state, which changes nothing.
    pub(crate) fn apply_state(
        &mut self,
        serial: &str,
        state: PhoneState,
        usb: Option<&str>,
        now: Timestamp,
    ) -> Option<StateChange> {
        if self
            .entry(serial)
            .is_some_and(|entry| entry.live.adb_state == state)
        {
            return None;
        }
        if self.entry(serial).is_none() {
            self.phones.push(PhoneEntry {
                live: LivePhone {
                    serial: serial.to_owned(),
                    adb_state: state,
                    status: PhoneStatus::Offline,
                    since: now,
                    usb: None,
                    identity: None,
                    identity_at: None,
                    health: None,
                    health_at: None,
                    probe_error: None,
                },
                session: None,
                pending: 0,
                refresh_queued: false,
                worker: None,
            });
        }
        let entry = self.entry_mut(serial)?;
        let before = entry.live.status;
        entry.live.adb_state = state;
        entry.live.status = spareline_core::phone_status(state.as_str());
        if let Some(usb) = usb.filter(|usb| !usb.is_empty()) {
            entry.live.usb = Some(usb.to_owned());
        }
        if entry.live.status != before {
            entry.live.since = now;
        }
        Some(StateChange {
            before,
            after: entry.live.status,
            has_session: entry.session.is_some(),
        })
    }

    /// Opens a session for the phone `serial`, whose probes go to `probes`, and clears its
    /// probe error. Returns `None` for an unknown phone.
    pub(crate) fn start_session(
        &mut self,
        serial: &str,
        probes: UnboundedSender<ProbeReason>,
    ) -> Option<SessionStart> {
        let id = self.next_session;
        self.next_session += 1;
        let entry = self.entry_mut(serial)?;
        let session = Session::new(id, probes);
        let token = session.token();
        entry.session = Some(session);
        entry.pending = 0;
        entry.refresh_queued = false;
        entry.live.probe_error = None;
        Some(SessionStart {
            token,
            previous_worker: entry.worker.take(),
        })
    }

    /// Keeps the probe worker of the session just opened for `serial`, for the next session and
    /// shutdown to wait for.
    pub(crate) fn set_worker(&mut self, serial: &str, worker: JoinHandle<()>) {
        if let Some(entry) = self.entry_mut(serial) {
            entry.worker = Some(worker);
        }
    }

    /// Ends the session of `serial`, cancelling its calls, and forgets its probe queue, whose
    /// worker can no longer report on it. Returns whether it had one. Wake whoever waits for
    /// probes to settle afterwards.
    pub(crate) fn end_session(&mut self, serial: &str) -> bool {
        let Some(entry) = self.entry_mut(serial) else {
            return false;
        };
        entry.pending = 0;
        entry.refresh_queued = false;
        entry.session.take().is_some()
    }

    /// Ends every session and forgets their probe queues, and returns their workers, for the
    /// manager to wait for at shutdown.
    pub(crate) fn end_all_sessions(&mut self) -> Vec<JoinHandle<()>> {
        let mut workers = Vec::new();
        for entry in &mut self.phones {
            entry.session = None;
            entry.pending = 0;
            entry.refresh_queued = false;
            workers.extend(entry.worker.take());
        }
        workers
    }

    /// Queues a probe of the online phone `serial` for `reason`, unless one is queued or running
    /// already: then only an identity refresh is queued, once. Returns whether it was queued.
    pub(crate) fn schedule(&mut self, serial: &str, reason: ProbeReason) -> bool {
        let Some(entry) = self.entry_mut(serial) else {
            return false;
        };
        let Some(session) = entry.session.as_ref() else {
            return false;
        };
        if entry.pending > 0 && (reason != ProbeReason::Identity || entry.refresh_queued) {
            return false;
        }
        if !session.send_probe(reason) {
            return false;
        }
        if reason == ProbeReason::Identity && entry.pending > 0 {
            entry.refresh_queued = true;
        }
        entry.pending += 1;
        true
    }

    /// Records that the worker of session `session` starts a probe for `reason`.
    pub(crate) fn probe_started(&mut self, serial: &str, session: u64, reason: ProbeReason) {
        if reason == ProbeReason::Identity
            && let Some(entry) = self.session_entry(serial, session)
        {
            entry.refresh_queued = false;
        }
    }

    /// Records that the worker of session `session` finished a probe.
    pub(crate) fn probe_finished(&mut self, serial: &str, session: u64) {
        if let Some(entry) = self.session_entry(serial, session) {
            entry.pending = entry.pending.saturating_sub(1);
        }
    }

    /// Whether any phone has a probe queued or running.
    pub(crate) fn any_probe_pending(&self) -> bool {
        self.phones.iter().any(|entry| entry.pending > 0)
    }

    /// The online phones with no probe queued or running, which the health round probes.
    pub(crate) fn idle_online(&self) -> Vec<String> {
        self.phones
            .iter()
            .filter(|entry| entry.live.status == PhoneStatus::Online && entry.pending == 0)
            .map(|entry| entry.live.serial.clone())
            .collect()
    }

    /// Records the identity session `session` read, at `at`, unless the session ended.
    pub(crate) fn set_identity(
        &mut self,
        serial: &str,
        session: u64,
        identity: PhoneIdentity,
        at: Timestamp,
    ) {
        if let Some(entry) = self.session_entry(serial, session) {
            entry.live.identity = Some(identity);
            entry.live.identity_at = Some(at);
        }
    }

    /// Records the health session `session` read, at `at`, and clears the probe error, unless
    /// the session ended.
    pub(crate) fn set_health(
        &mut self,
        serial: &str,
        session: u64,
        health: PhoneHealth,
        at: Timestamp,
    ) {
        if let Some(entry) = self.session_entry(serial, session) {
            entry.live.health = Some(health);
            entry.live.health_at = Some(at);
            entry.live.probe_error = None;
        }
    }

    /// Records why a probe of session `session` failed, unless the session ended.
    pub(crate) fn set_probe_error(&mut self, serial: &str, session: u64, error: String) {
        if let Some(entry) = self.session_entry(serial, session) {
            entry.live.probe_error = Some(error);
        }
    }

    fn entry(&self, serial: &str) -> Option<&PhoneEntry> {
        self.phones.iter().find(|entry| entry.live.serial == serial)
    }

    fn entry_mut(&mut self, serial: &str) -> Option<&mut PhoneEntry> {
        self.phones
            .iter_mut()
            .find(|entry| entry.live.serial == serial)
    }

    /// The phone `serial` while its session is the one numbered `session`.
    fn session_entry(&mut self, serial: &str, session: u64) -> Option<&mut PhoneEntry> {
        self.entry_mut(serial)
            .filter(|entry| entry.session.as_ref().map(Session::id) == Some(session))
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::{self, UnboundedReceiver};

    use super::*;

    const NOW: Timestamp = Timestamp::from_millis(1_792_656_000_000);

    fn online(table: &mut PhoneTable, serial: &str) -> UnboundedReceiver<ProbeReason> {
        table.apply_state(serial, PhoneState::Device, Some("1-1"), NOW);
        let (sender, receiver) = mpsc::unbounded_channel();
        table.start_session(serial, sender).unwrap();
        receiver
    }

    #[test]
    fn a_new_phone_is_added_and_a_repeated_state_changes_nothing() {
        let mut table = PhoneTable::new(NOW);
        let change = table
            .apply_state("X1", PhoneState::Unauthorized, Some("1-2"), NOW)
            .unwrap();
        assert_eq!(change.before, PhoneStatus::Offline);
        assert_eq!(change.after, PhoneStatus::NeedsAuth);
        assert_eq!(
            table.apply_state("X1", PhoneState::Unauthorized, None, NOW),
            None
        );
        let phone = table.get("X1").unwrap();
        assert_eq!(phone.usb.as_deref(), Some("1-2"));
        assert_eq!(phone.since, NOW);
        assert_eq!(table.serials(), ["X1"]);
    }

    #[test]
    fn since_moves_only_when_the_status_changes() {
        let mut table = PhoneTable::new(NOW);
        table.apply_state("X1", PhoneState::Offline, None, NOW);
        let later = Timestamp::from_millis(NOW.millis() + 1000);
        table.apply_state("X1", PhoneState::Gone, None, later);
        assert_eq!(table.get("X1").unwrap().since, NOW, "offline to gone");
        table.apply_state("X1", PhoneState::Device, None, later);
        assert_eq!(table.get("X1").unwrap().since, later);
    }

    #[test]
    fn a_phone_without_a_session_is_not_online_for_calls() {
        let mut table = PhoneTable::new(NOW);
        assert_eq!(
            table.token("X1").unwrap_err().to_string(),
            "phone X1 is not online (unknown)"
        );
        table.apply_state("X1", PhoneState::Unauthorized, None, NOW);
        assert_eq!(
            table.token("X1").unwrap_err().to_string(),
            "phone X1 is not online (unauthorized)"
        );
        let _probes = online(&mut table, "X1");
        assert!(table.token("X1").is_ok(), "an online phone with a session");
        assert!(table.end_session("X1"));
        assert!(table.token("X1").is_err(), "the session ended");
    }

    #[test]
    fn one_probe_at_a_time_with_one_identity_refresh_behind_it() {
        let mut table = PhoneTable::new(NOW);
        let mut probes = online(&mut table, "X1");
        assert!(table.schedule("X1", ProbeReason::Connect));
        assert!(!table.schedule("X1", ProbeReason::Health), "a probe runs");
        assert!(
            table.schedule("X1", ProbeReason::Identity),
            "a refresh waits"
        );
        assert!(!table.schedule("X1", ProbeReason::Identity), "only one");
        assert_eq!(probes.try_recv().unwrap(), ProbeReason::Connect);
        assert_eq!(probes.try_recv().unwrap(), ProbeReason::Identity);
        assert!(table.any_probe_pending());
        assert_eq!(table.idle_online(), Vec::<String>::new());
        let session = table.token("X1").unwrap().id();
        table.probe_finished("X1", session);
        table.probe_started("X1", session, ProbeReason::Identity);
        assert!(
            table.schedule("X1", ProbeReason::Identity),
            "the refresh started, so another can wait"
        );
        table.probe_finished("X1", session);
        table.probe_finished("X1", session);
        assert!(!table.any_probe_pending());
        assert_eq!(table.idle_online(), ["X1"]);
    }

    #[test]
    fn a_finished_probe_of_an_old_session_does_not_count_against_the_new_one() {
        let mut table = PhoneTable::new(NOW);
        let _first = online(&mut table, "X1");
        let old = table.token("X1").unwrap().id();
        assert!(table.schedule("X1", ProbeReason::Connect));
        table.end_session("X1");
        let (sender, _second) = mpsc::unbounded_channel();
        table.start_session("X1", sender).unwrap();
        assert!(table.schedule("X1", ProbeReason::Connect));
        table.probe_finished("X1", old);
        table.set_probe_error("X1", old, "late".to_owned());
        assert!(
            table.any_probe_pending(),
            "the new session's probe is pending"
        );
        assert_eq!(table.get("X1").unwrap().probe_error, None);
    }

    /// One attempt to reach the adb server.
    struct Attempt {
        reachable: bool,
        at: Timestamp,
    }

    /// Records `attempt` and returns whether it was news.
    fn attempt(table: &mut PhoneTable, attempt: &Attempt) -> bool {
        let news = table.adb_news(attempt.reachable);
        let error = (!attempt.reachable).then(|| "refused".to_owned());
        table.set_adb(attempt.reachable, Some(41), error, attempt.at);
        news
    }

    #[test]
    fn reaching_adb_at_startup_is_not_news_but_losing_it_is() {
        let mut table = PhoneTable::new(NOW);
        assert_eq!(table.adb().error.as_deref(), Some("not connected yet"));
        let reached = Attempt {
            reachable: true,
            at: NOW,
        };
        assert!(!attempt(&mut table, &reached), "reached at startup");
        let later = Timestamp::from_millis(NOW.millis() + 5);
        let reached_later = Attempt {
            reachable: true,
            at: later,
        };
        assert!(!attempt(&mut table, &reached_later), "still reachable");
        assert_eq!(table.adb().since, NOW);
        let lost = Attempt {
            reachable: false,
            at: later,
        };
        assert!(attempt(&mut table, &lost), "lost");
        assert_eq!(table.adb().since, later);
        assert!(attempt(&mut table, &reached_later), "recovered");
    }

    #[test]
    fn failing_to_reach_adb_at_startup_is_news() {
        let mut table = PhoneTable::new(NOW);
        let refused = Attempt {
            reachable: false,
            at: NOW,
        };
        assert!(attempt(&mut table, &refused), "refused at startup");
        assert!(!attempt(&mut table, &refused), "still refused");
    }
}
