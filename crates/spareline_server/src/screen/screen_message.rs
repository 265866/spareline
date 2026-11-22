//! The JSON text frames the relay sends viewers.

use serde::Serialize;

use crate::screen::frame_rate::FrameRate;

/// A text frame for a viewer. Binary frames are video packets; a viewer gets a session's
/// packets only after that session's `hello`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub(crate) enum ScreenMessage {
    /// The video size, at each session start, or at attach when the session runs.
    Hello {
        /// The width in pixels.
        width: u32,
        /// The height in pixels.
        height: u32,
    },
    /// The session isn't streaming yet.
    Status {
        /// Why.
        state: StatusState,
    },
    /// The last stats window, every 2 s.
    Stats {
        /// Frames per second, to one decimal.
        fps: FrameRate,
        /// Kilobits per second.
        kbps: u64,
    },
}

/// What a `status` frame says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StatusState {
    /// The viewer attached while no session ran; one is starting.
    Starting,
    /// The session died and a restart is scheduled.
    Restarting,
}

impl ScreenMessage {
    /// The frame's JSON text.
    pub(crate) fn to_json(self) -> String {
        serde_json::to_string(&self).expect("a screen message serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_print_as_the_web_ui_reads_them() {
        assert_eq!(
            ScreenMessage::Hello {
                width: 216,
                height: 480
            }
            .to_json(),
            r#"{"t":"hello","width":216,"height":480}"#
        );
        assert_eq!(
            ScreenMessage::Status {
                state: StatusState::Starting
            }
            .to_json(),
            r#"{"t":"status","state":"starting"}"#
        );
        assert_eq!(
            ScreenMessage::Status {
                state: StatusState::Restarting
            }
            .to_json(),
            r#"{"t":"status","state":"restarting"}"#
        );
        assert_eq!(
            ScreenMessage::Stats {
                fps: FrameRate::from_tenths(124),
                kbps: 3_812
            }
            .to_json(),
            r#"{"t":"stats","fps":12.4,"kbps":3812}"#
        );
    }
}
