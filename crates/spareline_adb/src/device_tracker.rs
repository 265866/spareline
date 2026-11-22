//! The adb server's live device list.

use tokio::net::TcpStream;

use crate::adb_error::AdbError;
use crate::device_entry::DeviceEntry;
use crate::frame;
use crate::frame_reader::FrameReader;

/// A `host:track-devices-l` stream: the full device list on connect and after every change.
/// Dropping it closes the connection.
#[derive(Debug)]
pub struct DeviceTracker {
    reader: FrameReader,
}

impl DeviceTracker {
    pub(crate) fn new(stream: TcpStream) -> Self {
        Self {
            reader: FrameReader::new(stream),
        }
    }

    /// The next full device list, or `None` when the server closed the stream between updates.
    ///
    /// # Cancel safety
    ///
    /// Dropping the returned future before it completes loses nothing: the next call continues
    /// with the same update.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection fails mid-update or an update is malformed.
    pub async fn next(&mut self) -> Result<Option<Vec<DeviceEntry>>, AdbError> {
        if !self.reader.fill(4).await? {
            return Ok(None);
        }
        let length = frame::parse_hex_length(&self.reader.buffered()[..4])?;
        self.reader.fill(4 + length).await?;
        let update = self.reader.take(4 + length);
        let payload = String::from_utf8_lossy(&update[4..]);
        Ok(Some(DeviceEntry::parse_list(&payload)))
    }
}
