//! The stdout of a long-running shell command.

use tokio::net::TcpStream;

use crate::adb_error::AdbError;
use crate::frame;
use crate::frame_reader::FrameReader;
use crate::shell_header::ShellHeader;

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
            if !self.reader.fill(ShellHeader::SIZE).await? {
                return Ok(None);
            }
            let ShellHeader { id, length } = ShellHeader::parse(self.reader.buffered())?;
            self.reader.fill(ShellHeader::SIZE + length).await?;
            self.reader.discard(ShellHeader::SIZE);
            let payload = self.reader.take(length);
            match id {
                frame::SHELL_STDOUT => return Ok(Some(payload)),
                frame::SHELL_EXIT => return Ok(None),
                _ => {}
            }
        }
    }
}
