//! The scrcpy-server wire format the screen relay speaks, and the rules for what each browser
//! viewer receives and may send.
//!
//! Nothing here touches a phone or a socket: the server runs the commands these build, reads the
//! bytes these parse, and drives the delivery state machines with times it supplies.

mod control_kind;
mod delivery;
mod keyframe_requests;
mod scid;
mod scrcpy;
mod video_framer;
mod video_meta;
mod video_packet;

pub use control_kind::{
    ControlKind, DISPLAY_POWER_OFF, MAX_TEXT_BYTES, MAX_VIEWER_MESSAGE_BYTES, RESET_VIDEO,
};
pub use delivery::{
    BACKLOG_LIMIT, Decision, Delivery, KEYFRAME_RETRY, SOCKET_QUEUE_CAP, ViewerCounts,
    ViewerDelivery,
};
pub use keyframe_requests::{KeyframeRequest, KeyframeRequests, RESET_INTERVAL};
pub use scid::Scid;
pub use scrcpy::{
    ANY_SCRCPY_PATTERN, ANY_SERVER_PATTERN, KILL_ANY_SERVER_COMMAND, LIST_ANY_SCRCPY_COMMAND,
    SCRCPY_VERSION, SERVER_JAR_PHONE_PATH, SERVER_JAR_SHA256, VIDEO_BIT_RATE, WAKE_COMMAND,
    WAKEFULNESS_COMMAND, parse_wakefulness, verify_server_jar,
};
pub use video_framer::{VideoFrame, VideoFramer, VideoFramerError};
pub use video_meta::{CODEC_H264, VIDEO_META_LENGTH, VideoMeta, VideoMetaError};
pub use video_packet::{
    MAX_PACKET_BYTES, PACKET_HEADER_LENGTH, PacketHeader, PacketKind, PacketTooLarge, VideoPacket,
};
