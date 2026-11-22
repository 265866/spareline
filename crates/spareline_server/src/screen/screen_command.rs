//! What a phone's relay acts on: its viewers, its phone, and its own tasks.

use spareline_core::screen::{VideoMeta, VideoPacket};

use crate::screen::viewer_queue::ViewerQueue;

/// One message to a phone's relay task.
#[derive(Debug)]
pub(crate) enum ScreenCommand {
    /// A viewer attached.
    Join {
        /// The viewer's id.
        viewer: u64,
        /// Its queue.
        queue: ViewerQueue,
        /// Its `control_sessions` row, when the phone has a database row.
        audit: Option<i64>,
    },
    /// A viewer's socket closed or its principal was revoked.
    Leave {
        /// The viewer's id.
        viewer: u64,
    },
    /// A viewer sent a binary message within the size limit.
    Control {
        /// The viewer's id.
        viewer: u64,
        /// The message, not yet checked.
        message: Vec<u8>,
    },
    /// The phone went offline.
    PhoneGone,
    /// The service is shutting down.
    Shutdown,
    /// The session task reported.
    Session {
        /// Which session.
        generation: u64,
        /// What happened.
        event: SessionEvent,
    },
    /// A wake command finished.
    Woke {
        /// The session it was for.
        generation: u64,
        /// Why it failed, if it did.
        error: Option<String>,
        /// What the wake was for.
        reason: WakeReason,
    },
    /// A wakefulness read finished.
    Wakefulness {
        /// The session it was for.
        generation: u64,
        /// The command's output, or why it failed.
        output: Result<String, String>,
    },
}

/// What a session's task reports.
#[derive(Debug)]
pub(crate) enum SessionEvent {
    /// The video header arrived: the session runs.
    Started(VideoMeta),
    /// A video packet.
    Packet(VideoPacket),
    /// The session ended, once: `None` when it was stopped, otherwise why it failed.
    Ended(Option<String>),
    /// The session's server has been killed.
    TornDown,
}

/// Why the relay woke a phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WakeReason {
    /// The session started; the wake checks begin after this wake.
    Start,
    /// A viewer joined a running session.
    Join,
    /// A wake check found the phone asleep; the next check darkens the panel again.
    Check,
}
