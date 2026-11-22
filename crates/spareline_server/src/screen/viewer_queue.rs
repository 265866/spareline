//! The queue between the relay and one viewer's socket, with the byte count backpressure reads
//! and the bound on the viewer's control messages in flight.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use spareline_core::screen::SOCKET_QUEUE_CAP;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::screen::viewer_frame::ViewerFrame;

/// How many of a viewer's control messages may wait for its relay. A viewer that sends faster
/// than the relay forwards loses the rest, counted as `rejected`, rather than growing a queue.
pub(crate) const CONTROL_BACKLOG: usize = 256;

/// What a send to a viewer's queue did, as Bun's `ServerWebSocket.send` reported it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Queued {
    /// The frame is queued for the socket.
    Queued,
    /// The frame would have put more than [`SOCKET_QUEUE_CAP`] bytes in the queue, or the socket
    /// is gone, so it was dropped.
    Dropped,
}

/// The counts both ends of a queue keep.
#[derive(Debug, Default)]
struct Counts {
    buffered: AtomicUsize,
    controls: AtomicUsize,
    overflowed: AtomicU64,
}

/// The relay's end of a viewer's queue. Cloning it shares the queue.
#[derive(Clone, Debug)]
pub(crate) struct ViewerQueue {
    frames: UnboundedSender<ViewerFrame>,
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the relay and the socket's task count the same frames and control messages \
                      from their two tasks"
        )
    )]
    counts: Arc<Counts>,
}

/// The socket's end of a viewer's queue: frames in order, the count to lower once each one is
/// written, and the gate its control messages pass.
#[derive(Debug)]
pub(crate) struct ViewerOutbox {
    frames: UnboundedReceiver<ViewerFrame>,
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the relay and the socket's task count the same frames and control messages \
                      from their two tasks"
        )
    )]
    counts: Arc<Counts>,
}

/// Both ends of a new, empty queue.
#[derive(Debug)]
pub(crate) struct ViewerChannel {
    /// The relay's end.
    pub(crate) queue: ViewerQueue,
    /// The socket's end.
    pub(crate) outbox: ViewerOutbox,
}

impl ViewerChannel {
    /// An empty queue.
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let counts = Arc::new(Counts::default());
        Self {
            queue: ViewerQueue {
                frames: sender,
                counts: Arc::clone(&counts),
            },
            outbox: ViewerOutbox {
                frames: receiver,
                counts,
            },
        }
    }
}

impl ViewerQueue {
    /// Queues `frame` unless it would take the queue past [`SOCKET_QUEUE_CAP`]; a close frame
    /// always goes.
    pub(crate) fn send(&self, frame: ViewerFrame) -> Queued {
        let length = frame.len();
        if length > 0 && self.buffered().saturating_add(length) > SOCKET_QUEUE_CAP {
            return Queued::Dropped;
        }
        self.counts.buffered.fetch_add(length, Ordering::AcqRel);
        if self.frames.send(frame).is_err() {
            self.counts.buffered.fetch_sub(length, Ordering::AcqRel);
            return Queued::Dropped;
        }
        Queued::Queued
    }

    /// The bytes queued and not yet written to the socket.
    pub(crate) fn buffered(&self) -> usize {
        self.counts.buffered.load(Ordering::Acquire)
    }

    /// The relay took one control message the gate let through.
    pub(crate) fn control_taken(&self) {
        match self
            .counts
            .controls
            .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                held.checked_sub(1)
            }) {
            Ok(_) | Err(_) => {}
        }
    }

    /// The control messages the gate refused since the last call, for the `rejected` count.
    pub(crate) fn take_overflowed(&self) -> u64 {
        self.counts.overflowed.swap(0, Ordering::AcqRel)
    }
}

impl ViewerOutbox {
    /// The next frame, or `None` once the relay has dropped every end of the queue.
    pub(crate) async fn next(&mut self) -> Option<ViewerFrame> {
        self.frames.recv().await
    }

    /// The next frame if one is queued, without waiting.
    pub(crate) fn try_next(&mut self) -> Option<ViewerFrame> {
        self.frames.try_recv().ok()
    }

    /// Takes a written frame's bytes off the backlog.
    pub(crate) fn written(&self, frame: &ViewerFrame) {
        self.counts
            .buffered
            .fetch_sub(frame.len(), Ordering::AcqRel);
    }

    /// Whether one more control message may go to the relay: fewer than [`CONTROL_BACKLOG`] wait
    /// for it. A refused one is counted for the relay to add to `rejected`.
    pub(crate) fn admit_control(&self) -> bool {
        let admitted = self
            .counts
            .controls
            .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < CONTROL_BACKLOG).then_some(held + 1)
            })
            .is_ok();
        if !admitted {
            self.counts.overflowed.fetch_add(1, Ordering::AcqRel);
        }
        admitted
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;

    use super::*;
    use crate::screen::viewer_close::ViewerClose;

    #[test]
    fn the_backlog_counts_queued_bytes_until_they_are_written() {
        let ViewerChannel { queue, mut outbox } = ViewerChannel::new();
        assert_eq!(
            queue.send(ViewerFrame::Binary(Bytes::from_static(&[1, 2, 3]))),
            Queued::Queued
        );
        assert_eq!(
            queue.send(ViewerFrame::Text("ab".to_owned())),
            Queued::Queued
        );
        assert_eq!(queue.buffered(), 5);
        let first = outbox.try_next().unwrap();
        outbox.written(&first);
        assert_eq!(queue.buffered(), 2);
    }

    #[test]
    fn a_frame_past_the_socket_cap_is_dropped_but_a_close_still_goes() {
        let ViewerChannel { queue, mut outbox } = ViewerChannel::new();
        let big = Bytes::from(vec![0; SOCKET_QUEUE_CAP - 1]);
        assert_eq!(queue.send(ViewerFrame::Binary(big)), Queued::Queued);
        assert_eq!(
            queue.send(ViewerFrame::Binary(Bytes::from_static(&[1, 2]))),
            Queued::Dropped
        );
        assert_eq!(
            queue.send(ViewerFrame::Binary(Bytes::from_static(&[1]))),
            Queued::Queued
        );
        assert_eq!(queue.buffered(), SOCKET_QUEUE_CAP);
        assert_eq!(
            queue.send(ViewerFrame::Close(ViewerClose::signed_out())),
            Queued::Queued
        );
        assert!(outbox.try_next().is_some());
        assert!(outbox.try_next().is_some());
        assert!(matches!(outbox.try_next(), Some(ViewerFrame::Close(_))));
    }

    #[test]
    fn a_send_to_a_closed_socket_is_dropped_and_not_counted() {
        let ViewerChannel { queue, outbox } = ViewerChannel::new();
        drop(outbox);
        assert_eq!(
            queue.send(ViewerFrame::Text("x".to_owned())),
            Queued::Dropped
        );
        assert_eq!(queue.buffered(), 0);
    }

    #[test]
    fn control_messages_past_the_backlog_are_refused_and_counted() {
        let ViewerChannel { queue, outbox } = ViewerChannel::new();
        for _ in 0..CONTROL_BACKLOG {
            assert!(outbox.admit_control(), "within the backlog");
        }
        assert!(!outbox.admit_control(), "past the backlog");
        assert!(!outbox.admit_control(), "still past it");
        queue.control_taken();
        assert!(outbox.admit_control(), "one was taken, so one more fits");
        assert_eq!(queue.take_overflowed(), 2);
        assert_eq!(queue.take_overflowed(), 0, "the count starts over");
    }
}
