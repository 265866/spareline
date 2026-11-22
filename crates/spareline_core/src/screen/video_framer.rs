//! Splits the bytes of scrcpy-server's video socket into the dummy byte, the header and packets,
//! however the bytes arrive.

use std::collections::VecDeque;

use crate::screen::video_meta::{VIDEO_META_LENGTH, VideoMeta};
use crate::screen::video_packet::{
    PACKET_HEADER_LENGTH, PacketHeader, PacketTooLarge, VideoPacket,
};

/// What the video socket carried next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VideoFrame {
    /// The header, after the dummy byte `0x00` that proves the server is live.
    Meta(VideoMeta),
    /// One packet, header and payload.
    Packet(VideoPacket),
}

/// A video socket that broke the protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[must_use]
pub enum VideoFramerError {
    /// The first byte wasn't the dummy byte `0x00`.
    #[error("expected dummy byte 0, got {byte}")]
    DummyByte {
        /// The byte that came instead.
        byte: u8,
    },
    /// A packet header claimed a payload over the limit.
    #[error(transparent)]
    TooLarge(#[from] PacketTooLarge),
    /// The stream ended inside a read.
    #[error("stream ended after {buffered} of {expected} expected bytes")]
    Truncated {
        /// The bytes of the unfinished read that arrived.
        buffered: usize,
        /// The bytes that read needed.
        expected: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Dummy,
    Meta,
    Header,
    Payload(PacketHeader),
}

/// A push parser for the video socket: feed it bytes with [`VideoFramer::push`], take frames
/// with [`VideoFramer::next_frame`], and call [`VideoFramer::finish`] when the socket ends.
#[derive(Debug)]
pub struct VideoFramer {
    buffer: VecDeque<u8>,
    stage: Stage,
}

impl Default for VideoFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoFramer {
    /// A framer at the start of a video socket, expecting the dummy byte.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: VecDeque::new(),
            stage: Stage::Dummy,
        }
    }

    /// Adds bytes read from the socket.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend(bytes);
    }

    /// The next complete frame, or `None` until more bytes arrive.
    ///
    /// # Errors
    ///
    /// [`VideoFramerError::DummyByte`] when the first byte isn't `0x00`, and
    /// [`VideoFramerError::TooLarge`] for a packet over the size limit. The stream is unusable
    /// after either.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>, VideoFramerError> {
        loop {
            match self.stage {
                Stage::Dummy => {
                    let Some(byte) = self.buffer.pop_front() else {
                        return Ok(None);
                    };
                    if byte != 0 {
                        return Err(VideoFramerError::DummyByte { byte });
                    }
                    self.stage = Stage::Meta;
                }
                Stage::Meta => {
                    if self.buffer.len() < VIDEO_META_LENGTH {
                        return Ok(None);
                    }
                    let mut header = [0; VIDEO_META_LENGTH];
                    for (slot, byte) in header
                        .iter_mut()
                        .zip(self.buffer.drain(..VIDEO_META_LENGTH))
                    {
                        *slot = byte;
                    }
                    self.stage = Stage::Header;
                    return Ok(Some(VideoFrame::Meta(VideoMeta::from_header(&header))));
                }
                Stage::Header => {
                    if self.buffer.len() < PACKET_HEADER_LENGTH {
                        return Ok(None);
                    }
                    let mut header = [0; PACKET_HEADER_LENGTH];
                    for (slot, &byte) in header.iter_mut().zip(self.buffer.iter()) {
                        *slot = byte;
                    }
                    self.stage = Stage::Payload(PacketHeader::parse(header)?);
                }
                Stage::Payload(header) => {
                    let length = PACKET_HEADER_LENGTH + payload_len(header);
                    if self.buffer.len() < length {
                        return Ok(None);
                    }
                    let bytes: Vec<u8> = self.buffer.drain(..length).collect();
                    self.stage = Stage::Header;
                    return Ok(Some(VideoFrame::Packet(VideoPacket {
                        bytes,
                        config: header.config,
                        keyframe: header.keyframe,
                    })));
                }
            }
        }
    }

    /// Checks the end of the socket: fine between packets, an error inside a read.
    ///
    /// # Errors
    ///
    /// [`VideoFramerError::Truncated`] when the socket ended before the dummy byte, inside the
    /// header, or inside a packet. The old relay treated a clean end as an error of its own,
    /// "the video stream ended".
    pub fn finish(&self) -> Result<(), VideoFramerError> {
        let buffered = self.buffer.len();
        let truncated = |buffered: usize, expected: usize| {
            Err(VideoFramerError::Truncated { buffered, expected })
        };
        match self.stage {
            Stage::Dummy => truncated(0, 1),
            Stage::Meta => truncated(buffered, VIDEO_META_LENGTH),
            Stage::Header if buffered == 0 => Ok(()),
            Stage::Header => truncated(buffered, PACKET_HEADER_LENGTH),
            Stage::Payload(header) => {
                truncated(buffered - PACKET_HEADER_LENGTH, payload_len(header))
            }
        }
    }
}

