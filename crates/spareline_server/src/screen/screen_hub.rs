//! What every phone's relay shares: the phones, the database, the clock, the log and the table
//! of relays.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use spareline_core::{PhoneStatus, Timestamp};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::AbortHandle;
use tokio::time::Instant;

use crate::clock::Clock;
use crate::db_handle::DbHandle;
use crate::devices::{Devices, PhoneEvent};
use crate::event_log::EventLog;
use crate::monitor::ScreenSessions;
use crate::screen::frame_rate::FrameRate;
use crate::screen::phone_screen::PhoneScreen;
use crate::screen::screen_command::ScreenCommand;
use crate::screen::screen_session::{ScreenSession, ScreenState};
use crate::screen::screen_timings::ScreenTimings;
use crate::screen::task_guard::TaskGuards;

/// The state every relay and the [`Screens`](crate::Screens) handle share.
#[derive(Debug)]
pub(crate) struct ScreenHub {
    /// The phones.
    pub(crate) devices: Devices,
    /// The database, for `control_sessions`.
    pub(crate) db: DbHandle,
    /// The time the audit rows and the delivery rules read.
    pub(crate) clock: Clock,
    /// The service's log.
    pub(crate) log: EventLog,
    /// The monitor's list of phones with a session, which the `stay_on_off` rule reads.
    pub(crate) monitor_sessions: ScreenSessions,
    /// How long each step waits.
    pub(crate) timings: ScreenTimings,
    /// The scrcpy-server jar, pushed at every session start.
    pub(crate) jar: &'static [u8],
    /// When the hub started; every relay's stats ticks fall on whole intervals after it.
    pub(crate) epoch: Instant,
    table: Mutex<ScreenTable>,
    tasks: TaskGuards,
    next_id: AtomicU64,
    watcher: Mutex<Option<AbortHandle>>,
}

/// The relays, one per phone with a session or viewers, and what `GET /api/v1/screens` shows of
/// each, in the order their phones were first attached, as the old `Map` kept them.
#[derive(Debug)]
struct ScreenTable {
    relays: HashMap<String, RelaySlot>,
    sessions: Vec<ScreenSession>,
    stopping: bool,
}

/// One phone's relay task, as the table knows it.
#[derive(Debug)]
struct RelaySlot {
    id: u64,
    commands: UnboundedSender<ScreenCommand>,
}

/// Why the hub refused a viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The service is shutting down.
    ShuttingDown,
    /// The phone isn't online.
    Offline,
}

impl ScreenHub {
    /// A hub with no relays.
    pub(crate) fn new(
        devices: Devices,
        db: DbHandle,
        clock: Clock,
        log: EventLog,
        monitor_sessions: ScreenSessions,
        timings: ScreenTimings,
        jar: &'static [u8],
    ) -> Self {
        Self {
            devices,
            db,
            clock,
            log,
            monitor_sessions,
            timings,
            jar,
            epoch: Instant::now(),
            table: Mutex::new(ScreenTable {
                relays: HashMap::new(),
                sessions: Vec::new(),
                stopping: false,
            }),
            tasks: TaskGuards::new(),
            next_id: AtomicU64::new(1),
            watcher: Mutex::new(None),
        }
    }

