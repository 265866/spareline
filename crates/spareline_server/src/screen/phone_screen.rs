//! One phone's relay: its viewers, its scrcpy session, and the timers around them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use spareline_core::screen::{
    ControlKind, DISPLAY_POWER_OFF, Decision, KeyframeRequest, KeyframeRequests, RESET_VIDEO, Scid,
    VideoMeta, VideoPacket, WAKE_COMMAND, WAKEFULNESS_COMMAND,
};
use spareline_core::{Timestamp, screen};
use spareline_db::ControlCounts;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::{self, Instant};

use crate::screen::frame_rate::FrameRate;
use crate::screen::scrcpy_session::{ScrcpySession, SessionSetup};
use crate::screen::screen_command::{ScreenCommand, SessionEvent, WakeReason};
use crate::screen::screen_hub::ScreenHub;
use crate::screen::screen_message::{ScreenMessage, StatusState};
use crate::screen::screen_session::{ScreenSession, ScreenState};
use crate::screen::task_guard::TaskGuard;
use crate::screen::viewer::Viewer;
use crate::screen::viewer_close::ViewerClose;
use crate::screen::viewer_frame::ViewerFrame;
use crate::screen::viewer_queue::{Queued, ViewerQueue};
use crate::shell_text;

/// How long a wake command may take.
const WAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a wakefulness read may take.
const WAKEFULNESS_TIMEOUT: Duration = Duration::from_secs(5);

/// Every this many consecutive checks that find the phone asleep, the relay warns that waking
/// doesn't stick.
const NOT_STICKING_EVERY: u32 = 6;

/// The relay of one phone's screen: at most one scrcpy session, shared by every viewer, kept
/// until the idle time after the last viewer leaves, and restarted within its budget when it
/// fails while watched. It runs as one task and handles one command or timer at a time.
#[derive(Debug)]
pub(crate) struct PhoneScreen {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "each phone's relay task holds the hub, whose table it leaves when it ends"
        )
    )]
    hub: Arc<ScreenHub>,
    serial: String,
    id: u64,
    commands: UnboundedSender<ScreenCommand>,
    guard: TaskGuard,
    viewers: BTreeMap<u64, Viewer>,
    session: Option<Session>,
    stopping: Option<u64>,
    /// The generations whose scrcpy process may still run: every session started and not yet
    /// torn down, the current one included.
    alive: BTreeSet<u64>,
    generation: u64,
    idle_at: Option<Instant>,
    idle: bool,
    retry_at: Option<Instant>,
    failures: usize,
    window: StatsWindow,
    fps: FrameRate,
    kbps: u64,
    next_stats: Instant,
    shutting_down: bool,
    listed: bool,
}

/// The session the relay runs, and what it knows of it.
#[derive(Debug)]
struct Session {
    task: ScrcpySession,
    meta: Option<VideoMeta>,
    started: Option<Started>,
    keyframes: KeyframeRequests,
    reset_at: Option<Instant>,
    watch: Option<WakeWatch>,
}

/// When a session started streaming.
#[derive(Clone, Copy, Debug)]
struct Started {
    at: Instant,
    time: Timestamp,
}

/// The wake checks of a running session: one at a time, every wake-check interval, while it
/// has viewers. A phone that goes to sleep mid-session, by a power-button press as measured on
/// the B30, stops sending frames to everyone watching. Nobody watching during the idle window
/// means no check, so an unwatched phone may sleep.
#[derive(Clone, Copy, Debug)]
struct WakeWatch {
    next_check: Instant,
    checking: bool,
    darken_again: bool,
    asleep: u32,
}

/// The packets and bytes since the last stats tick.
#[derive(Clone, Copy, Debug)]
struct StatsWindow {
    packets: u64,
    bytes: u64,
    since: Instant,
}

