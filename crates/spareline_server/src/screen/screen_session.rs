//! One entry of `GET /api/v1/screens`.

use serde::{Serialize, Serializer};
use spareline_core::Timestamp;

use crate::screen::frame_rate::FrameRate;

/// Where a phone's screen session stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenState {
    /// A scrcpy-server is being started, or restarted after a failure.
    Starting,
    /// The server streams video.
    Running,
    /// No server runs: the phone has viewers waiting for a restart, or the entry is about to go.
    Idle,
}

/// A phone with a screen session or viewers, as `GET /api/v1/screens` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenSession {
    /// The phone's adb serial.
    pub serial: String,
    /// Where the session stands.
    pub state: ScreenState,
    /// The video width in pixels, once a server has sent its header.
    pub width: Option<u32>,
    /// The video height in pixels, once a server has sent its header.
    pub height: Option<u32>,
    /// The browsers watching.
    pub viewers: usize,
    /// Video frames per second over the last stats window.
    pub fps: FrameRate,
    /// Kilobits per second over the last stats window.
    pub kbps: u64,
    /// When the running server started streaming.
    #[serde(serialize_with = "timestamp_text")]
    pub started_at: Option<Timestamp>,
}

impl ScreenSession {
    /// Whether a scrcpy-server runs or is starting, which holds the phone's
    /// `stay_on_while_plugged_in` until it ends.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state != ScreenState::Idle
    }
}

#[expect(
    clippy::ref_option,
    reason = "serde's serialize_with hands the function a reference to the field itself"
)]
fn timestamp_text<S: Serializer>(
    time: &Option<Timestamp>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match time {
        Some(time) => serializer.collect_str(time),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_serializes_with_the_old_field_names() {
        let session = ScreenSession {
            serial: "SPARELINE00001".to_owned(),
            state: ScreenState::Running,
            width: Some(216),
            height: Some(480),
            viewers: 2,
            fps: FrameRate::from_tenths(295),
            kbps: 812,
            started_at: Timestamp::parse_iso("2026-10-22T00:00:00.000Z"),
        };
        assert_eq!(
            serde_json::to_string(&session).unwrap(),
            r#"{"serial":"SPARELINE00001","state":"running","width":216,"height":480,"viewers":2,"fps":29.5,"kbps":812,"startedAt":"2026-10-22T00:00:00.000Z"}"#
        );
        let idle = ScreenSession {
            state: ScreenState::Idle,
            width: None,
            height: None,
            viewers: 0,
            fps: FrameRate::default(),
            kbps: 0,
            started_at: None,
            ..session
        };
        assert_eq!(
            serde_json::to_string(&idle).unwrap(),
            r#"{"serial":"SPARELINE00001","state":"idle","width":null,"height":null,"viewers":0,"fps":0,"kbps":0,"startedAt":null}"#
        );
        assert!(!idle.is_active(), "an idle entry has no server");
    }
}
