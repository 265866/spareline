//! A `shell,v2` packet header.

use tokio::net::TcpStream;

use crate::adb_error::{AdbError, AdbErrorKind};
use crate::frame;

/// A `shell,v2` packet header: one byte of id, then the payload's length as a little-endian
/// 32-bit number.
pub(crate) struct ShellHeader {
    /// Which stream the packet belongs to, such as [`frame::SHELL_STDOUT`].
    pub(crate) id: u8,
    /// The payload's length.
    pub(crate) length: usize,
}

impl ShellHeader {
    /// Reads a header, or `None` when the peer closed between packets.
    pub(crate) async fn read(stream: &mut TcpStream) -> Result<Option<Self>, AdbError> {
        let Some(header) = frame::read_exact_or_end(stream, 5).await? else {
            return Ok(None);
        };
        let length = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);
        let length = usize::try_from(length).map_err(|_| {
            AdbError::new(
                AdbErrorKind::Protocol,
                format!("shell packet of {length} bytes doesn't fit in memory"),
            )
        })?;
        Ok(Some(Self {
            id: header[0],
            length,
        }))
    }
}
