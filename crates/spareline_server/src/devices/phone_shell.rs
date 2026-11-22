//! The stdout of a long-running command on a phone, ended when the phone disconnects.

use spareline_adb::ShellStream;

use crate::devices::device_error::DeviceError;
use crate::devices::session::SessionToken;

/// A command such as `logcat` running on a phone under the shell protocol, from
/// [`Devices::open_shell`](crate::Devices::open_shell). adbd ends the command when this closes,
/// and dropping it closes it.
#[derive(Debug)]
pub struct PhoneShell {
    stream: ShellStream,
    serial: String,
    token: SessionToken,
}

impl PhoneShell {
    pub(crate) fn new(stream: ShellStream, serial: &str, token: SessionToken) -> Self {
        Self {
            stream,
            serial: serial.to_owned(),
            token,
        }
    }

    /// The serial of the phone the command runs on.
    #[must_use]
    pub fn serial(&self) -> &str {
        &self.serial
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
    /// [`DeviceError::Disconnected`] once the phone disconnected, and [`DeviceError::Adb`] when
    /// the connection fails mid-packet.
    pub async fn next_stdout(&mut self) -> Result<Option<Vec<u8>>, DeviceError> {
        let next = self
            .token
            .guard(&self.serial, self.stream.next_stdout())
            .await?;
        Ok(next?)
    }
}
