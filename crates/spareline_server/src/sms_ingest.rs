//! Copies inbound texts from every online phone's SMS and MMS providers into the database, as
//! spareline-old's `src/sms/ingest.ts` did.
//!
//! The provider is the source of truth: `dumpsys isms` (about 45 ms) says when to look, a
//! provider query (about 1.1 s) says what arrived, and a periodic reconcile catches anything the
//! trigger missed, such as an MMS, a cleared log, or a phone that was unplugged. Every write goes
//! through [`MessageStore::insert`](spareline_db::MessageStore::insert), whose unique key makes
//! storing a text again harmless.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::{pin, slice};

use futures_util::future::{self, Either};
use spareline_adb::AdbErrorKind;
use spareline_core::{
    ContentQueryError, ContentRow, ContentRows, IsmsActivity, MmsPart, MmsSignature, PhoneStatus,
    Provider, ProviderMessage, ProviderQuery, Timestamp,
};
use spareline_db::{
    AttachmentStore, DatabaseError, Message, MessageKey, NewAttachment, NewMessage, SavedAttachment,
};
use thiserror::Error;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch::{self, Sender};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio::task::{self, JoinHandle};
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::buses::Buses;
use crate::clock::Clock;
use crate::db_handle::DbHandle;
use crate::devices::{DeviceError, Devices, PhoneEvent, SessionToken};
use crate::event_log::EventLog;
use crate::shell_text;

/// How often each online phone's `dumpsys isms` is compared: 3 s.
pub const TRIGGER_INTERVAL: Duration = Duration::from_secs(3);

/// How often each online phone's whole inbox is compared with the database: 60 s.
pub const RECONCILE_INTERVAL: Duration = Duration::from_mins(1);

/// How soon a failed reconcile is tried again: 15 s, not on every tick, since each try starts
/// `app_process` on the phone a few times.
const RECONCILE_RETRY: Duration = Duration::from_secs(15);

/// Provider queries after an isms change that found nothing yet: the Messages app writes the row
/// shortly after the platform delivers the text.
const FOLLOW_UPS: u32 = 3;

/// Reconciles after which an MMS whose part can't be read is stored without that part.
const PART_ATTEMPTS: u32 = 5;

/// The shell timeout of each provider query.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// The shell timeout of the isms trigger.
const TRIGGER_TIMEOUT: Duration = Duration::from_secs(10);

/// The shell timeout of reading one MMS part.
const PART_TIMEOUT: Duration = Duration::from_mins(2);

/// The command that checks a phone still answers after a part read lost its exit code. It
/// changes nothing on the phone and prints nothing.
const ANSWERS_COMMAND: &str = "true";

/// The shell timeout of that check.
const ANSWERS_TIMEOUT: Duration = Duration::from_secs(10);

/// How many provider rows one reconcile query asks for.
const BATCH: usize = 100;

/// What [`SmsIngest`] needs.
#[derive(Clone, Debug)]
pub struct SmsIngestOptions {
    /// The phones, whose providers are read.
    pub devices: Devices,
    /// The database the texts go to.
    pub db: DbHandle,
    /// Where texts are published after they commit.
    pub buses: Buses,
    /// Where MMS parts are saved, `DATA_DIR/attachments`.
    pub attachments: AttachmentStore,
    /// The log.
    pub log: EventLog,
    /// The clock `ingested_at` and the reconcile schedule are read from.
    pub clock: Clock,
    /// How often each phone's isms is compared: [`TRIGGER_INTERVAL`] unless a test sets another.
    pub trigger_interval: Duration,
    /// How often each phone's inbox is reconciled: [`RECONCILE_INTERVAL`] unless a test sets
    /// another.
    pub reconcile_interval: Duration,
}

impl SmsIngestOptions {
    /// The options spareline-old's server used.
    #[must_use]
    pub fn new(
        devices: Devices,
        db: DbHandle,
        buses: Buses,
        attachments: AttachmentStore,
        log: EventLog,
        clock: Clock,
    ) -> Self {
        Self {
            devices,
            db,
            buses,
            attachments,
            log,
            clock,
            trigger_interval: TRIGGER_INTERVAL,
            reconcile_interval: RECONCILE_INTERVAL,
        }
    }
}

/// A step of ingest that failed.
#[derive(Debug, Error)]
#[must_use]
pub enum IngestError {
    /// The phone couldn't be reached.
    #[error(transparent)]
    Device(#[from] DeviceError),
    /// A provider query failed.
    #[error(transparent)]
    Query(#[from] ContentQueryError),
    /// The isms trigger exited with a status above 1.
    #[error("dumpsys isms exited {exit_code}")]
    Isms {
        /// The exit status.
        exit_code: u8,
    },
    /// The database or the attachment directory failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// Text ingest: a loop per phone that is ready, which watches its isms, pulls new provider rows
/// after a change, and reconciles its inbox every minute.
///
/// Create it with [`SmsIngest::new`] before the phone manager starts, then
/// [`SmsIngest::start`] it, so it sees every phone's `Ready`.
#[derive(Debug)]
pub struct SmsIngest {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "each phone's loop and the event follower run in tasks of their own that \
                      use the same ingest state as the handle"
        )
    )]
    ingest: Arc<Ingest>,
    follower: Option<JoinHandle<()>>,
}

