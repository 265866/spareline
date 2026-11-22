//! The stdout of a long-running shell command.

use tokio::net::TcpStream;

use crate::adb_error::AdbError;
use crate::frame;
use crate::shell_header::ShellHeader;

/// A command such as `logcat` running under the `shell,v2` protocol, so adbd ends it when this
/// closes. Dropping it closes the connection.
#[derive(Debug)]
pub struct ShellStream {
    stream: TcpStream,
}

impl ShellStream {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self { stream }
    }

    /// The next bytes the command wrote to stdout, or `None` once it exited or the connection
    /// closed. Stderr is skipped.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection fails mid-packet or a packet is malformed.
    pub async fn next_stdout(&mut self) -> Result<Option<Vec<u8>>, AdbError> {
        loop {
            let Some(ShellHeader { id, length }) = ShellHeader::read(&mut self.stream).await?
            else {
                return Ok(None);
            };
            let data = frame::read_exact(&mut self.stream, length).await?;
            match id {
                frame::SHELL_STDOUT => return Ok(Some(data)),
                frame::SHELL_EXIT => return Ok(None),
                _ => {}
            }
        }
    }
}
