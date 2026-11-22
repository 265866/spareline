//! Alerts: what needs a person's attention, recomputed on every health sample and after every
//! check, Set up phone or failed data-off.

use serde::Serialize;

use crate::Timestamp;
use crate::monitor::check_report::CheckReport;
use crate::monitor::iso_time;
use crate::monitor::policy::Policy;
use crate::monitor::severity::Severity;

/// What an alert is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// The phone has not been online for more than 5 minutes.
    Offline,
    /// The host's adb server cannot open the USB device.
    NoPermissions,
    /// A loaded SIM has been out of service for more than 30 minutes.
    OutOfService,
    /// The battery is over 45 °C.
    BatteryHot,
    /// A rule fails.
    Policy,
    /// A radio or data rule still fails after the automatic fix.
    DriftUnfixed,
    /// A SIM used more mobile data than the threshold in 24 h.
    DataThreshold,
    /// The drift check failed.
    CheckFailed,
    /// Turning mobile data off failed after a SIM change or the threshold.
    SimUnsecured,
}

impl AlertKind {
    /// The kind as the API and the events spell it, such as `battery_hot`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::NoPermissions => "no_permissions",
            Self::OutOfService => "out_of_service",
            Self::BatteryHot => "battery_hot",
            Self::Policy => "policy",
            Self::DriftUnfixed => "drift_unfixed",
            Self::DataThreshold => "data_threshold",
            Self::CheckFailed => "check_failed",
            Self::SimUnsecured => "sim_unsecured",
        }
    }
}

/// One alert, as `Alert` in the API: `{key, kind, serial, severity, message, since}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Alert {
    /// Stable while the condition lasts, for the raise and clear events, such as
    /// `policy:<serial>:wifi_off`.
    pub key: String,
    /// What it is about.
    pub kind: AlertKind,
    /// The phone's serial.
    pub serial: String,
    /// How urgent it is.
    pub severity: Severity,
    /// What happened, in a sentence.
    pub message: String,
    /// When the condition started. An alert keeps the `since` it was raised with while its key
    /// lasts.
    #[serde(serialize_with = "iso_time::serialize")]
    pub since: Timestamp,
}

/// Why mobile data had to be turned off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataOffCause {
    /// A SIM changed.
    Sim,
    /// A SIM went over the data threshold.
    DataThreshold,
}

impl DataOffCause {
    /// The cause as the `data-off-failed` log line spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sim => "sim",
            Self::DataThreshold => "data_threshold",
        }
    }
}

/// A data-off that failed. It stays until a check that started after it finds mobile data off,
/// even while the phone is offline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataOffFailure {
    /// When it failed.
    pub at: Timestamp,
    /// Why data had to go off.
    pub cause: DataOffCause,
    /// The error.
    pub error: String,
}

/// One phone's policy state, as the alerts read it.
#[derive(Clone, Copy, Debug)]
pub struct PolicyAlertSource<'phone> {
    /// The phone's serial.
    pub serial: &'phone str,
    /// The phone is online now. Only the data-off failure alerts while it is not.
    pub online: bool,
    /// A data-off that failed, if one is outstanding.
    pub data_off_failure: Option<&'phone DataOffFailure>,
    /// The last drift check.
    pub check: Option<&'phone CheckReport>,
}

