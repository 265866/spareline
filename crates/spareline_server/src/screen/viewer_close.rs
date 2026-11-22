//! Why the relay closes a viewer's socket.

/// The longest close reason WebSocket allows, in bytes: a control frame carries at most 125
/// bytes, two of which hold the code.
const MAX_REASON_BYTES: usize = 123;

/// The longest reason spareline-old sent, in UTF-16 code units, as `slice(0, 120)` cut it.
const MAX_REASON_UNITS: usize = 120;

/// A close code and reason for a viewer's socket, with the codes the web UI acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ViewerClose {
    /// The WebSocket close code.
    pub(crate) code: u16,
    /// The reason, within WebSocket's 123-byte limit.
    pub(crate) reason: String,
}

impl ViewerClose {
    /// 4001 at attach: the phone isn't online.
    pub(crate) fn not_online(serial: &str) -> Self {
        Self::new(4001, &format!("phone {serial} is not online"))
    }

    /// 4001 while watching: the phone went offline.
    pub(crate) fn phone_offline() -> Self {
        Self::new(4001, "phone offline")
    }

    /// 4002: the session failed after its restart budget ran out.
    pub(crate) fn session_failed(error: &str) -> Self {
        Self::new(4002, &format!("screen session failed: {error}"))
    }

    /// 4003: the viewer's principal signed out or lost access.
    pub(crate) fn signed_out() -> Self {
        Self::new(4003, "signed out")
    }

    /// 1001 at attach: the service is shutting down.
    pub(crate) fn refused_shutting_down() -> Self {
        Self::new(1001, "the server is shutting down")
    }

    /// 1001 while watching: the service is shutting down.
    pub(crate) fn shutting_down() -> Self {
        Self::new(1001, "server shutting down")
    }

    /// 1009: the viewer sent a message over the relay's 4 KiB limit, as Bun's
    /// `maxPayloadLength` closed it.
    pub(crate) fn message_too_big() -> Self {
        Self::new(1009, "message too big")
    }

    /// 1011: the relay couldn't record the viewer's `control_sessions` row, so it refuses the
    /// viewer rather than let unaudited input through.
    pub(crate) fn audit_failed() -> Self {
        Self::new(1011, "screen audit unavailable")
    }

    fn new(code: u16, reason: &str) -> Self {
        Self {
            code,
            reason: cut_reason(reason),
        }
    }
}

/// `reason` cut to 120 UTF-16 code units, as spareline-old cut it, and then to whole characters
/// within 123 UTF-8 bytes, which a close frame can carry.
fn cut_reason(reason: &str) -> String {
    let mut units = 0;
    let mut bytes = 0;
    let mut cut = String::new();
    for character in reason.chars() {
        units += character.len_utf16();
        bytes += character.len_utf8();
        if units > MAX_REASON_UNITS || bytes > MAX_REASON_BYTES {
            break;
        }
        cut.push(character);
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_reason_is_cut_to_120_characters() {
        let close = ViewerClose::session_failed(&"x".repeat(300));
        assert_eq!(close.code, 4002);
        assert_eq!(close.reason.len(), 120);
        assert!(close.reason.starts_with("screen session failed: xxx"));
        let short = ViewerClose::session_failed("the video stream ended");
        assert_eq!(
            short.reason,
            "screen session failed: the video stream ended"
        );
    }

    #[test]
    fn a_reason_with_wide_characters_fits_the_close_frame() {
        let close = ViewerClose::session_failed(&"\u{e9}".repeat(200));
        assert!(close.reason.len() <= 123, "{} bytes", close.reason.len());
        assert!(close.reason.ends_with('\u{e9}'));
        let emoji = ViewerClose::session_failed(&"\u{1f600}".repeat(100));
        assert!(emoji.reason.len() <= 123, "{} bytes", emoji.reason.len());
        let units: usize = emoji.reason.chars().map(char::len_utf16).sum();
        assert!(units <= 120, "{units} UTF-16 units");
    }

    #[test]
    fn the_attach_refusals_carry_the_old_reasons() {
        assert_eq!(
            ViewerClose::not_online("SPARELINE00001"),
            ViewerClose {
                code: 4001,
                reason: "phone SPARELINE00001 is not online".to_owned()
            }
        );
        assert_eq!(ViewerClose::refused_shutting_down().code, 1001);
        assert_eq!(ViewerClose::signed_out().reason, "signed out");
    }
}