impl SmsIngest {
    /// Ingest that hasn't started.
    #[must_use]
    pub fn new(options: SmsIngestOptions) -> Self {
        Self {
            ingest: Arc::new(Ingest {
                options,
                loops: Mutex::new(HashMap::new()),
                retired: Mutex::new(Vec::new()),
                stopping: watch::channel(false).0,
                part_failures: Mutex::new(HashMap::new()),
            }),
            follower: None,
        }
    }

    /// Starts following the phones: gives the texts stored without a number their number where
    /// it is known now, then watches every phone that is ready, and every phone once it becomes
    /// ready (not at `Connected`: until its identity probe has placed its SIMs, texts on a SIM
    /// that just moved there would get no number). Call it inside the tokio runtime; starting
    /// again does nothing.
    ///
    /// # Errors
    ///
    /// Fails when the first attribution can't be written; then nothing is watched.
    pub async fn start(&mut self) -> Result<(), DatabaseError> {
        if self.follower.is_some() {
            return Ok(());
        }
        let events = self.ingest.options.devices.subscribe();
        self.ingest.attribute_pending().await?;
        for phone in self.ingest.options.devices.list() {
            if phone.status == PhoneStatus::Online && phone.identity_at.is_some() {
                Ingest::watch(&self.ingest, &phone.serial);
            }
        }
        self.follower = Some(tokio::spawn(follow(Arc::clone(&self.ingest), events)));
        Ok(())
    }

    /// Stops following the phones, cancels the cycles in flight, and stops every loop, the ones
    /// unwatched just before included.
    pub async fn stop(mut self) {
        if let Some(follower) = self.follower.take() {
            follower.abort();
            match follower.await {
                Ok(()) | Err(_) => {}
            }
        }
        let mut loops: Vec<LoopHandle> = self
            .ingest
            .loops()
            .drain()
            .map(|(_, phone_loop)| phone_loop)
            .collect();
        loops.append(&mut self.ingest.retired());
        for phone_loop in &loops {
            phone_loop.stopped.store(true, Ordering::Relaxed);
        }
        self.ingest.stopping.send_replace(true);
        for phone_loop in loops {
            let running = phone_loop.state.lock().await;
            phone_loop.timer.abort();
            drop(running);
            match phone_loop.timer.await {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Runs one cycle for the phone now: the trigger check, or a reconcile when one is due. When
    /// a cycle is already running, waits for it instead.
    pub async fn tick(&self, serial: &str) {
        self.ingest.tick(serial).await;
    }

    /// Compares both providers' whole inbox with the database and stores what is missing.
    /// Returns how many texts were new. An MMS needs two reads that agree, so a phone this
    /// ingest doesn't watch has none stored this way.
    ///
    /// # Errors
    ///
    /// Fails when the phone can't be read or the database fails.
    pub async fn reconcile(&self, serial: &str) -> Result<usize, IngestError> {
        let state = self.ingest.loop_state(serial);
        match state {
            Some(state) => {
                let mut state = state.lock().await;
                self.ingest.reconcile(serial, Some(&mut state)).await
            }
            None => self.ingest.reconcile(serial, None).await,
        }
    }

    /// Gives the texts stored without a number their number, at a new seq, and publishes them
    /// again so clients following that number see them.
    ///
    /// # Errors
    ///
    /// Fails when the database does.
    pub async fn attribute_pending(&self) -> Result<(), DatabaseError> {
        self.ingest.attribute_pending().await
    }

    /// Finishes once no phone's cycle is running, for tests.
    pub async fn settled(&self) {
        loop {
            let states: Vec<_> = self
                .ingest
                .loops()
                .values()
                .map(|phone_loop| Arc::clone(&phone_loop.state))
                .collect();
            if states.iter().all(|state| state.try_lock().is_ok()) {
                return;
            }
            for state in states {
                drop(state.lock().await);
            }
        }
    }

    /// Whether the phone's loop runs, for tests.
    #[must_use]
    pub fn is_watching(&self, serial: &str) -> bool {
        self.ingest.loops().contains_key(serial)
    }
}

/// Follows the phone events: a ready phone is watched, one that disconnects isn't, and a change
/// of SIMs or numbers gives pending texts their number.
#[cfg_attr(
    dylint_lib = "trellis_lints",
    expect(
        shared_ownership,
        reason = "the follower runs in a task of its own and starts loops that share the \
                  ingest state"
    )
)]
async fn follow(ingest: Arc<Ingest>, mut events: Receiver<PhoneEvent>) {
    loop {
        match events.recv().await {
            Ok(PhoneEvent::Ready { serial }) => Ingest::watch(&ingest, &serial),
            Ok(PhoneEvent::Disconnected { serial }) => ingest.unwatch(&serial),
            Ok(PhoneEvent::SubsChanged { .. }) => ingest.attribute_pending_logged().await,
            Ok(PhoneEvent::Connected { .. }) => {}
            Err(RecvError::Lagged(_)) => {
                for phone in ingest.options.devices.list() {
                    if phone.status != PhoneStatus::Online {
                        ingest.unwatch(&phone.serial);
                    } else if phone.identity_at.is_some() {
                        Ingest::watch(&ingest, &phone.serial);
                    }
                }
                ingest.attribute_pending_logged().await;
            }
            Err(RecvError::Closed) => return,
        }
    }
}

/// The state ingest shares between its tasks.
#[derive(Debug)]
struct Ingest {
    options: SmsIngestOptions,
    loops: Mutex<HashMap<String, LoopHandle>>,
    /// Unwatched loops whose task may still be running a cycle.
    retired: Mutex<Vec<LoopHandle>>,
    /// Set once ingest stops, which cancels the cycles in flight.
    stopping: Sender<bool>,
    /// Failed reconcile reads of each MMS part, kept while its phone is away and forgotten once
    /// the MMS leaves the inbox.
    part_failures: Mutex<HashMap<PartKey, u32>>,
}

/// A watched phone's loop: its state, which a cycle holds while it runs, and its timer.
#[derive(Debug)]
struct LoopHandle {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the loop's timer task and every caller of tick or reconcile take turns \
                      with the one state"
        )
    )]
    state: Arc<AsyncMutex<PhoneLoop>>,
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the timer task reads the flag that unwatch sets from another task"
        )
    )]
    stopped: Arc<AtomicBool>,
    timer: JoinHandle<()>,
}

