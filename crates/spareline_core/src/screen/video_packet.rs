//! One video packet on scrcpy-server's video socket.

/// The length of a packet header: a `u64` of flags and PTS, then a `u32` payload size.
pub const PACKET_HEADER_LENGTH: usize = 12;

/// The largest payload the relay accepts. A native 1080x2400 keyframe is a few hundred KB, so a
/// larger size means a corrupt stream, not a frame to buffer.
pub const MAX_PACKET_BYTES: u32 = 8 * 1024 * 1024;

/// A packet's 12-byte header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacketHeader {
    /// Bit 63 of the first word: a config packet (SPS and PPS), which a decoder needs before any
    /// frame.
    pub config: bool,
    /// Bit 62: a keyframe, which decodes without the frames before it.
    pub keyframe: bool,
    /// The payload size in bytes.
    pub size: u32,
}

/// A header whose payload size is over [`MAX_PACKET_BYTES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("video packet of {size} bytes exceeds {max}")]
#[must_use]
pub struct PacketTooLarge {
    size: u32,
    max: u32,
}

impl PacketHeader {
    /// Reads a header: a big-endian `u64` whose bit 63 marks a config packet, bit 62 a keyframe,
    /// and the rest the PTS; then the payload size as a big-endian `u32`.
    ///
    /// # Errors
    ///
    /// [`PacketTooLarge`] when the payload size is over [`MAX_PACKET_BYTES`].
    pub fn parse(header: [u8; PACKET_HEADER_LENGTH]) -> Result<Self, PacketTooLarge> {
        let flags = header[0];
        let size = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
        if size > MAX_PACKET_BYTES {
            return Err(PacketTooLarge {
                size,
                max: MAX_PACKET_BYTES,
            });
        }
        Ok(Self {
            config: flags & 0x80 != 0,
            keyframe: flags & 0x40 != 0,
            size,
        })
    }
}

/// What a packet holds, as far as delivery cares. A packet with both bits set counts as a config
/// packet, as the old relay checked the config bit first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketKind {
    /// SPS and PPS, which a decoder needs before any frame.
    Config,
    /// A frame that decodes without the frames before it.
    Keyframe,
    /// A frame that needs the frames before it.
    Frame,
}

impl PacketHeader {
    /// What the packet holds.
    #[must_use]
    pub const fn kind(self) -> PacketKind {
        if self.config {
            PacketKind::Config
        } else if self.keyframe {
            PacketKind::Keyframe
        } else {
            PacketKind::Frame
        }
    }
}

/// One packet exactly as the server sent it, header and payload, which is also exactly what a
/// browser viewer receives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoPacket {
    /// The 12-byte header followed by the payload.
    pub bytes: Vec<u8>,
    /// A config packet.
    pub config: bool,
    /// A keyframe.
    pub keyframe: bool,
}

impl VideoPacket {
    /// What the packet holds.
    #[must_use]
    pub const fn kind(&self) -> PacketKind {
        PacketHeader {
            config: self.config,
            keyframe: self.keyframe,
            size: 0,
        }
        .kind()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(first: u8, size: u32) -> [u8; PACKET_HEADER_LENGTH] {
        let mut bytes = [0; PACKET_HEADER_LENGTH];
        bytes[0] = first;
        bytes[8..].copy_from_slice(&size.to_be_bytes());
        bytes
    }

    #[test]
    fn the_top_two_bits_mark_config_and_keyframe_packets() {
        assert_eq!(
            PacketHeader::parse(header(0x80, 30)).unwrap(),
            PacketHeader {
                config: true,
                keyframe: false,
                size: 30
            }
        );
        assert_eq!(
            PacketHeader::parse(header(0x40, 0)).unwrap(),
            PacketHeader {
                config: false,
                keyframe: true,
                size: 0
            }
        );
        assert_eq!(
            PacketHeader::parse(header(0x3f, 1)).unwrap(),
            PacketHeader {
                config: false,
                keyframe: false,
                size: 1
            }
        );
    }

    #[test]
    fn a_packet_header_claiming_more_than_8_mib_is_a_corrupt_stream() {
        assert!(PacketHeader::parse(header(0, MAX_PACKET_BYTES)).is_ok());
        assert_eq!(
            PacketHeader::parse(header(0, MAX_PACKET_BYTES + 1))
                .unwrap_err()
                .to_string(),
            "video packet of 8388609 bytes exceeds 8388608"
        );
    }
}
