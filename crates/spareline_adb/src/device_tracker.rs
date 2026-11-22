//! The adb server's live device list.

use tokio::net::TcpStream;

use crate::adb_error::AdbError;
use crate::device_entry::DeviceEntry;
use crate::frame;

/// A `host:track-devices-l` stream: the full device list on connect and after every change.
/// Dropping it closes the connection.
#[derive(Debug)]
pub struct DeviceTracker {
    stream: TcpStream,
}

impl DeviceTracker {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self { stream }
    }

    /// The next full device list, or `None` when the server closed the stream between updates.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection fails mid-update or an update is malformed.
    pub async fn next(&mut self) -> Result<Option<Vec<DeviceEntry>>, AdbError> {
        let Some(header) = frame::read_exact_or_end(&mut self.stream, 4).await? else {
            return Ok(None);
        };
        let length = frame::parse_hex_length(&header)?;
        let payload = frame::read_exact(&mut self.stream, length).await?;
        Ok(Some(DeviceEntry::parse_list(&String::from_utf8_lossy(
            &payload,
        ))))
    }
}
