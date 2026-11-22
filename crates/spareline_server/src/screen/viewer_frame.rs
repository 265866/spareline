//! What the relay sends one viewer's socket.

use axum::body::Bytes;

use crate::screen::viewer_close::ViewerClose;

/// One frame for a viewer's WebSocket, in the order the relay queued it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ViewerFrame {
    /// A JSON text frame: `hello`, `status` or `stats`.
    Text(String),
    /// A scrcpy video packet, header and payload, exactly as the server sent it.
    Binary(Bytes),
    /// The last frame: close the socket with this code and reason.
    Close(ViewerClose),
}

impl ViewerFrame {
    /// The bytes the frame puts on the socket, which count toward the viewer's backlog.
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Binary(bytes) => bytes.len(),
            Self::Close(_) => 0,
        }
    }
}