fn payload_len(header: PacketHeader) -> usize {
    usize::try_from(header.size).expect("a packet size under 8 MiB fits in usize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::CODEC_H264;

    const RECORDED: &[u8] = include_bytes!("../../tests/fixtures/screen/grid-home.bin");

    struct Parsed {
        meta: VideoMeta,
        packets: Vec<VideoPacket>,
    }

    fn parse(bytes: &[u8], chunk: &mut dyn FnMut() -> usize) -> Result<Parsed, VideoFramerError> {
        let mut framer = VideoFramer::new();
        let mut meta = None;
        let mut packets = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let end = (offset + chunk()).min(bytes.len());
            framer.push(&bytes[offset..end]);
            offset = end;
            while let Some(frame) = framer.next_frame()? {
                match frame {
                    VideoFrame::Meta(value) => meta = Some(value),
                    VideoFrame::Packet(packet) => packets.push(packet),
                }
            }
        }
        framer.finish()?;
        Ok(Parsed {
            meta: meta.expect("the stream has a header"),
            packets,
        })
    }

    fn check_recorded(chunk: &mut dyn FnMut() -> usize) {
        let Parsed { meta, packets } = parse(RECORDED, chunk).unwrap();
        assert_eq!(
            meta,
            VideoMeta {
                device_name: "S6702X".to_owned(),
                codec: CODEC_H264,
                width: 216,
                height: 480,
            }
        );
        assert_eq!(packets.len(), 24);
        assert_eq!(packets.iter().filter(|packet| packet.config).count(), 2);
        assert_eq!(packets.iter().filter(|packet| packet.keyframe).count(), 2);
        assert!(packets[0].config, "the stream opens with a config packet");
        for (index, packet) in packets.iter().enumerate() {
            if packet.config {
                assert!(
                    packets[index + 1].keyframe,
                    "a keyframe follows each config packet"
                );
            }
        }
        let relayed: Vec<u8> = packets
            .into_iter()
            .flat_map(|packet| packet.bytes)
            .collect();
        assert_eq!(
            relayed,
            RECORDED[77..],
            "the relay forwards packets byte for byte"
        );
    }

    #[test]
    fn the_recorded_stream_parses_as_one_chunk() {
        check_recorded(&mut || RECORDED.len());
    }

    #[test]
    fn the_recorded_stream_parses_one_byte_at_a_time() {
        check_recorded(&mut || 1);
    }

    #[test]
    fn the_recorded_stream_parses_in_random_chunks() {
        let mut state: u64 = 7;
        check_recorded(&mut || {
            state = (state * 1_103_515_245 + 12_345) % (1 << 31);
            1 + usize::try_from(state % 4_000).unwrap()
        });
    }

    #[test]
    fn a_stream_cut_mid_packet_is_an_error_not_a_short_packet() {
        let cut = &RECORDED[..RECORDED.len() - 10];
        let error = parse(cut, &mut || 1_000).err().unwrap();
        assert!(
            error.to_string().starts_with("stream ended after"),
            "{error}"
        );
    }

    #[test]
    fn a_wrong_dummy_byte_or_an_oversized_packet_breaks_the_stream() {
        let mut framer = VideoFramer::new();
        framer.push(&[1]);
        assert_eq!(
            framer.next_frame(),
            Err(VideoFramerError::DummyByte { byte: 1 })
        );
        let mut framer = VideoFramer::new();
        framer.push(&RECORDED[..77]);
        assert!(matches!(framer.next_frame(), Ok(Some(VideoFrame::Meta(_)))));
        framer.push(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 1]);
        assert_eq!(
            framer.next_frame().unwrap_err().to_string(),
            "video packet of 8388609 bytes exceeds 8388608"
        );
    }

    #[test]
    fn an_end_before_anything_or_between_packets() {
        let framer = VideoFramer::new();
        assert_eq!(
            framer.finish(),
            Err(VideoFramerError::Truncated {
                buffered: 0,
                expected: 1
            })
        );
        let mut framer = VideoFramer::new();
        framer.push(&RECORDED[..77]);
        assert!(framer.next_frame().unwrap().is_some());
        assert_eq!(framer.next_frame(), Ok(None));
        assert_eq!(framer.finish(), Ok(()));
        framer.push(&[0; 5]);
        assert_eq!(framer.next_frame(), Ok(None));
        assert_eq!(
            framer.finish(),
            Err(VideoFramerError::Truncated {
                buffered: 5,
                expected: 12
            })
        );
    }
}