    /// Follows the phones' events, so a relay hears when its phone goes offline. The task holds
    /// the hub weakly and ends with it or at shutdown.
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the watcher needs the hub that owns it, held weakly so it never keeps it"
        )
    )]
    pub(crate) fn watch_phones(hub: &Arc<Self>) {
        let mut events = hub.devices.subscribe();
        let weak: Weak<Self> = Arc::downgrade(hub);
        let task = tokio::spawn(async move {
            loop {
                let event = events.recv().await;
                let Some(hub) = weak.upgrade() else {
                    return;
                };
                match event {
                    Ok(PhoneEvent::Disconnected { serial }) => hub.phone_gone(&serial),
                    Ok(
                        PhoneEvent::Connected { .. }
                        | PhoneEvent::Ready { .. }
                        | PhoneEvent::SubsChanged { .. },
                    ) => {}
                    Err(RecvError::Lagged(_missed)) => hub.recheck_phones(),
                    Err(RecvError::Closed) => return,
                }
            }
        });
        *hub.lock_watcher() = Some(task.abort_handle());
    }

    /// A new id for a viewer or a relay.
    pub(crate) fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Whether the service is shutting down.
    pub(crate) fn is_stopping(&self) -> bool {
        self.table().stopping
    }

    /// Every phone with a relay, in the order they were first attached.
    pub(crate) fn sessions(&self) -> Vec<ScreenSession> {
        self.table().sessions.clone()
    }

    /// What `GET /api/v1/screens` shows of the phone with `serial`.
    pub(crate) fn session(&self, serial: &str) -> Option<ScreenSession> {
        self.table()
            .sessions
            .iter()
            .find(|session| session.serial == serial)
            .cloned()
    }

    /// A monotonic time for the delivery rules and the keyframe spacing, which compare times
    /// only with each other: milliseconds since the hub started, so a step of the wall clock
    /// can't hold back a keyframe.
    pub(crate) fn monotonic_now(&self) -> Timestamp {
        Timestamp::from_millis(i64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(i64::MAX))
    }

    /// Whether the phone with `serial` is online.
    pub(crate) fn is_online(&self, serial: &str) -> bool {
        self.devices
            .get(serial)
            .is_some_and(|phone| phone.status == PhoneStatus::Online)
    }

    /// Sends `command` to the relay of the phone with `serial`, starting the relay when there is
    /// none. Returns the relay's address for the viewer's later messages.
    ///
    /// # Errors
    ///
    /// [`Refusal::ShuttingDown`] once [`ScreenHub::shut_down`] has begun, and
    /// [`Refusal::Offline`] when the phone isn't online. The check is under the table's lock, so
    /// a phone that goes offline after it reaches the relay through the phone watcher.
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "each phone's relay task holds the hub, whose table it leaves when it ends"
        )
    )]
    pub(crate) fn send_or_start(
        hub: &Arc<Self>,
        serial: &str,
        command: ScreenCommand,
    ) -> Result<UnboundedSender<ScreenCommand>, Refusal> {
        let mut table = hub.table();
        if table.stopping {
            return Err(Refusal::ShuttingDown);
        }
        if !hub.is_online(serial) {
            return Err(Refusal::Offline);
        }
        let relay = if let Some(slot) = table.relays.get(serial) {
            slot.commands.clone()
        } else {
            let Some(guard) = hub.tasks.guard() else {
                return Err(Refusal::ShuttingDown);
            };
            let id = hub.next_id();
            let (sender, receiver) = mpsc::unbounded_channel();
            table.relays.insert(
                serial.to_owned(),
                RelaySlot {
                    id,
                    commands: sender.clone(),
                },
            );
            table.sessions.push(idle_session(serial));
            let relay = PhoneScreen::new(Arc::clone(hub), serial, id, sender.clone(), guard);
            drop(tokio::spawn(relay.run(receiver)));
            sender
        };
        match relay.send(command) {
            Ok(()) => Ok(relay),
            Err(_) => Err(Refusal::ShuttingDown),
        }
    }

    /// Records what `GET /api/v1/screens` shows of the relay `id`'s phone.
    pub(crate) fn publish(&self, id: u64, session: ScreenSession) {
        let mut table = self.table();
        if table
            .relays
            .get(&session.serial)
            .is_some_and(|slot| slot.id == id)
            && let Some(entry) = table
                .sessions
                .iter_mut()
                .find(|entry| entry.serial == session.serial)
        {
            *entry = session;
        }
    }

    /// Removes the relay `id` of the phone with `serial` from the table, unless a command for it
    /// arrived meanwhile in `commands`. Returns whether the relay may end. A viewer joins under
    /// the same lock, so no command can arrive after this check.
    pub(crate) fn leave(
        &self,
        serial: &str,
        id: u64,
        commands: &UnboundedReceiver<ScreenCommand>,
    ) -> bool {
        let mut table = self.table();
        if !commands.is_empty() {
            return false;
        }
        if table.relays.get(serial).is_some_and(|slot| slot.id == id) {
            table.relays.remove(serial);
            table.sessions.retain(|session| session.serial != serial);
        }
        true
    }

    /// Closes every viewer with 1001 and stops every session, finishing once every relay has
    /// ended and every server has been killed. Later viewers are refused.
    pub(crate) async fn shut_down(&self) {
        let relays: Vec<UnboundedSender<ScreenCommand>> = {
            let mut table = self.table();
            table.stopping = true;
            table
                .relays
                .values()
                .map(|slot| slot.commands.clone())
                .collect()
        };
        if let Some(watcher) = self.lock_watcher().take() {
            watcher.abort();
        }
        for relay in relays {
            match relay.send(ScreenCommand::Shutdown) {
                Ok(()) | Err(_) => {}
            }
        }
        self.tasks.close_and_wait().await;
    }

    fn phone_gone(&self, serial: &str) {
        let table = self.table();
        if let Some(slot) = table.relays.get(serial) {
            match slot.commands.send(ScreenCommand::PhoneGone) {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// After missing phone events, tells every relay whose phone isn't online that it went.
    fn recheck_phones(&self) {
        let serials: Vec<String> = self.table().relays.keys().cloned().collect();
        for serial in serials {
            if !self.is_online(&serial) {
                self.phone_gone(&serial);
            }
        }
    }

    fn table(&self) -> MutexGuard<'_, ScreenTable> {
        self.table
            .lock()
            .expect("no task panics while holding the screen table")
    }

    fn lock_watcher(&self) -> MutexGuard<'_, Option<AbortHandle>> {
        self.watcher
            .lock()
            .expect("no task panics while holding the phone watcher")
    }
}

