//! A connection that reads whole frames and keeps a partly read frame across cancelled calls.

use tokio::net::TcpStream;

use crate::adb_error::{AdbError, AdbErrorKind};
use crate::frame;

/// Bytes read from a long-lived stream that aren't consumed yet. Every await reads into this
/// buffer and only a complete frame leaves it, so a caller that drops a read, for example when a
/// timer wins a `future::select`, can call again and continue at the same byte.
#[derive(Debug)]
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

        if length > frame::MAX_FRAME + 8 {
            return Err(AdbError::new(
                AdbErrorKind::Protocol,
                format!(
                    "refusing to read {length} bytes in one frame (limit {})",
                    frame::MAX_FRAME
                ),
            ));
        }
        let mut chunk = vec![0; 64 * 1024];
        while self.buffer.len() < length {
            let read = self
                .stream
                .read(&mut chunk)
                .await
                .map_err(|error| frame::closed(&error))?;
            if read == 0 {
                if self.buffer.is_empty() {
                    return Ok(false);
                }
                return Err(frame::closed_early(self.buffer.len(), length));
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
        Ok(true)
    }

    /// The buffered bytes, which [`FrameReader::fill`] filled.
    pub(crate) fn buffered(&self) -> &[u8] {
        &self.buffer
    }

    /// Removes and returns the first `length` buffered bytes.
    pub(crate) fn take(&mut self, length: usize) -> Vec<u8> {
        self.buffer.drain(..length).collect()
    }
}
