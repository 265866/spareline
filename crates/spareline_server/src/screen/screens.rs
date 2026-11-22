//! The handle the HTTP routes and the monitor use to reach the screen sessions.

use std::future::Future;
use std::pin::{self, Pin};
use std::sync::Arc;
use std::time::Duration;

use axum::Error;
use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::Stream;
use futures_util::future::{self as futures, Either};
use spareline_core::screen::MAX_VIEWER_MESSAGE_BYTES;
use spareline_db::ControlCounts;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{self, Instant};

use crate::clock::Clock;
use crate::db_handle::DbHandle;
use crate::devices::Devices;
use crate::embedded;
use crate::event_log::EventLog;
use crate::monitor::ScreenSessions;
use crate::screen::screen_command::ScreenCommand;
use crate::screen::screen_hub::{Refusal, ScreenHub};
use crate::screen::screen_session::ScreenSession;
use crate::screen::screen_timings::ScreenTimings;
use crate::screen::viewer_close::ViewerClose;
use crate::screen::viewer_frame::ViewerFrame;
use crate::screen::viewer_queue::{ViewerChannel, ViewerOutbox};

/// The largest message, and frame, the screen route's WebSocket upgrade accepts: 64 KiB. The
/// route sets both with `max_message_size` and `max_frame_size`. It is well above the relay's own
/// 4 KiB limit on a viewer's messages, so a larger message reaches [`Screens::attach`], which
/// closes the socket with 1009 as Bun did, rather than failing in the WebSocket layer, which the
/// browser sees as 1006.
pub const SCREEN_SOCKET_LIMIT: usize = 64 * 1024;

/// How often a viewer's socket is pinged.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// How long a viewer may send nothing, not even a pong, before it is taken for gone.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a close frame may wait on a stalled client.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Every phone's screen session: at most one scrcpy-server per phone, shared by all of that
/// phone's viewers. Cheap to clone; every clone reaches the same sessions.
#[derive(Clone, Debug)]
pub struct Screens {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the routes, the monitor and the per-phone relay tasks all reach the same \
                      sessions"
        )
    )]
    pub(crate) hub: Arc<ScreenHub>,
}

/// A viewer that joined its phone's relay: its id, the relay's address, and its queue. Dropping
/// it leaves the relay, so a socket task that is cancelled can't keep a session alive.
#[derive(Debug)]
pub(crate) struct ViewerHandle {
    /// The viewer's id.
    pub(crate) id: u64,
    /// The relay's address, for control messages and the leave.
    pub(crate) relay: UnboundedSender<ScreenCommand>,
    /// What the relay sends the viewer.
    pub(crate) outbox: ViewerOutbox,
    left: bool,
}

impl ViewerHandle {
    /// Passes one control message to the relay, unless [`CONTROL_BACKLOG`] of the viewer's
    /// messages already wait for it, which drops and counts it. Returns `false` when the relay
    /// is gone.
    ///
    /// [`CONTROL_BACKLOG`]: crate::screen::viewer_queue::CONTROL_BACKLOG
    fn control(&self, message: Vec<u8>) -> bool {
        if !self.outbox.admit_control() {
            return true;
        }
        let control = ScreenCommand::Control {
            viewer: self.id,
            message,
        };
        self.relay.send(control).is_ok()
    }

    /// Leaves the relay, once.
    pub(crate) fn leave(&mut self) {
        if self.left {
            return;
        }
        self.left = true;
        match self.relay.send(ScreenCommand::Leave { viewer: self.id }) {
            Ok(()) | Err(_) => {}
        }
    }
}

impl Drop for ViewerHandle {
    fn drop(&mut self) {
        self.leave();
    }
}

