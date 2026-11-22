//! Why the monitor refused a manual check or Set up phone.

use thiserror::Error;

/// Why [`Monitor::check`](super::Monitor::check) or
/// [`Monitor::provision`](super::Monitor::provision) refused to run. The routes answer 409 with
/// the message as `{"error": message}`.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[must_use]
pub enum MonitorError {
    /// The service runs without a policy profile, so there are no rules to check or apply.
    #[error("no policy profile is loaded (set POLICY_PROFILE to use phone rules)")]
    NoPolicy,
    /// The phone isn't online, or the monitor doesn't know it.
    #[error("phone {serial} is not online")]
    NotOnline {
        /// The serial asked for.
        serial: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_messages_are_the_old_services() {
        assert_eq!(
            MonitorError::NoPolicy.to_string(),
            "no policy profile is loaded (set POLICY_PROFILE to use phone rules)"
        );
        assert_eq!(
            MonitorError::NotOnline {
                serial: "SPARELINE00001".to_owned()
            }
            .to_string(),
            "phone SPARELINE00001 is not online"
        );
    }
}
