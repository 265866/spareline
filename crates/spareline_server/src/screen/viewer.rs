//! One viewer as its phone's relay holds it.

use spareline_core::Timestamp;
use spareline_core::screen::ViewerDelivery;

use crate::screen::screen_message::ScreenMessage;
use crate::screen::viewer_frame::ViewerFrame;
use crate::screen::viewer_queue::{Queued, ViewerQueue};

/// A browser watching a phone: its queue, where it is in the stream, what it sent, and its
/// audit row.
#[derive(Debug)]
pub(crate) struct Viewer {
    /// The queue to its socket.
    pub(crate) queue: ViewerQueue,
    /// Its delivery state and counts.
    pub(crate) delivery: ViewerDelivery,
    /// The session whose `hello` it got; it receives only that session's packets.
    pub(crate) greeted: Option<u64>,
    /// Its `control_sessions` row.
    pub(crate) audit: Option<i64>,
}

impl Viewer {
    /// A viewer that joined at `now`.
    pub(crate) fn new(queue: ViewerQueue, audit: Option<i64>, now: Timestamp) -> Self {
        Self {
            queue,
            delivery: ViewerDelivery::joined(now),
            greeted: None,
            audit,
        }
    }

    /// Sends a text frame. Like spareline-old's `sendJson`, a frame the socket drops is gone.
    pub(crate) fn tell(&self, message: ScreenMessage) {
        match self.queue.send(ViewerFrame::Text(message.to_json())) {
            Queued::Queued | Queued::Dropped => {}
        }
    }

    /// Sends the session's `hello`: from now on the viewer gets that session's packets, starting
    /// from a fresh config packet and keyframe.
    pub(crate) fn greet(&mut self, generation: u64, width: u32, height: u32, now: Timestamp) {
        self.tell(ScreenMessage::Hello { width, height });
        self.greeted = Some(generation);
        self.delivery.greeted(now);
    }
}