/// A phone's entry before its relay has done anything.
fn idle_session(serial: &str) -> ScreenSession {
    ScreenSession {
        serial: serial.to_owned(),
        state: ScreenState::Idle,
        width: None,
        height: None,
        viewers: 0,
        fps: FrameRate::default(),
        kbps: 0,
        started_at: None,
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant as StdInstant};

    use tokio::runtime::Builder;

    use super::*;
    use crate::fake_adb::{self, B30};
    use crate::screen::screen_harness::{self, Harness};
    use crate::screen::viewer_frame::ViewerFrame;
    use crate::screen::viewer_queue::ViewerChannel;

    #[test]
    fn a_viewer_joining_just_as_its_phone_goes_is_closed_by_the_news() {
        Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let h = Harness::start(screen_harness::test_timings()).await;
                h.phone.set_answer_resets(false);
                let mut first = h.watch("first").await;
                fake_adb::until("the first viewer's packets", || {
                    first.pump();
                    first.binary.len() >= 2
                })
                .await;
                fake_adb::until("the relay done with the wake, its last step", || {
                    h.phone.log().contains(&"control:10".to_owned())
                })
                .await;
                let hub = Arc::clone(&h.screens.hub);
                let ViewerChannel { queue, mut outbox } = ViewerChannel::new();
                let join = ScreenCommand::Join {
                    viewer: hub.next_id(),
                    queue,
                    audit: None,
                };
                let looking = h.devices.table();
                let joiner = Arc::clone(&hub);
                let joining = thread::spawn(move || {
                    ScreenHub::send_or_start(&joiner, B30, join).map(|_relay| ())
                });
                let began = StdInstant::now();
                let mut held = 0;
                while held < 1000 {
                    assert!(
                        began.elapsed() < Duration::from_secs(10),
                        "the join holds the table while it looks at the phone"
                    );
                    held = if hub.table.try_lock().is_err() {
                        held + 1
                    } else {
                        0
                    };
                    thread::yield_now();
                }
                let watcher = Arc::clone(&hub);
                let going = thread::spawn(move || watcher.phone_gone(B30));
                drop(looking);
                assert_eq!(
                    joining.join().unwrap(),
                    Ok(()),
                    "online when the join looked"
                );
                going.join().unwrap();
                let mut closed = None;
                fake_adb::until("the joiner's close", || {
                    while let Some(frame) = outbox.try_next() {
                        if let ViewerFrame::Close(close) = frame {
                            closed = Some(close.code);
                        }
                    }
                    closed.is_some()
                })
                .await;
                assert_eq!(
                    closed,
                    Some(4001),
                    "the phone went after the join looked, so the news reached the relay after \
                     the join and closed it"
                );
                h.screens.shutdown().await;
            });
    }
}