impl Screens {
    /// The screens of the phones `devices` reaches, with spareline-old's timings, the embedded
    /// scrcpy-server, audits in `db` and times from `clock`. Each phone is listed in
    /// `monitor_sessions`, the monitor's [`ScreenSessions`], from when its session starts until
    /// its scrcpy process has been killed, so the `stay_on_off` rule accepts the setting scrcpy
    /// holds. Call it inside
    /// the tokio runtime: it starts the task that follows phones going offline.
    #[must_use]
    pub fn new(
        devices: Devices,
        db: DbHandle,
        clock: Clock,
        log: EventLog,
        monitor_sessions: ScreenSessions,
    ) -> Self {
        Self::with_timings(
            devices,
            db,
            clock,
            log,
            monitor_sessions,
            ScreenTimings::default(),
            embedded::SCRCPY_SERVER,
        )
    }

    /// As [`Screens::new`], with other timings and another jar, for tests.
    #[must_use]
    pub(crate) fn with_timings(
        devices: Devices,
        db: DbHandle,
        clock: Clock,
        log: EventLog,
        monitor_sessions: ScreenSessions,
        timings: ScreenTimings,
        jar: &'static [u8],
    ) -> Self {
        let hub = Arc::new(ScreenHub::new(
            devices,
            db,
            clock,
            log,
            monitor_sessions,
            timings,
            jar,
        ));
        ScreenHub::watch_phones(&hub);
        Self { hub }
    }

    /// Every phone with a session or viewers, for `GET /api/v1/screens`, by serial.
    #[must_use]
    pub fn list(&self) -> Vec<ScreenSession> {
        self.hub.sessions()
    }

    /// Whether the phone with `serial` has a scrcpy-server running or starting, which holds its
    /// `stay_on_while_plugged_in` until it ends, so the policy accepts the setting meanwhile.
    #[must_use]
    pub fn is_active(&self, serial: &str) -> bool {
        self.hub
            .session(serial)
            .is_some_and(|session| session.is_active())
    }

    /// Serves one browser's `GET /api/v1/phones/:serial/screen` WebSocket until it closes.
    ///
    /// `login` names the principal in the viewer's `control_sessions` row. When `revoked`
    /// resolves, the principal has signed out or lost access, and the socket closes with 4003.
    /// A phone that isn't online gets 4001 at once, and a service shutting down 1001. Text
    /// frames from the browser are ignored, binary frames are scrcpy control messages, and a
    /// message over 4 KiB closes the socket with 1009.
    pub async fn attach<R>(&self, serial: &str, socket: WebSocket, login: String, revoked: R)
    where
        R: Future<Output = ()> + Send,
    {
        match self.join(serial, login).await {
            Ok(viewer) => serve(socket, viewer, revoked).await,
            Err(close) => close_socket(socket, close).await,
        }
    }

    /// Closes every viewer with 1001 and stops every session, returning once each server on a
    /// phone has been killed. Later viewers are refused with 1001.
    pub async fn shutdown(&self) {
        self.hub.shut_down().await;
    }

    /// Adds a viewer to the relay of the phone with `serial`, after recording its
    /// `control_sessions` row.
    ///
    /// # Errors
    ///
    /// The close to send when the service is shutting down, the phone isn't online, or its
    /// audit row can't be written. A refusal after the audit row was opened closes it.
    pub(crate) async fn join(
        &self,
        serial: &str,
        login: String,
    ) -> Result<ViewerHandle, ViewerClose> {
        if self.hub.is_stopping() {
            return Err(ViewerClose::refused_shutting_down());
        }
        if !self.hub.is_online(serial) {
            return Err(ViewerClose::not_online(serial));
        }
        let now = self.hub.clock.now();
        let owned_serial = serial.to_owned();
        let opened = self
            .hub
            .db
            .call(move |db| db.control_sessions().open(&owned_serial, Some(&login), now))
            .await;
        let audit = match opened {
            Ok(audit) => audit,
            Err(error) => {
                self.hub
                    .log
                    .event("screen-audit-failed")
                    .field("serial", serial)
                    .field("error", &error.to_string())
                    .write();
                return Err(ViewerClose::audit_failed());
            }
        };
        let ViewerChannel { queue, outbox } = ViewerChannel::new();
        let id = self.hub.next_id();
        let join = ScreenCommand::Join {
            viewer: id,
            queue,
            audit,
        };
        match ScreenHub::send_or_start(&self.hub, serial, join) {
            Ok(relay) => Ok(ViewerHandle {
                id,
                relay,
                outbox,
                left: false,
            }),
            Err(refusal) => {
                if let Some(audit) = audit {
                    let now = self.hub.clock.now();
                    let closed = self
                        .hub
                        .db
                        .call(move |db| {
                            db.control_sessions()
                                .close(audit, ControlCounts::default(), now)
                        })
                        .await;
                    if let Err(error) = closed {
                        self.hub
                            .log
                            .event("screen-audit-failed")
                            .field("serial", serial)
                            .field("error", &error.to_string())
                            .write();
                    }
                }
                Err(match refusal {
                    Refusal::ShuttingDown => ViewerClose::refused_shutting_down(),
                    Refusal::Offline => ViewerClose::not_online(serial),
                })
            }
        }
    }
}

