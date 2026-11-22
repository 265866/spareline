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
    /// The size of a header on the wire.
    pub(crate) const SIZE: usize = 5;

    /// Reads a header, or `None` when the peer closed between packets.
    pub(crate) async fn read(stream: &mut TcpStream) -> Result<Option<Self>, AdbError> {
        let Some(header) = frame::read_exact_or_end(stream, Self::SIZE).await? else {
            return Ok(None);
        };
        Self::parse(&header).map(Some)
    }

    /// Reads the first [`ShellHeader::SIZE`] bytes of `bytes` as a header, refusing a payload
    /// longer than one frame may be.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, AdbError> {
        let declared = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
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
        Ok(Self {
            id: bytes[0],
            length,
        })
    }
}