impl Alert {
    /// The policy alerts of the phones the monitor knows, in the order given, after the health
    /// alerts: a failed data-off, a failed check, each failing rule, and each SIM over the data
    /// threshold. A failing auto-fix safety rule is a `drift_unfixed` alert; any other failing
    /// rule, the log buffer included, a `policy` one.
    #[must_use]
    pub fn for_policy(sources: &[PolicyAlertSource<'_>], policy: Option<&Policy>) -> Vec<Self> {
        let mut alerts = Vec::new();
        for source in sources {
            let serial = source.serial;
            if let Some(failure) = source.data_off_failure {
                let cause = match failure.cause {
                    DataOffCause::Sim => "A SIM changed",
                    DataOffCause::DataThreshold => "Mobile data went over the threshold",
                };
                alerts.push(Self {
                    key: format!("sim_unsecured:{serial}"),
                    kind: AlertKind::SimUnsecured,
                    serial: serial.to_owned(),
                    severity: Severity::Critical,
                    message: format!(
                        "{cause} and turning mobile data off failed: {}",
                        failure.error
                    ),
                    since: failure.at,
                });
            }
            if !source.online {
                continue;
            }
            let Some(check) = source.check else {
                continue;
            };
            let since = check.at;
            if let Some(error) = &check.error {
                alerts.push(Self {
                    key: format!("check_failed:{serial}"),
                    kind: AlertKind::CheckFailed,
                    serial: serial.to_owned(),
                    severity: Severity::Warning,
                    message: format!("Policy check failed: {error}"),
                    since,
                });
            }
            for result in &check.results {
                if result.ok {
                    continue;
                }
                let unfixed_radio = result.auto_fix
                    && policy
                        .and_then(|policy| policy.rule(&result.id))
                        .is_some_and(|rule| rule.safety);
                let (kind, message) = if unfixed_radio {
                    (
                        AlertKind::DriftUnfixed,
                        format!(
                            "{}: still {} after the automatic fix.",
                            result.describe, result.observed
                        ),
                    )
                } else {
                    let instruction = result
                        .instruction
                        .as_ref()
                        .map_or_else(String::new, |instruction| format!(" {instruction}"));
                    (
                        AlertKind::Policy,
                        format!(
                            "{}: {} (want {}).{instruction}",
                            result.describe, result.observed, result.want
                        ),
                    )
                };
                alerts.push(Self {
                    key: format!("{}:{serial}:{}", kind.as_str(), result.id),
                    kind,
                    serial: serial.to_owned(),
                    severity: result.severity,
                    message,
                    since,
                });
            }
            for sim in &check.usage {
                if !sim.over {
                    continue;
                }
                let name = sim.slot.map_or_else(
                    || format!("sub {}", sim.sub_id),
                    |slot| (u64::from(slot) + 1).to_string(),
                );
                alerts.push(Self {
                    key: format!("data_threshold:{serial}:{}", sim.sub_id),
                    kind: AlertKind::DataThreshold,
                    serial: serial.to_owned(),
                    severity: Severity::Critical,
                    message: format!(
                        "SIM {name} used {} bytes of mobile data in 24 h; data turned off again.",
                        sim.rx.saturating_add(sim.tx)
                    ),
                    since,
                });
            }
        }
        alerts
    }
}

/// The alerts that appeared and vanished in one update.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlertChanges {
    /// New keys, in the new list's order, for `alert.raised` events.
    pub raised: Vec<Alert>,
    /// Vanished keys, in the old list's order, for `alert.cleared` events.
    pub cleared: Vec<Alert>,
}

/// The open alerts, in the order they were computed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlertSet {
    current: Vec<Alert>,
}

impl AlertSet {
    /// No open alerts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: Vec::new(),
        }
    }

    /// The open alerts.
    #[must_use]
    pub fn alerts(&self) -> &[Alert] {
        &self.current
    }

    /// Replaces the open alerts with `next`, the health alerts followed by the policy alerts. An
    /// alert whose key was already open keeps its `since`; a key listed twice keeps its first
    /// place and its last alert.
    pub fn update(&mut self, next: Vec<Alert>) -> AlertChanges {
        let mut merged: Vec<Alert> = Vec::new();
        for mut alert in next {
            if let Some(kept) = self.current.iter().find(|kept| kept.key == alert.key) {
                alert.since = kept.since;
            }
            match merged.iter_mut().find(|existing| existing.key == alert.key) {
                Some(existing) => *existing = alert,
                None => merged.push(alert),
            }
        }
        let raised: Vec<Alert> = merged
            .iter()
            .filter(|alert| self.current.iter().all(|kept| kept.key != alert.key))
            .cloned()
            .collect();
        let cleared: Vec<Alert> = self
            .current
            .iter()
            .filter(|kept| merged.iter().all(|alert| alert.key != kept.key))
            .cloned()
            .collect();
        self.current = merged;
        AlertChanges { raised, cleared }
    }
}
