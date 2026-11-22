//! Which phones have a screen session, which the `stay_on_off` rule accepts.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};

/// The phones whose screen is being streamed, fed by the screens module. scrcpy holds
/// `stay_on_while_plugged_in` until its session ends, so while a phone is listed here the
/// `stay_on_off` rule passes and Set up phone leaves that setting alone.
///
/// Cloning is cheap and every clone sees the same set. A session counts from when it starts
/// until its scrcpy process has been killed, which covers spareline-old's `starting` and
/// `running` and the teardown after them.
#[derive(Clone, Debug, Default)]
pub struct ScreenSessions {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the screens module writes the set and the monitor's per-phone tasks read \
                      it, each from its own task"
        )
    )]
    active: Arc<Mutex<BTreeSet<String>>>,
}

impl ScreenSessions {
    /// No sessions.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that a session for `serial` started.
    pub fn started(&self, serial: &str) {
        self.lock().insert(serial.to_owned());
    }

    /// Records that the session for `serial` ended and its scrcpy process has been killed, so
    /// none is left.
    pub fn ended(&self, serial: &str) {
        self.lock().remove(serial);
    }

    /// Whether `serial` has a session now.
    #[must_use]
    pub fn is_active(&self, serial: &str) -> bool {
        self.lock().contains(serial)
    }

    fn lock(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.active
            .lock()
            .expect("no thread panicked while holding the screen session set")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_counts_from_start_until_it_ends_and_only_for_its_phone() {
        let sessions = ScreenSessions::new();
        let screens = sessions.clone();
        assert!(!sessions.is_active("A"));
        screens.started("A");
        assert!(sessions.is_active("A"));
        assert!(!sessions.is_active("B"), "another phone has no session");
        screens.ended("A");
        assert!(!sessions.is_active("A"));
        screens.ended("B");
        assert!(
            !sessions.is_active("B"),
            "ending an unknown session is harmless"
        );
    }
}