/// What ended a viewer's socket loop.
enum Ending {
    /// The relay closed the viewer, which it has already removed.
    ByRelay(ViewerClose),
    /// The viewer leaves: the socket closed, failed, went silent, broke a rule, or its principal
    /// was revoked, with the close to send, if any.
    Leaves(Option<ViewerClose>),
}

/// What one bounded send to the socket did.
enum Sending {
    /// The message is on its way.
    Done,
    /// The principal was revoked while the send waited.
    Revoked,
    /// The socket failed, or its client neither read nor spoke for the idle time.
    Gone,
}

/// Pumps frames between the relay and the socket until either side ends.
///
/// As Bun's defaults did, it pings the client every 30 s and leaves when nothing has come from
/// it for 120 s, so a browser that vanished stops keeping the phone awake. A revocation ends it
/// even while a frame waits on a stalled socket: by then the frame is in the socket's buffer,
/// ahead of the close.
async fn serve<R: Future<Output = ()> + Send>(
    mut socket: WebSocket,
    mut viewer: ViewerHandle,
    revoked: R,
) {
    let mut revoked = pin::pin!(revoked);
    let mut heard_at = Instant::now();
    let mut next_ping = heard_at + PING_INTERVAL;
    let mut waiter = Waiter::default();
    let ending = loop {
        let silent_at = heard_at + IDLE_TIMEOUT;
        let wake_at = next_ping.min(silent_at);
        let next = waiter
            .next(revoked.as_mut(), wake_at, &mut viewer.outbox, &mut socket)
            .await;
        match next {
            Wakeup::Revoked => break Ending::Leaves(Some(ViewerClose::signed_out())),
            Wakeup::Timer => {
                if Instant::now() >= silent_at {
                    break Ending::Leaves(None);
                }
                next_ping = Instant::now() + PING_INTERVAL;
                let ping = Message::Ping(Bytes::new());
                match send(&mut socket, ping, revoked.as_mut(), silent_at).await {
                    Sending::Done => {}
                    Sending::Revoked => break Ending::Leaves(Some(ViewerClose::signed_out())),
                    Sending::Gone => break Ending::Leaves(None),
                }
            }
            Wakeup::Frame(None) => break unsent_close(&mut viewer.outbox),
            Wakeup::Frame(Some(ViewerFrame::Close(close))) => break Ending::ByRelay(close),
            Wakeup::Frame(Some(frame)) => {
                let message = match &frame {
                    ViewerFrame::Text(text) => Message::Text(text.as_str().into()),
                    ViewerFrame::Binary(bytes) => Message::Binary(bytes.clone()),
                    ViewerFrame::Close(_) => continue,
                };
                let sent = send(&mut socket, message, revoked.as_mut(), silent_at).await;
                viewer.outbox.written(&frame);
                match sent {
                    Sending::Done => {}
                    Sending::Revoked => break Ending::Leaves(Some(ViewerClose::signed_out())),
                    Sending::Gone => break Ending::Leaves(None),
                }
            }
            Wakeup::Incoming(incoming) => {
                let Some(Ok(message)) = incoming else {
                    break Ending::Leaves(None);
                };
                heard_at = Instant::now();
                match message {
                    Message::Binary(message) => {
                        if message.len() > MAX_VIEWER_MESSAGE_BYTES {
                            break Ending::Leaves(Some(ViewerClose::message_too_big()));
                        }
                        if !viewer.control(message.to_vec()) {
                            break unsent_close(&mut viewer.outbox);
                        }
                    }
                    Message::Text(text) => {
                        if text.len() > MAX_VIEWER_MESSAGE_BYTES {
                            break Ending::Leaves(Some(ViewerClose::message_too_big()));
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Close(_) => break Ending::Leaves(None),
                }
            }
        }
    };
    match ending {
        Ending::ByRelay(close) => close_socket(socket, close).await,
        Ending::Leaves(close) => {
            viewer.leave();
            if let Some(close) = close {
                close_socket(socket, close).await;
            }
        }
    }
}

/// Sends `message`, unless the principal is revoked or the client stays silent until
/// `silent_at` first.
async fn send<R: Future<Output = ()>>(
    socket: &mut WebSocket,
    message: Message,
    revoked: Pin<&mut R>,
    silent_at: Instant,
) -> Sending {
    let silence = pin::pin!(time::sleep_until(silent_at));
    let sending = pin::pin!(socket.send(message));
    match futures::select(revoked, futures::select(silence, sending)).await {
        Either::Left(((), _)) => Sending::Revoked,
        Either::Right((Either::Left(((), _)) | Either::Right((Err(_), _)), _)) => Sending::Gone,
        Either::Right((Either::Right((Ok(()), _)), _)) => Sending::Done,
    }
}

/// What woke a viewer's socket loop.
enum Wakeup {
    /// The principal was revoked.
    Revoked,
    /// The ping or idle time came.
    Timer,
    /// The relay queued a frame, or dropped the queue.
    Frame(Option<ViewerFrame>),
    /// The client sent something, or the socket ended.
    Incoming(Option<Result<Message, Error>>),
}

/// Which of a viewer's two sources of traffic a wakeup polls first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Turn {
    /// The client's messages, then the relay's frames.
    #[default]
    Incoming,
    /// The relay's frames, then the client's messages.
    Frames,
}

/// Waits for what comes next on a viewer's socket, giving the relay's frames and the client's
/// messages the first look in turns. Polled in a fixed order, an outbox that stays ready, as
/// for a live viewer on a slow link, would keep the client's pongs and input unread until the
/// idle timeout dropped it, and a client that keeps sending would hold off its own frames.
#[derive(Debug, Default)]
struct Waiter {
    turn: Turn,
}

impl Waiter {
    /// The next wakeup: the revocation first, then the timer at `wake_at`, then the frames and
    /// the client's messages, in this wakeup's turn.
    async fn next<R, S>(
        &mut self,
        revoked: Pin<&mut R>,
        wake_at: Instant,
        outbox: &mut ViewerOutbox,
        socket: &mut S,
    ) -> Wakeup
    where
        R: Future<Output = ()>,
        S: Stream<Item = Result<Message, Error>> + Unpin,
    {
        let turn = self.turn;
        self.turn = match turn {
            Turn::Incoming => Turn::Frames,
            Turn::Frames => Turn::Incoming,
        };
        wakeup(revoked, wake_at, outbox, socket, turn).await
    }
}

/// Waits for the first of the revocation, the timer at `wake_at`, a frame from the relay and a
/// message from the client. The revocation is polled first, so it wins a tie, then the timer,
/// then the frame and the message in the order `turn` gives.
async fn wakeup<R, S>(
    revoked: Pin<&mut R>,
    wake_at: Instant,
    outbox: &mut ViewerOutbox,
    socket: &mut S,
    turn: Turn,
) -> Wakeup
where
    R: Future<Output = ()>,
    S: Stream<Item = Result<Message, Error>> + Unpin,
{
    use futures_util::StreamExt as _;

    let timer = pin::pin!(time::sleep_until(wake_at));
    let frame = pin::pin!(outbox.next());
    let incoming = pin::pin!(socket.next());
    let traffic = pin::pin!(async move {
        match turn {
            Turn::Incoming => match futures::select(incoming, frame).await {
                Either::Left((incoming, _)) => Wakeup::Incoming(incoming),
                Either::Right((frame, _)) => Wakeup::Frame(frame),
            },
            Turn::Frames => match futures::select(frame, incoming).await {
                Either::Left((frame, _)) => Wakeup::Frame(frame),
                Either::Right((incoming, _)) => Wakeup::Incoming(incoming),
            },
        }
    });
    match futures::select(revoked, futures::select(timer, traffic)).await {
        Either::Left(((), _)) => Wakeup::Revoked,
        Either::Right((Either::Left(((), _)), _)) => Wakeup::Timer,
        Either::Right((Either::Right((wakeup, _)), _)) => wakeup,
    }
}

/// The relay is gone: the close it queued before it went, if any, still goes out.
fn unsent_close(outbox: &mut ViewerOutbox) -> Ending {
    while let Some(frame) = outbox.try_next() {
        if let ViewerFrame::Close(close) = frame {
            return Ending::ByRelay(close);
        }
    }
    Ending::Leaves(None)
}

/// Sends the close frame, then drops the socket. A client that doesn't take it within a few
/// seconds can't hold the connection open; a socket that fails it is already gone.
async fn close_socket(mut socket: WebSocket, close: ViewerClose) {
    let frame = CloseFrame {
        code: close.code,
        reason: close.reason.into(),
    };
    match time::timeout(CLOSE_TIMEOUT, socket.send(Message::Close(Some(frame)))).await {
        Ok(Ok(()) | Err(_)) | Err(_) => {}
    }
    drop(socket);
}

#[cfg(test)]
mod tests {
    use std::future;

    use axum::body::Bytes;
    use futures_util::stream;

    use super::*;
    use crate::fake_adb;
    use crate::screen::viewer_queue::Queued;

    #[test]
    fn a_viewer_whose_outbox_stays_ready_still_has_its_pongs_read() {
        fake_adb::block_on(async {
            let ViewerChannel { queue, mut outbox } = ViewerChannel::new();
            for _ in 0..8 {
                match queue.send(ViewerFrame::Binary(Bytes::from_static(&[1]))) {
                    Queued::Queued | Queued::Dropped => {}
                }
            }
            let mut client =
                stream::repeat_with(|| Ok::<Message, Error>(Message::Pong(Bytes::new())));
            let mut revoked = pin::pin!(future::pending::<()>());
            let wake_at = Instant::now() + Duration::from_hours(1);
            let mut waiter = Waiter::default();
            let mut read = 0;
            let mut sent = 0;
            for _ in 0..4 {
                match waiter
                    .next(revoked.as_mut(), wake_at, &mut outbox, &mut client)
                    .await
                {
                    Wakeup::Incoming(Some(Ok(Message::Pong(_)))) => read += 1,
                    Wakeup::Frame(Some(frame)) => {
                        outbox.written(&frame);
                        sent += 1;
                    }
                    Wakeup::Revoked | Wakeup::Timer | Wakeup::Frame(None) | Wakeup::Incoming(_) => {
                        panic!("only frames and pongs are ready")
                    }
                }
            }
            assert_eq!(read, 2, "the client's pongs are read while frames wait");
            assert_eq!(
                sent, 2,
                "and frames still go out while the client keeps talking"
            );
        });
    }

    #[test]
    fn a_close_the_relay_queued_before_it_went_still_goes_out() {
        let ViewerChannel { queue, mut outbox } = ViewerChannel::new();
        match queue.send(ViewerFrame::Binary(Bytes::from_static(&[1]))) {
            Queued::Queued | Queued::Dropped => {}
        }
        match queue.send(ViewerFrame::Close(ViewerClose::phone_offline())) {
            Queued::Queued | Queued::Dropped => {}
        }
        drop(queue);
        assert!(matches!(
            unsent_close(&mut outbox),
            Ending::ByRelay(close) if close == ViewerClose::phone_offline()
        ));
        assert!(matches!(unsent_close(&mut outbox), Ending::Leaves(None)));
    }
}
