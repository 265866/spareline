//! The monitor's part of `GET /api/v1/status`.

use spareline_core::monitor::{Alert, PhonePolicy, ProfileSummary};

/// What `GET /api/v1/status` needs from the monitor: each phone's policy state, the open alerts,
/// the data threshold and the profile summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorStatus {
    /// The policy state of every phone that came online this run, in the order they first did.
    /// A phone the monitor hasn't seen has none, which the route sends as `"policy": null`.
    pub phones: Vec<PhonePolicy>,
    /// The open alerts, in the order they were computed.
    pub alerts: Vec<Alert>,
    /// Internet mobile bytes per SIM per trailing 24 h before data is turned off again and the
    /// phone goes red, or `None` without a radios-off profile.
    pub data_threshold_bytes: Option<u64>,
    /// The profile's name, what Set up phone sets and whether it removes apps, or `None` without
    /// a profile.
    pub profile: Option<ProfileSummary>,
}

impl MonitorStatus {
    /// The policy state of the phone with `serial`, if it came online this run.
    #[must_use]
    pub fn policy(&self, serial: &str) -> Option<&PhonePolicy> {
        self.phones.iter().find(|phone| phone.serial == serial)
    }
}