/// One phone's ingest state between cycles.
#[derive(Debug)]
struct PhoneLoop {
    serial: String,
    /// The last isms read; a difference from it sets the follow-ups.
    signature: Option<IsmsActivity>,
    follow_ups: u32,
    /// When the next reconcile is due; `None` until the first, which the first cycle runs.
    reconcile_due: Option<Timestamp>,
    /// MMS read once, waiting for a second read that agrees.
    unconfirmed: MmsReads,
    /// MMS two reads agreed on that have a part not readable yet, retried only on reconcile.
    confirmed: MmsReads,
}

/// MMS by provider `_id`, with what the last read showed, in the order first read.
#[derive(Debug, Default)]
struct MmsReads {
    reads: Vec<MmsRead>,
}

#[derive(Debug)]
struct MmsRead {
    id: i64,
    signature: MmsSignature,
}

impl MmsReads {
    fn get(&self, id: i64) -> Option<&MmsSignature> {
        self.reads
            .iter()
            .find(|read| read.id == id)
            .map(|read| &read.signature)
    }

    fn set(&mut self, id: i64, signature: MmsSignature) {
        match self.reads.iter_mut().find(|read| read.id == id) {
            Some(read) => read.signature = signature,
            None => self.reads.push(MmsRead { id, signature }),
        }
    }

    fn remove(&mut self, id: i64) {
        self.reads.retain(|read| read.id != id);
    }

    fn ids(&self) -> Vec<i64> {
        self.reads.iter().map(|read| read.id).collect()
    }

    fn is_empty(&self) -> bool {
        self.reads.is_empty()
    }
}

/// Why an MMS part read gave no bytes to keep.
#[derive(Debug, PartialEq, Eq)]
enum PartFailure {
    /// The part couldn't be read, with the reason: the phone answered with a non-zero exit, an
    /// error on stderr or no bytes, or the call to the phone failed at the 2-minute timeout,
    /// broke adb's protocol, such as with output over its 32 MiB cap, or ended without its exit
    /// code. A part that fails that way tends to fail every time, so only this counts toward
    /// giving up on the part.
    Unreadable(String),
    /// The call says nothing about the part: the phone wasn't online, it disconnected during
    /// the call or its session had ended by the time the call failed, the adb server couldn't
    /// be reached, or the adb server refused the call, as it does for a phone that is offline
    /// or not found. The cycle fails and is tried again.
    Device(DeviceError),
}

impl PartFailure {
    /// What a failed part read through the session `token` follows means for the part, by the
    /// kind of failure. A shell that ended without its exit code still needs the phone to
    /// answer afterwards to count, which [`Ingest::call_failure`] checks.
    fn of_call(error: DeviceError, token: &SessionToken) -> Self {
        match &error {
            DeviceError::NotOnline { .. } | DeviceError::Disconnected { .. } => Self::Device(error),
            DeviceError::Adb(_) if token.is_ended() => Self::Device(error),
            DeviceError::Adb(adb) => match adb.kind() {
                AdbErrorKind::Timeout | AdbErrorKind::Protocol | AdbErrorKind::Closed => {
                    Self::Unreadable(adb.to_string())
                }
                AdbErrorKind::Connect | AdbErrorKind::Fail => Self::Device(error),
            },
        }
    }
}

/// An MMS part on a phone.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PartKey {
    serial: String,
    mms: i64,
    part: i64,
}

/// Provider rows made into texts ready to store.
struct Built {
    messages: Vec<NewMessage>,
    /// The row ids that need no more reading, in order, up to the first that does (an MMS not
    /// confirmed yet, or with a part not readable yet); the cursor may move that far.
    complete: Vec<i64>,
}

/// What reads provider rows, which decides whether a part that can't be read counts toward
/// giving up on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadBy {
    /// The trigger and its follow-ups, which don't count.
    Trigger,
    /// A reconcile, which counts.
    Reconcile,
}

/// An MMS row worth reading further.
struct MmsRow<'rows> {
    id: i64,
    row: &'rows ContentRow,
}

