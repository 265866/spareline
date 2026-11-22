//! The stdout of a long-running shell command.

use tokio::net::TcpStream;

use crate::adb_error::{AdbError, AdbErrorKind};
use crate::frame;
use crate::frame_reader::FrameReader;

/// A command such as `logcat` running under the `shell,v2` protocol, so adbd ends it when this
/// closes. Dropping it closes the connection.
#[derive(Debug)]
pub struct ShellStream {
    reader: FrameReader,
}

impl ShellStream {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self {
            reader: FrameReader::new(stream),
        }
    }

    /// The next bytes the command wrote to stdout, or `None` once it exited or the connection
    /// closed between packets. Stderr is skipped.
    ///
    /// # Cancel safety
    ///
    /// Dropping the returned future before it completes loses nothing: the next call continues
    /// with the same packet.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection fails mid-packet or a packet is malformed.
    pub async fn next_stdout(&mut self) -> Result<Option<Vec<u8>>, AdbError> {
        loop {
            if !self.reader.fill(5).await? {
                return Ok(None);
            }
            let header = self.reader.buffered();
            let id = header[0];
            let declared = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);
            let length = usize::try_from(declared)
                .ok()
                .filter(|&length| length <= frame::MAX_FRAME)
                .ok_or_else(|| {
                    AdbError::new(
                        AdbErrorKind::Protocol,
                        format!(
                            "refusing to read {declared} bytes in one frame (limit {})",
                            frame::MAX_FRAME
                        ),
                    )
                })?;
            self.reader.fill(5 + length).await?;
            let packet = self.reader.take(5 + length);
            match id {
                frame::SHELL_STDOUT => return Ok(Some(packet[5..].to_vec())),
                frame::SHELL_EXIT => return Ok(None),
                _ => {}
            }
        }
    }
}
