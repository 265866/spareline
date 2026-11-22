//! The header scrcpy-server sends first on its video socket, after the dummy byte.

const DEVICE_NAME_LENGTH: usize = 64;
const CODEC_HEADER_LENGTH: usize = 12;

/// The length of the video header: a 64-byte device name, then codec, width and height.
pub const VIDEO_META_LENGTH: usize = DEVICE_NAME_LENGTH + CODEC_HEADER_LENGTH;

/// The codec id of H.264, the only one the relay accepts: `h264` in ASCII.
pub const CODEC_H264: u32 = 0x68_32_36_34;

/// The video header: the device name and the stream's codec and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoMeta {
    /// The device's name, such as `S6702X`.
    pub device_name: String,
    /// The codec id, [`CODEC_H264`] for the streams the relay asks for.
    pub codec: u32,
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
}

/// A video header of the wrong length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("video meta must be 76 bytes, got {length}")]
#[must_use]
pub struct VideoMetaError {
    length: usize,
}

impl VideoMeta {
    /// Reads the header: the NUL-padded device name, decoded as UTF-8 with replacement
    /// characters and without a leading byte order mark, as `TextDecoder` decodes it, then the codec, width and height as big-endian `u32`s.
    ///
    /// # Errors
    ///
    /// [`VideoMetaError`] unless `bytes` is exactly [`VIDEO_META_LENGTH`] long.
    pub fn parse(bytes: &[u8]) -> Result<Self, VideoMetaError> {
        let Ok(header) = <&[u8; VIDEO_META_LENGTH]>::try_from(bytes) else {
            return Err(VideoMetaError {
                length: bytes.len(),
            });
        };
        Ok(Self::from_header(header))
    }

    /// Reads a header of the right length, as [`Self::parse`] does.
    #[must_use]
    pub fn from_header(header: &[u8; VIDEO_META_LENGTH]) -> Self {
        let (name, numbers) = header.split_at(DEVICE_NAME_LENGTH);
        let end = name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name.len());
        let word = |index: usize| {
            u32::from_be_bytes([
                numbers[index * 4],
                numbers[index * 4 + 1],
                numbers[index * 4 + 2],
                numbers[index * 4 + 3],
            ])
        };
        let device_name = String::from_utf8_lossy(&name[..end]);
        Self {
            device_name: device_name
                .strip_prefix('\u{feff}')
                .unwrap_or(&device_name)
                .to_owned(),
            codec: word(0),
            width: word(1),
            height: word(2),
        }
    }

    /// Whether the stream is H.264, the only codec the relay forwards.
    #[must_use]
    pub fn is_h264(&self) -> bool {
        self.codec == CODEC_H264
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_meta_must_be_76_bytes() {
        assert_eq!(
            VideoMeta::parse(&[0; 75]).unwrap_err().to_string(),
            "video meta must be 76 bytes, got 75"
        );
        assert!(VideoMeta::parse(&[0; 77]).is_err());
    }

    #[test]
    fn the_name_decodes_as_text_decoder_does() {
        let mut bytes = [0; VIDEO_META_LENGTH];
        bytes[..8].copy_from_slice(b"\xef\xbb\xbfS6\xffX\0");
        assert_eq!(VideoMeta::parse(&bytes).unwrap().device_name, "S6\u{fffd}X");
    }

    #[test]
    fn a_name_that_fills_all_64_bytes_has_no_terminator() {
        let mut bytes = [b'a'; VIDEO_META_LENGTH];
        bytes[64..68].copy_from_slice(&CODEC_H264.to_be_bytes());
        bytes[68..72].copy_from_slice(&1080_u32.to_be_bytes());
        bytes[72..76].copy_from_slice(&2400_u32.to_be_bytes());
        let meta = VideoMeta::parse(&bytes).unwrap();
        assert_eq!(meta.device_name, "a".repeat(64));
        assert!(meta.is_h264());
        assert_eq!((meta.width, meta.height), (1080, 2400));
    }
}