impl Ingest {
    fn loops(&self) -> MutexGuard<'_, HashMap<String, LoopHandle>> {
        self.loops
            .lock()
            .expect("no task panicked while holding the ingest loops")
    }

    fn retired(&self) -> MutexGuard<'_, Vec<LoopHandle>> {
        self.retired
            .lock()
            .expect("no task panicked while holding the retired loops")
    }

    fn part_failures(&self) -> MutexGuard<'_, HashMap<PartKey, u32>> {
        self.part_failures
            .lock()
            .expect("no task panicked while counting part failures")
    }

    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "a caller of reconcile takes its turn with the state the loop's timer task \
                      also holds"
        )
    )]
    fn loop_state(&self, serial: &str) -> Option<Arc<AsyncMutex<PhoneLoop>>> {
        self.loops()
            .get(serial)
            .map(|phone_loop| Arc::clone(&phone_loop.state))
    }

    /// Starts the phone's loop, unless it runs: a cycle now, which reconciles, since a phone
    /// that (re)connects may hold texts that arrived while it was unplugged, then one every
    /// trigger interval.
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the loop's timer task keeps the ingest state for as long as the phone \
                      is watched"
        )
    )]
    fn watch(ingest: &Arc<Self>, serial: &str) {
        let mut loops = ingest.loops();
        if loops.contains_key(serial) {
            return;
        }
        let state = Arc::new(AsyncMutex::new(PhoneLoop {
            serial: serial.to_owned(),
            signature: None,
            follow_ups: 0,
            reconcile_due: None,
            unconfirmed: MmsReads::default(),
            confirmed: MmsReads::default(),
        }));
        let first = Arc::clone(&state)
            .try_lock_owned()
            .expect("nothing else holds a new loop's state");
        let stopped = Arc::new(AtomicBool::new(false));
        let timer = tokio::spawn(run_loop(
            Arc::clone(ingest),
            Arc::clone(&state),
            first,
            Arc::clone(&stopped),
        ));
        loops.insert(
            serial.to_owned(),
            LoopHandle {
                state,
                stopped,
                timer,
            },
        );
    }

    /// Stops the phone's loop after the cycle in flight, if any. The loop is kept until its
    /// task ends, so [`SmsIngest::stop`] still waits for it. Its part failures are kept, as
    /// spareline-old kept them, so a phone that keeps dropping off USB still gives up on a part
    /// after five reconciles; each is forgotten once its MMS leaves the inbox.
    fn unwatch(&self, serial: &str) {
        let Some(phone_loop) = self.loops().remove(serial) else {
            return;
        };
        phone_loop.stopped.store(true, Ordering::Relaxed);
        let mut retired = self.retired();
        retired.retain(|old| !old.timer.is_finished());
        retired.push(phone_loop);
    }

    async fn tick(&self, serial: &str) {
        let Some((state, stopped)) = self.loops().get(serial).map(|phone_loop| {
            (
                Arc::clone(&phone_loop.state),
                Arc::clone(&phone_loop.stopped),
            )
        }) else {
            return;
        };
        self.tick_loop(&state, &stopped).await;
    }

    /// Runs a cycle of the loop with `state`, unless it stopped, or waits for the one running.
    async fn tick_loop(&self, state: &AsyncMutex<PhoneLoop>, stopped: &AtomicBool) {
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        match state.try_lock() {
            Ok(mut phone_loop) => self.run_cycle(&mut phone_loop, stopped).await,
            Err(_) => drop(state.lock().await),
        }
    }

    /// Runs a cycle and logs its failure, unless the loop stopped meanwhile. Stopping ingest
    /// cancels the cycle: whatever it stored is committed and published already, since both
    /// happen under the database lock, and the rest is read again on the next start.
    async fn run_cycle(&self, phone_loop: &mut PhoneLoop, stopped: &AtomicBool) {
        let serial = phone_loop.serial.clone();
        let mut stopping = self.stopping.subscribe();
        let cycle = pin::pin!(self.cycle(phone_loop));
        let stop = pin::pin!(async move {
            while !*stopping.borrow_and_update() {
                if stopping.changed().await.is_err() {
                    return;
                }
            }
        });
        let Either::Left((Err(error), _)) = future::select(cycle, stop).await else {
            return;
        };
        if !stopped.load(Ordering::Relaxed) {
            self.options
                .log
                .event("sms-ingest-failed")
                .field("serial", &serial)
                .field("error", &error.to_string())
                .write();
        }
    }

    async fn cycle(&self, phone_loop: &mut PhoneLoop) -> Result<(), IngestError> {
        let now = self.options.clock.now();
        if phone_loop.reconcile_due.is_none_or(|due| now >= due) {
            phone_loop.reconcile_due =
                Some(now.plus(self.options.reconcile_interval.min(RECONCILE_RETRY)));
            let serial = phone_loop.serial.clone();
            phone_loop.signature = Some(self.signature(&serial).await?);
            self.reconcile(&serial, Some(&mut *phone_loop)).await?;
            phone_loop.reconcile_due = Some(
                self.options
                    .clock
                    .now()
                    .plus(self.options.reconcile_interval),
            );
            return Ok(());
        }
        let signature = self.signature(&phone_loop.serial).await?;
        if phone_loop
            .signature
            .as_ref()
            .is_some_and(|last| *last != signature)
        {
            phone_loop.follow_ups = FOLLOW_UPS;
        }
        phone_loop.signature = Some(signature);
        if !phone_loop.unconfirmed.is_empty() {
            self.confirm(phone_loop).await?;
        }
        if phone_loop.follow_ups == 0 {
            return Ok(());
        }
        phone_loop.follow_ups -= 1;
        let mut added = self.pull_new(phone_loop, Provider::Sms).await?;
        if added == 0 {
            added = self.pull_new(phone_loop, Provider::Mms).await?;
        }
        if added > 0 {
            phone_loop.follow_ups = 0;
        }
        Ok(())
    }

    async fn signature(&self, serial: &str) -> Result<IsmsActivity, IngestError> {
        let output = self
            .options
            .devices
            .shell(
                serial,
                spareline_core::ISMS_TRIGGER_COMMAND,
                TRIGGER_TIMEOUT,
            )
            .await?;
        if output.exit_code > 1 {
            return Err(IngestError::Isms {
                exit_code: output.exit_code,
            });
        }
        Ok(spareline_core::parse_isms(&shell_text::shell_text(
            &output.stdout,
        )))
    }

    async fn reconcile(
        &self,
        serial: &str,
        mut phone_loop: Option<&mut PhoneLoop>,
    ) -> Result<usize, IngestError> {
        let mut added = 0;
        for provider in [Provider::Sms, Provider::Mms] {
            added += self
                .reconcile_provider(serial, provider, phone_loop.as_deref_mut())
                .await?;
        }
        Ok(added)
    }

    /// Stores the provider rows after the cursor. Returns how many texts were new.
    async fn pull_new(
        &self,
        phone_loop: &mut PhoneLoop,
        provider: Provider,
    ) -> Result<usize, IngestError> {
        let serial = phone_loop.serial.clone();
        let Some(phone_id) = self.phone_id(&serial).await? else {
            return Ok(0);
        };
        let stored = stored_provider(provider);
        let cursor = self
            .options
            .db
            .call(move |db| db.messages().ingest_cursor(phone_id, stored))
            .await?;
        let rows = self
            .query_rows(
                &serial,
                &ProviderQuery::After {
                    provider,
                    after_id: cursor,
                },
            )
            .await?;
        if rows.is_empty() {
            return Ok(0);
        }
        let built = self
            .build(&serial, provider, &rows, Some(phone_loop), ReadBy::Trigger)
            .await?;
        let added = self.store(&serial, phone_id, built.messages).await?;
        let next = built.complete.iter().copied().fold(cursor, i64::max);
        if next > cursor {
            self.set_cursor(phone_id, provider, next).await?;
        }
        Ok(added)
    }

    /// Reads the MMS seen once again, in batches of 100, and stores the ones that read the same.
    /// One that is gone, such as a notification replaced by its download, has nothing left to
    /// confirm, and its part failures are forgotten.
    async fn confirm(&self, phone_loop: &mut PhoneLoop) -> Result<(), IngestError> {
        let serial = phone_loop.serial.clone();
        let Some(phone_id) = self.phone_id(&serial).await? else {
            return Ok(());
        };
        let ids = phone_loop.unconfirmed.ids();
        for batch in ids.chunks(BATCH) {
            let rows = self
                .query_rows(
                    &serial,
                    &ProviderQuery::ByIds {
                        provider: Provider::Mms,
                        ids: batch.to_vec(),
                    },
                )
                .await?;
            let present: HashSet<i64> = rows.iter().filter_map(|row| row.integer("_id")).collect();
            for &id in batch {
                if !present.contains(&id) {
                    phone_loop.unconfirmed.remove(id);
                    self.forget_part_failures(&serial, |mms| mms == id);
                }
            }
            let built = self
                .build(
                    &serial,
                    Provider::Mms,
                    &rows,
                    Some(&mut *phone_loop),
                    ReadBy::Trigger,
                )
                .await?;
            self.store(&serial, phone_id, built.messages).await?;
        }
        Ok(())
    }

    /// Forgets the part failures of the phone's MMS for which `forget` holds.
    fn forget_part_failures(&self, serial: &str, forget: impl Fn(i64) -> bool) {
        self.part_failures()
            .retain(|key, _| key.serial != serial || !forget(key.mms));
    }

    async fn reconcile_provider(
        &self,
        serial: &str,
        provider: Provider,
        mut phone_loop: Option<&mut PhoneLoop>,
    ) -> Result<usize, IngestError> {
        let Some(phone_id) = self.phone_id(serial).await? else {
            return Ok(0);
        };
        let rows = self
            .query_rows(serial, &ProviderQuery::Keys { provider })
            .await?;
        let keys = spareline_core::inbox_keys(&rows, provider);
        if provider == Provider::Mms {
            let inbox: HashSet<i64> = keys.iter().map(|key| key.provider_id).collect();
            self.forget_part_failures(serial, |mms| !inbox.contains(&mms));
        }
        let stored = self.stored_keys(phone_id, provider).await?;
        let missing: Vec<i64> = keys
            .iter()
            .filter(|key| !stored.contains(&message_key(key.provider_id, key.received_at)))
            .map(|key| key.provider_id)
            .collect();
        let mut added = 0;
        for ids in missing.chunks(BATCH) {
            let rows = self
                .query_rows(
                    serial,
                    &ProviderQuery::ByIds {
                        provider,
                        ids: ids.to_vec(),
                    },
                )
                .await?;
            let built = self
                .build(
                    serial,
                    provider,
                    &rows,
                    phone_loop.as_deref_mut(),
                    ReadBy::Reconcile,
                )
                .await?;
            added += self.store(serial, phone_id, built.messages).await?;
        }
        let highest = keys.iter().map(|key| key.provider_id).fold(0, i64::max);
        self.set_cursor(phone_id, provider, highest).await?;
        Ok(added)
    }

    /// Runs a provider query and returns its rows, logging the rows that didn't parse by their
    /// id only, so senders and bodies stay out of the log.
    async fn query_rows(
        &self,
        serial: &str,
        query: &ProviderQuery,
    ) -> Result<Vec<ContentRow>, IngestError> {
        let output = self
            .options
            .devices
            .shell(serial, &query.command(), QUERY_TIMEOUT)
            .await?;
        let rows: ContentRows = spareline_core::check_content_output(
            &shell_text::shell_text(&output.stdout),
            &shell_text::shell_text(&output.stderr),
            i32::from(output.exit_code),
            query.columns(),
        )?;
        if let Some(first) = rows.malformed.first() {
            let mut line = self
                .options
                .log
                .event("sms-rows-malformed")
                .field("serial", serial)
                .field("rows", &rows.malformed.len());
            if let Some(sample) = row_id_prefix(first) {
                line = line.field("sample", sample);
            }
            line.write();
        }
        Ok(rows.rows)
    }

    /// Turns provider rows into texts ready to store, reading MMS addresses and parts.
    async fn build(
        &self,
        serial: &str,
        provider: Provider,
        rows: &[ContentRow],
        mut phone_loop: Option<&mut PhoneLoop>,
        read_by: ReadBy,
    ) -> Result<Built, IngestError> {
        let mut parsed: Vec<ProviderMessage> = Vec::new();
        let mut invalid: HashSet<i64> = HashSet::new();
        let mut done: HashSet<i64> = HashSet::new();
        match provider {
            Provider::Sms => {
                for row in rows {
                    match spareline_core::sms_message(row) {
                        Ok(message) => parsed.push(message),
                        Err(error) => {
                            if let Some(id) = row.integer("_id") {
                                invalid.insert(id);
                            }
                            self.options
                                .log
                                .event("sms-row-invalid")
                                .field("serial", serial)
                                .field("id", &row.get("_id"))
                                .field("error", &error.to_string())
                                .write();
                        }
                    }
                }
            }
            Provider::Mms => {
                let read = self
                    .read_mms(
                        serial,
                        rows,
                        phone_loop.as_deref_mut(),
                        &mut invalid,
                        &mut done,
                        read_by,
                    )
                    .await?;
                parsed.extend(read);
            }
        }
        let mut ready: Vec<NewMessage> = Vec::new();
        for message in parsed {
            let Some(attachments) = self.read_parts(serial, &message, read_by).await? else {
                continue;
            };
            if message.provider == Provider::Mms
                && let Some(phone_loop) = phone_loop.as_deref_mut()
            {
                phone_loop.confirmed.remove(message.provider_id);
            }
            let new = NewMessage {
                provider: stored_provider(message.provider),
                provider_id: message.provider_id,
                sub_id: message.sub_id,
                sender: message.sender,
                body: message.body,
                subject: message.subject,
                sent_at: message.sent_at,
                received_at: message.received_at,
                attachments,
            };
            match ready
                .iter_mut()
                .find(|existing| existing.provider_id == new.provider_id)
            {
                Some(existing) => *existing = new,
                None => ready.push(new),
            }
        }
        let mut complete = Vec::new();
        for row in rows {
            let Some(id) = row.integer("_id") else {
                continue;
            };
            if invalid.contains(&id) {
                continue;
            }
            if !done.contains(&id) && !ready.iter().any(|message| message.provider_id == id) {
                break;
            }
            complete.push(id);
        }
        Ok(Built {
            messages: ready,
            complete,
        })
    }

    /// MMS rows with their addresses and parts. AOSP's download code inserts the row and then
    /// updates its date and subscription, so a read can catch a half-written MMS; each one is
    /// returned only once two reads in a row agree on its date, sub id and parts. Without a loop
    /// (a one-off reconcile) there is no earlier read, so nothing is returned.
    async fn read_mms(
        &self,
        serial: &str,
        rows: &[ContentRow],
        mut phone_loop: Option<&mut PhoneLoop>,
        invalid: &mut HashSet<i64>,
        done: &mut HashSet<i64>,
        read_by: ReadBy,
    ) -> Result<Vec<ProviderMessage>, IngestError> {
        let stored = match self.phone_id(serial).await? {
            Some(phone_id) => self.stored_keys(phone_id, Provider::Mms).await?,
            None => HashSet::new(),
        };
        let mut valid: Vec<MmsRow<'_>> = Vec::new();
        for row in rows {
            let Some(id) = row.integer("_id") else {
                self.options
                    .log
                    .event("mms-row-invalid")
                    .field("serial", serial)
                    .field("error", "no integer _id")
                    .write();
                continue;
            };
            let key = spareline_core::inbox_keys(slice::from_ref(row), Provider::Mms);
            if let Some(key) = key.first()
                && stored.contains(&message_key(key.provider_id, key.received_at))
            {
                done.insert(id);
                if let Some(phone_loop) = phone_loop.as_deref_mut() {
                    phone_loop.unconfirmed.remove(id);
                    phone_loop.confirmed.remove(id);
                }
                continue;
            }
            valid.push(MmsRow { id, row });
        }
        if valid.is_empty() {
            return Ok(Vec::new());
        }
        let part_rows = self
            .query_rows(
                serial,
                &ProviderQuery::MmsParts {
                    mms_ids: valid.iter().map(|mms| mms.id).collect(),
                },
            )
            .await?;
        let parts: Vec<MmsPart> = part_rows
            .iter()
            .filter_map(|row| spareline_core::mms_part(row).ok())
            .collect();
        let mut messages = Vec::new();
        for MmsRow { id, row } in valid {
            let own: Vec<MmsPart> = parts
                .iter()
                .filter(|part| part.mid == id)
                .cloned()
                .collect();
            let seen = spareline_core::mms_signature(row, &own);
            let Some(phone_loop) = phone_loop.as_deref_mut() else {
                continue;
            };
            if phone_loop.confirmed.get(id) == Some(&seen) {
                if read_by == ReadBy::Trigger {
                    continue;
                }
            } else if phone_loop.unconfirmed.get(id) == Some(&seen) {
                phone_loop.unconfirmed.remove(id);
                phone_loop.confirmed.set(id, seen);
            } else {
                phone_loop.confirmed.remove(id);
                phone_loop.unconfirmed.set(id, seen);
                continue;
            }
            let addresses = self
                .query_rows(serial, &ProviderQuery::MmsAddresses { mms_id: id })
                .await?;
            match spareline_core::mms_message(row, &addresses, &own) {
                Ok(message) => messages.push(message),
                Err(error) => {
                    invalid.insert(id);
                    self.options
                        .log
                        .event("mms-row-invalid")
                        .field("serial", serial)
                        .field("id", &id)
                        .field("error", &error.to_string())
                        .write();
                }
            }
        }
        Ok(messages)
    }

    /// Reads each binary part of `message` and saves it. Returns `None` when a part can't be
    /// read yet and the message should be tried again. Only reconciles count toward giving up,
    /// so the follow-up reads after a trigger don't use up the attempts.
    ///
    /// # Errors
    ///
    /// Fails when the phone went away while a part was read, which counts toward nothing, or a
    /// part can't be saved.
    async fn read_parts(
        &self,
        serial: &str,
        message: &ProviderMessage,
        read_by: ReadBy,
    ) -> Result<Option<Vec<NewAttachment>>, IngestError> {
        let mut stored = Vec::new();
        for part in &message.parts {
            let key = PartKey {
                serial: serial.to_owned(),
                mms: message.provider_id,
                part: part.part_id,
            };
            match self.read_part(serial, part.part_id).await {
                Ok(bytes) => {
                    self.part_failures().remove(&key);
                    let saved = self.save(bytes).await?;
                    stored.push(NewAttachment {
                        content_type: part.content_type.clone(),
                        size: saved.size,
                        sha256: saved.sha256,
                        filename: part.filename.clone(),
                    });
                }
                Err(PartFailure::Device(error)) => return Err(error.into()),
                Err(PartFailure::Unreadable(reason)) => {
                    let failures = self.part_failures().get(&key).copied().unwrap_or(0)
                        + u32::from(read_by == ReadBy::Reconcile);
                    self.options
                        .log
                        .event("mms-part-unreadable")
                        .field("serial", serial)
                        .field("mms", &message.provider_id)
                        .field("part", &part.part_id)
                        .field("attempt", &failures)
                        .field("reason", &reason)
                        .write();
                    if failures < PART_ATTEMPTS {
                        self.part_failures().insert(key, failures);
                        return Ok(None);
                    }
                    self.part_failures().remove(&key);
                }
            }
        }
        Ok(Some(stored))
    }

    /// `content read` of one part over the shell protocol: binary stdout, with stderr and the
    /// exit status apart. The tool prints provider errors on stderr and still exits 0, so any
    /// stderr makes the part unreadable. A call that times out, breaks adb's protocol or ends
    /// without its exit code while the phone still answers makes it unreadable too, as
    /// spareline-old counted it; any other failed call fails the cycle instead, as
    /// [`Ingest::call_failure`] sorts them.
    async fn read_part(&self, serial: &str, part_id: i64) -> Result<Vec<u8>, PartFailure> {
        let devices = &self.options.devices;
        let token = devices.token(serial).map_err(PartFailure::Device)?;
        let read = devices
            .shell_in(
                &token,
                serial,
                &spareline_core::read_part_command(part_id),
                PART_TIMEOUT,
            )
            .await;
        let output = match read {
            Ok(output) => output,
            Err(error) => return Err(self.call_failure(serial, error, &token).await),
        };
        spareline_core::check_part_read(
            i32::from(output.exit_code),
            &output.stdout,
            &output.stderr,
        )
        .map_err(|reason| PartFailure::Unreadable(reason.to_string()))?;
        Ok(output.stdout)
    }

    /// What a failed part read means for the part, as [`PartFailure::of_call`] sorts it, except
    /// that a shell that ended without its exit code counts only when the phone still answers
    /// in the same session right after: a USB unplug or an adb server crash mid-read ends the
    /// shell the same way, before the session ends.
    async fn call_failure(
        &self,
        serial: &str,
        error: DeviceError,
        token: &SessionToken,
    ) -> PartFailure {
        let lost_exit_code =
            matches!(&error, DeviceError::Adb(adb) if adb.kind() == AdbErrorKind::Closed);
        let failure = PartFailure::of_call(error.clone(), token);
        if lost_exit_code
            && matches!(failure, PartFailure::Unreadable(_))
            && !self.still_answers(serial, token).await
        {
            return PartFailure::Device(error);
        }
        failure
    }

    /// Whether the phone answers a command that changes nothing, in the session `token`
    /// follows, which hasn't ended by the time it answers.
    async fn still_answers(&self, serial: &str, token: &SessionToken) -> bool {
        let answered = self
            .options
            .devices
            .shell_in(token, serial, ANSWERS_COMMAND, ANSWERS_TIMEOUT)
            .await;
        matches!(answered, Ok(output) if output.exit_code == 0) && !token.is_ended()
    }

    /// Saves a part's bytes under their SHA-256, off the async threads.
    async fn save(&self, bytes: Vec<u8>) -> Result<SavedAttachment, DatabaseError> {
        let attachments = self.options.attachments.clone();
        let now = self.options.clock.now();
        task::spawn_blocking(move || attachments.save(&bytes, now))
            .await
            .expect("saving an attachment doesn't panic")
    }

    /// Stores `messages` from the phone and publishes the new ones while the database is still
    /// held, so texts reach the bus in seq order whichever loop stores them: a stream drops a
    /// seq at or below its cursor, and would lose one published late. Returns how many were new.
    async fn store(
        &self,
        serial: &str,
        phone_id: i64,
        messages: Vec<NewMessage>,
    ) -> Result<usize, IngestError> {
        if messages.is_empty() {
            return Ok(0);
        }
        let now = self.options.clock.now();
        let buses = self.options.buses.clone();
        let inserted = self
            .options
            .db
            .call(move |db| {
                let inserted = db.messages().insert(phone_id, &messages, now)?;
                buses.publish_texts(&inserted);
                Ok::<Vec<Message>, DatabaseError>(inserted)
            })
            .await?;
        if inserted.is_empty() {
            return Ok(0);
        }
        self.options
            .log
            .event("sms-ingested")
            .field("serial", serial)
            .field("seqs", &seqs(&inserted))
            .write();
        Ok(inserted.len())
    }

    /// Gives pending texts their number, publishing the moved ones while the database is still
    /// held, as [`Ingest::store`] does.
    async fn attribute_pending(&self) -> Result<(), DatabaseError> {
        let buses = self.options.buses.clone();
        let moved = self
            .options
            .db
            .call(move |db| {
                let moved = db.messages().attribute_pending()?;
                buses.publish_texts(&moved);
                Ok::<Vec<Message>, DatabaseError>(moved)
            })
            .await?;
        if moved.is_empty() {
            return Ok(());
        }
        self.options
            .log
            .event("sms-attributed")
            .field("seqs", &seqs(&moved))
            .write();
        Ok(())
    }

    /// Attributes the pending texts, logging a failure, which the next change retries.
    async fn attribute_pending_logged(&self) {
        if let Err(error) = self.attribute_pending().await {
            self.options
                .log
                .event("sms-ingest-failed")
                .field("error", &error.to_string())
                .write();
        }
    }

    async fn phone_id(&self, serial: &str) -> Result<Option<i64>, DatabaseError> {
        let serial = serial.to_owned();
        self.options
            .db
            .call(move |db| db.inventory().phone_id(&serial))
            .await
    }

    async fn stored_keys(
        &self,
        phone_id: i64,
        provider: Provider,
    ) -> Result<HashSet<MessageKey>, DatabaseError> {
        let provider = stored_provider(provider);
        self.options
            .db
            .call(move |db| db.messages().stored_keys(phone_id, provider))
            .await
    }

    async fn set_cursor(
        &self,
        phone_id: i64,
        provider: Provider,
        cursor: i64,
    ) -> Result<(), DatabaseError> {
        let provider = stored_provider(provider);
        self.options
            .db
            .call(move |db| db.messages().set_ingest_cursor(phone_id, provider, cursor))
            .await
    }
}

