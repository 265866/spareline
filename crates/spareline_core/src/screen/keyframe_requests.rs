//! A session's keyframe requests to scrcpy-server, coalesced.

use std::time::Duration;

use crate::Timestamp;

/// The shortest spacing of `RESET_VIDEO` messages to one server.
pub const RESET_INTERVAL: Duration = Duration::from_millis(500);

/// What the server does about one keyframe request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyframeRequest {
    /// Write [`crate::screen::RESET_VIDEO`] now.
    SendNow,
    /// Set a timer for this time, then call [`KeyframeRequests::fire`].
    SendAt(Timestamp),
    /// A send is already scheduled; this request rides along.
    Coalesced,
}

/// The coalescing of `RESET_VIDEO` for one running session: at most one per
/// [`RESET_INTERVAL`], with requests in between folded into one scheduled send.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyframeRequests {
    last_sent: Option<Timestamp>,
    scheduled: bool,
}

impl KeyframeRequests {
    /// A session that has sent no request yet; its first request goes out at once.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_sent: None,
            scheduled: false,
        }
    }

    /// Asks for a keyframe at `now`. The server calls this only while the session runs.
    pub fn request(&mut self, now: Timestamp) -> KeyframeRequest {
        if self.scheduled {
            return KeyframeRequest::Coalesced;
        }
        match self.last_sent.map(|last| last.plus(RESET_INTERVAL)) {
            Some(due) if due > now => {
                self.scheduled = true;
                KeyframeRequest::SendAt(due)
            }
            _ => {
                self.last_sent = Some(now);
                KeyframeRequest::SendNow
            }
        }
    }

    /// The scheduled send's timer fired at `now`: write [`crate::screen::RESET_VIDEO`].
    pub const fn fire(&mut self, now: Timestamp) {
        self.scheduled = false;
        self.last_sent = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis)
    }

    #[test]
    fn requests_are_coalesced_to_one_per_500_ms() {
        let mut requests = KeyframeRequests::new();
        assert_eq!(requests.request(at(1_000)), KeyframeRequest::SendNow);
        assert_eq!(
            requests.request(at(1_100)),
            KeyframeRequest::SendAt(at(1_500))
        );
        assert_eq!(requests.request(at(1_200)), KeyframeRequest::Coalesced);
        requests.fire(at(1_500));
        assert_eq!(
            requests.request(at(1_999)),
            KeyframeRequest::SendAt(at(2_000))
        );
        requests.fire(at(2_003));
        assert_eq!(requests.request(at(2_503)), KeyframeRequest::SendNow);
        assert_eq!(requests.request(at(5_000)), KeyframeRequest::SendNow);
    }
}
