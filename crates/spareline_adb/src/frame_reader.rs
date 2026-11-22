//! A connection that reads whole frames and keeps a partly read frame across cancelled calls.

use std::fmt::{self, Debug, Formatter};
use std::mem;

use tokio::net::TcpStream;

use crate::adb_error::{AdbError, AdbErrorKind};
use crate::frame;

/// Bytes read from a long-lived stream that aren't consumed yet. Every await reads into this
/// buffer and only a complete frame leaves it, so a caller that drops a read, for example when a
/// timer wins a `future::select`, can call again and continue at the same byte.
pub(crate) struct FrameReader {
    stream: TcpStream,
    buffer: Vec<u8>,
}

impl FrameReader {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
        }
    }

    /// Reads until `length` bytes are buffered. Returns `false` when the peer closed cleanly with
    /// nothing buffered, and an error when it closed partway.
    pub(crate) async fn fill(&mut self, length: usize) -> Result<bool, AdbError> {
        use tokio::io::AsyncReadExt as _;

        if length > frame::MAX_FRAME + MAX_HEADER {
            return Err(AdbError::new(
                AdbErrorKind::Protocol,
                format!(
                    "refusing to read {length} bytes in one frame (limit {})",
                    frame::MAX_FRAME
                ),
            ));
        }
        while self.buffer.len() < length {
            self.buffer.reserve(length - self.buffer.len());
            let read = self
                .stream
                .read_buf(&mut self.buffer)
                .await
                .map_err(|error| frame::closed(&error))?;
            if read == 0 {
                if self.buffer.is_empty() {
                    return Ok(false);
                }
                return Err(frame::closed_early(self.buffer.len(), length));
            }
        }
        Ok(true)
    }

    /// The buffered bytes, which [`FrameReader::fill`] filled.
    pub(crate) fn buffered(&self) -> &[u8] {
        &self.buffer
    }

    /// Removes and returns the first `length` buffered bytes.
    pub(crate) fn take(&mut self, length: usize) -> Vec<u8> {
        let rest = self.buffer.split_off(length);
        let taken = mem::replace(&mut self.buffer, rest);
        if self.buffer.capacity() > SHRINK_ABOVE {
            self.buffer.shrink_to(SHRINK_ABOVE);
        }
        taken
    }

    /// Drops the first `length` buffered bytes, such as a frame's header.
    pub(crate) fn discard(&mut self, length: usize) {
        self.buffer.drain(..length);
    }
}

/// The longest frame header the streams read: a `shell,v2` packet's 5 bytes.
const MAX_HEADER: usize = 5;

/// A buffer that held a large frame gives the memory back once the frame is taken.
const SHRINK_ABOVE: usize = 64 * 1024;

impl Debug for FrameReader {
    /// Shows how much is buffered, never the bytes, which can hold phone serials or output.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameReader")
            .field("stream", &self.stream)
            .field("buffered_bytes", &self.buffer.len())
            .finish()
    }
}