/// A watched phone's loop: the first cycle, already holding the state, then a cycle every
/// trigger interval until the phone is unwatched.
#[cfg_attr(
    dylint_lib = "trellis_lints",
    expect(
        shared_ownership,
        reason = "the loop runs in a task of its own for as long as the phone is watched, and \
                  unwatch sets its stop flag from the follower's task"
    )
)]
async fn run_loop(
    ingest: Arc<Ingest>,
    state: Arc<AsyncMutex<PhoneLoop>>,
    mut first: OwnedMutexGuard<PhoneLoop>,
    stopped: Arc<AtomicBool>,
) {
    ingest.run_cycle(&mut first, &stopped).await;
    drop(first);
    let interval = ingest.options.trigger_interval;
    let mut ticks = time::interval_at(Instant::now() + interval, interval);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        ingest.tick_loop(&state, &stopped).await;
    }
}

/// The database's name for a provider.
fn stored_provider(provider: Provider) -> spareline_db::Provider {
    match provider {
        Provider::Sms => spareline_db::Provider::Sms,
        Provider::Mms => spareline_db::Provider::Mms,
    }
}

/// The key a stored text of this provider id and receive time has.
fn message_key(provider_id: i64, received_at: Timestamp) -> MessageKey {
    MessageKey {
        provider_id,
        received_at: received_at.to_string(),
    }
}

/// The seqs of `messages`, for the log.
fn seqs(messages: &[Message]) -> Vec<i64> {
    messages.iter().map(|message| message.seq).collect()
}

/// The `Row: N _id=M` a row line starts with, the only part of a malformed row that is logged.
fn row_id_prefix(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("Row: ")?;
    let index_digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if index_digits == 0 {
        return None;
    }
    let rest = rest[index_digits..].strip_prefix(" _id=")?;
    let id_digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if id_digits == 0 {
        return None;
    }
    let length = "Row: ".len() + index_digits + " _id=".len() + id_digits;
    Some(&line[..length])
}

#[cfg(test)]
mod tests;