impl PhoneScreen {
    /// The relay of the phone with `serial`, which the table knows as `id`.
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "each phone's relay task holds the hub, whose table it leaves when it ends"
        )
    )]
    pub(crate) fn new(
        hub: Arc<ScreenHub>,
        serial: &str,
        id: u64,
        commands: UnboundedSender<ScreenCommand>,
        guard: TaskGuard,
    ) -> Self {
        let now = Instant::now();
        let next_stats = next_tick(hub.epoch, hub.timings.stats_interval, now);
        Self {
            hub,
            serial: serial.to_owned(),
            id,
            commands,
            guard,
            viewers: BTreeMap::new(),
            session: None,
            stopping: None,
            alive: BTreeSet::new(),
            generation: 0,
            idle_at: None,
            idle: false,
            retry_at: None,
            failures: 0,
            window: StatsWindow {
                packets: 0,
                bytes: 0,
                since: now,
            },
            fps: FrameRate::default(),
            kbps: 0,
            next_stats,
            shutting_down: false,
            listed: false,
        }
    }

    /// Runs the relay until it has no viewers, no session and no teardown to wait for, the
    /// teardowns of a shutdown included.
    pub(crate) async fn run(mut self, mut commands: UnboundedReceiver<ScreenCommand>) {
        loop {
            match time::timeout_at(self.next_deadline(), commands.recv()).await {
                Ok(Some(command)) => self.handle(command),
                Ok(None) => break,
                Err(_timer) => self.fire_timers(),
            }
            self.publish();
            if self.is_done() && self.hub.leave(&self.serial, self.id, &commands) {
                break;
            }
        }
        drop(self.guard);
    }

    fn handle(&mut self, command: ScreenCommand) {
        match command {
            ScreenCommand::Join {
                viewer,
                queue,
                audit,
            } => self.join(viewer, queue, audit),
            ScreenCommand::Leave { viewer } => self.leave(viewer),
            ScreenCommand::Control { viewer, message } => self.control(viewer, message),
            ScreenCommand::PhoneGone => self.phone_gone(),
            ScreenCommand::Shutdown => self.shut_down(),
            ScreenCommand::Session { generation, event } => self.session_event(generation, event),
            ScreenCommand::Woke {
                generation,
                error,
                reason,
            } => self.woke(generation, error, reason),
            ScreenCommand::Wakefulness { generation, output } => {
                self.wakefulness(generation, &output);
            }
        }
    }

    /// Adds a viewer, starting or restarting the session as needed.
    fn join(&mut self, id: u64, queue: ViewerQueue, audit: Option<i64>) {
        let now = self.hub.monotonic_now();
        let mut viewer = Viewer::new(queue, audit, now);
        self.idle_at = None;
        self.idle = false;
        let running = self.running_generation().zip(
            self.session
                .as_ref()
                .and_then(|session| session.meta.as_ref()),
        );
        if let Some((generation, meta)) = running {
            viewer.greet(generation, meta.width, meta.height, now);
            self.viewers.insert(id, viewer);
            self.request_keyframe();
            self.wake(generation, WakeReason::Join);
        } else {
            viewer.tell(ScreenMessage::Status {
                state: StatusState::Starting,
            });
            self.viewers.insert(id, viewer);
        }
        self.reconcile();
    }

    fn leave(&mut self, id: u64) {
        self.detach(id);
        self.reconcile();
    }

    /// Forwards one control message from viewer `id` when the session runs and the message is
    /// allowed; everything else is dropped and counted. A message the session's full write queue
    /// refuses counts as rejected too.
    fn control(&mut self, id: u64, message: Vec<u8>) {
        let running = self.running_generation().is_some();
        let Some(viewer) = self.viewers.get_mut(&id) else {
            return;
        };
        viewer.queue.control_taken();
        if !running {
            viewer.delivery.reject();
            return;
        }
        let Some(kind) = viewer.delivery.control(&message) else {
            return;
        };
        let written = self
            .session
            .as_ref()
            .is_some_and(|session| session.task.write(message));
        if !written {
            let counts = &mut viewer.delivery.counts;
            match kind {
                ControlKind::Text => counts.texts -= 1,
                ControlKind::Input => counts.inputs -= 1,
            }
            counts.rejected += 1;
        }
    }

    fn phone_gone(&mut self) {
        self.retry_at = None;
        self.close_all(&ViewerClose::phone_offline());
        self.idle = true;
        self.reconcile();
    }

    fn shut_down(&mut self) {
        self.shutting_down = true;
        self.idle_at = None;
        self.retry_at = None;
        self.stopping = None;
        if let Some(mut session) = self.session.take() {
            session.task.stop();
        }
        self.close_all(&ViewerClose::shutting_down());
    }

    fn session_event(&mut self, generation: u64, event: SessionEvent) {
        if let SessionEvent::TornDown = event {
            self.alive.remove(&generation);
            if self.stopping == Some(generation) {
                self.stopping = None;
                self.reconcile();
            }
            return;
        }
        if self.session.as_ref().map(|s| s.task.generation()) != Some(generation) {
            return;
        }
        match event {
            SessionEvent::Started(meta) => self.started(generation, meta),
            SessionEvent::Packet(packet) => self.broadcast(generation, packet),
            SessionEvent::Ended(error) => self.ended(error),
            SessionEvent::TornDown => {}
        }
    }

    /// The session streams: every viewer gets its hello, then the phone is woken, which asks
    /// for the fresh config packet and keyframe they start from.
    fn started(&mut self, generation: u64, meta: VideoMeta) {
        let now = self.hub.monotonic_now();
        for viewer in self.viewers.values_mut() {
            viewer.greet(generation, meta.width, meta.height, now);
        }
        let time = self.hub.clock.now();
        if let Some(session) = &mut self.session {
            session.meta = Some(meta);
            session.started = Some(Started {
                at: Instant::now(),
                time,
            });
        }
        self.wake(generation, WakeReason::Start);
    }

    fn broadcast(&mut self, generation: u64, packet: VideoPacket) {
        if self.running_generation() != Some(generation) {
            return;
        }
        let kind = packet.kind();
        if !packet.config {
            self.window.packets += 1;
        }
        self.window.bytes += u64::try_from(packet.bytes.len()).unwrap_or(u64::MAX);
        let bytes = Bytes::from(packet.bytes);
        let now = self.hub.monotonic_now();
        let mut wants_keyframe = false;
        for viewer in self.viewers.values_mut() {
            if viewer.greeted != Some(generation) {
                continue;
            }
            match viewer.delivery.deliver(kind, viewer.queue.buffered(), now) {
                Decision::Send => {
                    if viewer.queue.send(ViewerFrame::Binary(bytes.clone())) == Queued::Dropped {
                        viewer.delivery.send_dropped(kind);
                    }
                }
                Decision::DropAndRequestKeyframe => wants_keyframe = true,
                Decision::Drop => {}
            }
        }
        if wants_keyframe {
            self.request_keyframe();
        }
    }

    /// The session ended. Stopped, unwatched or shutting down: nothing more. Failed while
    /// watched: restart after the next delay of the budget, which a session that ran long
    /// enough starts over, or close the viewers with 4002 when it has run out.
    fn ended(&mut self, error: Option<String>) {
        let Some(session) = self.session.take() else {
            return;
        };
        let Some(error) = error else {
            self.reconcile();
            return;
        };
        if self.viewers.is_empty() || self.shutting_down {
            self.reconcile();
            return;
        }
        let ran_for = session
            .started
            .map_or(Duration::ZERO, |started| started.at.elapsed());
        if ran_for >= self.hub.timings.healthy_after {
            self.failures = 0;
        }
        let delay = self.hub.timings.restart_delays.get(self.failures).copied();
        self.failures += 1;
        match delay {
            None => {
                self.failures = 0;
                self.close_all(&ViewerClose::session_failed(&error));
                self.reconcile();
            }
            Some(delay) => {
                for viewer in self.viewers.values() {
                    viewer.tell(ScreenMessage::Status {
                        state: StatusState::Restarting,
                    });
                }
                self.retry_at = Some(Instant::now() + delay);
            }
        }
    }

    /// A wake finished: ask for a fresh keyframe and darken the phone's own display again,
    /// since waking turns it on and it stays dark while watched.
    fn woke(&mut self, generation: u64, error: Option<String>, reason: WakeReason) {
        if let Some(error) = error {
            self.hub
                .log
                .event("screen-wake-failed")
                .field("serial", &self.serial)
                .field("error", &error)
                .write();
        }
        if self.running_generation() != Some(generation) {
            return;
        }
        self.request_keyframe();
        self.write(DISPLAY_POWER_OFF.to_vec());
        let wake_check = self.hub.timings.wake_check;
        let Some(session) = &mut self.session else {
            return;
        };
        match reason {
            WakeReason::Start => {
                session.watch = Some(WakeWatch {
                    next_check: Instant::now() + wake_check,
                    checking: false,
                    darken_again: false,
                    asleep: 0,
                });
            }
            WakeReason::Join => {}
            WakeReason::Check => {
                if let Some(watch) = &mut session.watch {
                    watch.darken_again = true;
                    watch.checking = false;
                }
            }
        }
    }

    fn wakefulness(&mut self, generation: u64, output: &Result<String, String>) {
        let running = self.running_generation() == Some(generation);
        let serial = self.serial.clone();
        let Some(watch) = self
            .session
            .as_mut()
            .filter(|session| session.task.generation() == generation)
            .and_then(|session| session.watch.as_mut())
        else {
            return;
        };
        let state = match output {
            Ok(output) => screen::parse_wakefulness(output).map(str::to_owned),
            Err(_phone_gone_or_busy) => None,
        };
        let Some(state) = state.filter(|_| running) else {
            watch.checking = false;
            return;
        };
        if state == "Awake" {
            watch.asleep = 0;
            watch.checking = false;
            return;
        }
        watch.asleep += 1;
        let asleep = watch.asleep;
        if asleep == 1 {
            self.hub
                .log
                .event("screen-woke")
                .field("serial", &serial)
                .field("was", &state)
                .write();
        } else if asleep.is_multiple_of(NOT_STICKING_EVERY) {
            self.hub
                .log
                .event("screen-wake-not-sticking")
                .field("serial", &serial)
                .field("state", &state)
                .field("checks", &asleep)
                .write();
        }
        self.wake(generation, WakeReason::Check);
    }

    fn fire_timers(&mut self) {
        let now = Instant::now();
        if self.idle_at.is_some_and(|at| at <= now) {
            self.idle_at = None;
            self.idle = true;
            self.reconcile();
        }
        if self.retry_at.is_some_and(|at| at <= now) {
            self.retry_at = None;
            self.reconcile();
        }
        if self.next_stats <= now {
            self.next_stats = next_tick(self.hub.epoch, self.hub.timings.stats_interval, now);
            self.publish_stats(now);
        }
        let reset_due = self
            .session
            .as_ref()
            .and_then(|session| session.reset_at)
            .is_some_and(|at| at <= now);
        if reset_due {
            let time = self.hub.monotonic_now();
            if let Some(session) = &mut self.session {
                session.reset_at = None;
                session.keyframes.fire(time);
            }
            self.write(RESET_VIDEO.to_vec());
        }
        self.check_awake(now);
    }

    /// The wake check's tick: one read of the phone's wakefulness at a time, while watched.
    fn check_awake(&mut self, now: Instant) {
        let running = self.running_generation();
        let watched = !self.viewers.is_empty();
        let wake_check = self.hub.timings.wake_check;
        let Some(session) = &mut self.session else {
            return;
        };
        let Some(watch) = &mut session.watch else {
            return;
        };
        if watch.next_check > now {
            return;
        }
        watch.next_check = next_tick(watch.next_check, wake_check, now);
        let Some(generation) = running else {
            session.watch = None;
            return;
        };
        if watch.checking || !watched {
            return;
        }
        watch.checking = true;
        if watch.darken_again {
            watch.darken_again = false;
            match session.task.write(DISPLAY_POWER_OFF.to_vec()) {
                true | false => {}
            }
        }
        let devices = self.hub.devices.clone();
        let serial = self.serial.clone();
        let commands = self.commands.clone();
        let guard = self.guard.clone();
        drop(tokio::spawn(async move {
            let output = devices
                .exec(&serial, WAKEFULNESS_COMMAND, WAKEFULNESS_TIMEOUT)
                .await
                .map(|output| shell_text::shell_text(&output))
                .map_err(|error| error.to_string());
            let read = ScreenCommand::Wakefulness { generation, output };
            match commands.send(read) {
                Ok(()) | Err(_) => {}
            }
            drop(guard);
        }));
    }

    /// Wakes the phone and dismisses its keyguard, so the screen shows something that changes;
    /// [`PhoneScreen::woke`] follows when the command finishes.
    fn wake(&self, generation: u64, reason: WakeReason) {
        let devices = self.hub.devices.clone();
        let serial = self.serial.clone();
        let commands = self.commands.clone();
        let guard = self.guard.clone();
        drop(tokio::spawn(async move {
            let error = devices
                .exec(&serial, WAKE_COMMAND, WAKE_TIMEOUT)
                .await
                .err()
                .map(|error| error.to_string());
            let woke = ScreenCommand::Woke {
                generation,
                error,
                reason,
            };
            match commands.send(woke) {
                Ok(()) | Err(_) => {}
            }
            drop(guard);
        }));
    }

    /// Asks the server for a new config packet and keyframe, at most once per
    /// [`RESET_INTERVAL`](spareline_core::screen::RESET_INTERVAL); requests in between fold into
    /// one.
    fn request_keyframe(&mut self) {
        if self.running_generation().is_none() {
            return;
        }
        let time = self.hub.monotonic_now();
        let Some(session) = &mut self.session else {
            return;
        };
        match session.keyframes.request(time) {
            KeyframeRequest::SendNow => {
                session.task.write(RESET_VIDEO.to_vec());
            }
            KeyframeRequest::SendAt(due) => {
                session.reset_at = Some(Instant::now() + due.since(time));
            }
            KeyframeRequest::Coalesced => {}
        }
    }

    /// Writes one of the relay's own control messages. One that a stalled socket's full queue
    /// refuses is dropped; the next wake or keyframe request writes again.
    fn write(&self, message: Vec<u8>) {
        if self.running_generation().is_some()
            && let Some(session) = &self.session
        {
            match session.task.write(message) {
                true | false => {}
            }
        }
    }

    /// Brings the session in line with the viewers: one while anyone watches, kept until the
    /// idle time after the last one leaves, then none. A stopped session's teardown finishes
    /// before the next session starts.
    fn reconcile(&mut self) {
        if self.stopping.is_some() {
            return;
        }
        let keep = !(self.shutting_down || (self.viewers.is_empty() && self.idle));
        if !keep {
            if let Some(mut session) = self.session.take() {
                session.task.stop();
                self.stopping = Some(session.task.generation());
            }
            return;
        }
        if self.viewers.is_empty() || self.session.is_some() || self.retry_at.is_some() {
            return;
        }
        self.start();
    }

    fn start(&mut self) {
        if !self.hub.is_online(&self.serial) {
            self.phone_gone();
            return;
        }
        self.generation += 1;
        self.alive.insert(self.generation);
        let scid = Scid::from_random(random_bits(self.hub.clock.now()));
        let setup = SessionSetup {
            devices: self.hub.devices.clone(),
            log: self.hub.log.clone(),
            serial: self.serial.clone(),
            scid,
            generation: self.generation,
            jar: self.hub.jar,
            connect_timeout: self.hub.timings.connect_timeout,
            events: self.commands.clone(),
        };
        self.session = Some(Session {
            task: ScrcpySession::spawn(setup, self.guard.clone()),
            meta: None,
            started: None,
            keyframes: KeyframeRequests::new(),
            reset_at: None,
            watch: None,
        });
    }

    /// Removes viewer `id` and closes its audit row; the last one to leave starts the idle
    /// time.
    fn detach(&mut self, id: u64) -> Option<ViewerQueue> {
        let viewer = self.viewers.remove(&id)?;
        if self.viewers.is_empty() {
            self.idle_at = Some(Instant::now() + self.hub.timings.idle);
        }
        self.close_audit(&viewer);
        Some(viewer.queue)
    }

    /// Closes every viewer's socket with `close`, and their audit rows.
    fn close_all(&mut self, close: &ViewerClose) {
        let ids: Vec<u64> = self.viewers.keys().copied().collect();
        for id in ids {
            if let Some(queue) = self.detach(id) {
                match queue.send(ViewerFrame::Close(close.clone())) {
                    Queued::Queued | Queued::Dropped => {}
                }
            }
        }
    }

    /// Records the viewer's counts in its audit row, in a task of its own, so a busy database
    /// never holds up the video. The shutdown waits for it.
    fn close_audit(&self, viewer: &Viewer) {
        let Some(id) = viewer.audit else {
            return;
        };
        let counts = viewer.delivery.counts;
        let rejected = counts
            .rejected
            .saturating_add(viewer.queue.take_overflowed());
        let counts = ControlCounts {
            inputs: i64::try_from(counts.inputs).unwrap_or(i64::MAX),
            texts: i64::try_from(counts.texts).unwrap_or(i64::MAX),
            rejected: i64::try_from(rejected).unwrap_or(i64::MAX),
        };
        let now = self.hub.clock.now();
        let hub = Arc::clone(&self.hub);
        let serial = self.serial.clone();
        let guard = self.guard.clone();
        drop(tokio::spawn(async move {
            let closed = hub
                .db
                .call(move |db| db.control_sessions().close(id, counts, now))
                .await;
            if let Err(error) = closed {
                hub.log
                    .event("screen-audit-failed")
                    .field("serial", &serial)
                    .field("error", &error.to_string())
                    .write();
            }
            drop(guard);
        }));
    }

    /// Updates the stats every viewer gets and the list shows: frames per second to one
    /// decimal and kilobits per second, over the window since the last tick.
    fn publish_stats(&mut self, now: Instant) {
        let seconds = now
            .duration_since(self.window.since)
            .as_secs_f64()
            .max(0.001);
        #[expect(
            clippy::cast_precision_loss,
            reason = "packet and byte counts of one stats window stay far below 2^52"
        )]
        let (packets, bytes) = (self.window.packets as f64, self.window.bytes as f64);
        self.fps = FrameRate::from_tenths(rounded(packets / seconds * 10.0));
        self.kbps = rounded(bytes * 8.0 / 1000.0 / seconds);
        self.window = StatsWindow {
            packets: 0,
            bytes: 0,
            since: now,
        };
        for viewer in self.viewers.values() {
            viewer.tell(ScreenMessage::Stats {
                fps: self.fps,
                kbps: self.kbps,
            });
        }
    }

    /// The generation of the session when it runs: started streaming and not ended.
    fn running_generation(&self) -> Option<u64> {
        self.session
            .as_ref()
            .filter(|session| session.meta.is_some())
            .map(|session| session.task.generation())
    }

    fn next_deadline(&self) -> Instant {
        let session = self.session.as_ref();
        [
            Some(self.next_stats),
            self.idle_at,
            self.retry_at,
            session.and_then(|session| session.reset_at),
            session
                .and_then(|session| session.watch)
                .map(|watch| watch.next_check),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(self.next_stats)
    }

    fn is_done(&self) -> bool {
        self.viewers.is_empty()
            && self.session.is_none()
            && self.stopping.is_none()
            && self.alive.is_empty()
    }

    /// Publishes the phone's entry, and lists the phone in the monitor's screen sessions from
    /// the start of a session until no scrcpy process of its is left, whether it stopped,
    /// failed, was given up on or was shut down: scrcpy holds `stay_on_while_plugged_in` until
    /// it is killed.
    fn publish(&mut self) {
        let active = self.session.is_some() || !self.alive.is_empty();
        if active != self.listed {
            self.listed = active;
            if active {
                self.hub.monitor_sessions.started(&self.serial);
            } else {
                self.hub.monitor_sessions.ended(&self.serial);
            }
        }
        let session = self.session.as_ref();
        let meta = session.and_then(|session| session.meta.as_ref());
        let state = match (session, meta) {
            (Some(_), Some(_)) => ScreenState::Running,
            (Some(_), None) => ScreenState::Starting,
            (None, _) => ScreenState::Idle,
        };
        self.hub.publish(
            self.id,
            ScreenSession {
                serial: self.serial.clone(),
                state,
                width: meta.map(|meta| meta.width),
                height: meta.map(|meta| meta.height),
                viewers: self.viewers.len(),
                fps: self.fps,
                kbps: self.kbps,
                started_at: session
                    .and_then(|session| session.started)
                    .map(|started| started.time),
            },
        );
    }
}

/// The first tick of `interval` after `now` on the grid that starts at `origin`.
fn next_tick(origin: Instant, interval: Duration, now: Instant) -> Instant {
    let interval = interval.max(Duration::from_millis(1));
    let elapsed = now.saturating_duration_since(origin);
    let ticks = elapsed.as_nanos() / interval.as_nanos() + 1;
    let ticks = u32::try_from(ticks).unwrap_or(u32::MAX);
    origin + interval.saturating_mul(ticks)
}

/// `value` rounded to the nearest whole number, as `Math.round` rounds a positive one.
fn rounded(value: f64) -> u64 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a rate is finite and not negative, and `as` saturates at the ends anyway"
    )]
    let whole = value.round() as u64;
    whole
}

/// 32 random bits for a session id. Should the OS random source fail, the low bits of `now`
/// still keep two sessions on one phone apart.
fn random_bits(now: Timestamp) -> u32 {
    getrandom::u32().unwrap_or_else(|_unavailable| {
        now.millis().to_le_bytes()[..4]
            .iter()
            .fold(0, |bits, &byte| bits << 8 | u32::from(byte))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_ticks_fall_on_whole_intervals_after_the_origin() {
        let origin = Instant::now();
        let interval = Duration::from_secs(2);
        assert_eq!(next_tick(origin, interval, origin), origin + interval);
        assert_eq!(
            next_tick(origin, interval, origin + Duration::from_millis(2_500)),
            origin + Duration::from_secs(4)
        );
        assert_eq!(
            next_tick(origin, interval, origin + Duration::from_secs(4)),
            origin + Duration::from_secs(6)
        );
    }

    #[test]
    fn rates_round_as_math_round_did() {
        assert_eq!(rounded(29.45), 29);
        assert_eq!(rounded(29.5), 30);
        assert_eq!(rounded(0.0), 0);
    }
}
