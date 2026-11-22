//! What a drift check and Set up phone report, as the API serializes them.

use serde::Serialize;

use crate::Timestamp;
use crate::monitor::iso_time;
use crate::monitor::rule_result::RuleResult;
use crate::monitor::sim_usage::SimDataUsage;

/// Why a drift check ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckReason {
    /// The phone came online.
    Connect,
    /// The periodic check.
    Interval,
    /// Someone asked, or Set up phone just ran.
    Manual,
    /// A SIM changed.
    Sim,
}

impl CheckReason {
    /// The reason as the API and the events spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Interval => "interval",
            Self::Manual => "manual",
            Self::Sim => "sim",
        }
    }
}

/// One rule the drift checker fixed, or tried to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Revert {
    /// The rule's id.
    pub rule: String,
    /// What the read showed before.
    pub before: String,
    /// What the re-read showed after.
    pub after: String,
    /// The rule passes after.
    pub fixed: bool,
}

/// A drift check's report, as `CheckReport` in the API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckReport {
    /// When the check started.
    #[serde(serialize_with = "iso_time::serialize")]
    pub at: Timestamp,
    /// Why it ran.
    pub reason: CheckReason,
    /// How long the batched read took, in milliseconds; 0 after Set up phone or a failure.
    pub read_ms: u64,
    /// Every rule's verdict after any fix. A failed check keeps the previous check's.
    pub results: Vec<RuleResult>,
    /// Internet mobile bytes per SIM over the trailing 24 h. A failed check keeps the previous
    /// check's.
    pub usage: Vec<SimDataUsage>,
    /// What the drift checker fixed.
    pub reverted: Vec<Revert>,
    /// Why the check failed, if it did.
    pub error: Option<String>,
}

impl CheckReport {
    /// The report of a check that failed with `error`, keeping the last known results and usage
    /// from `previous`.
    #[must_use]
    pub fn failed(
        at: Timestamp,
        reason: CheckReason,
        error: &str,
        previous: Option<&Self>,
    ) -> Self {
        Self {
            at,
            reason,
            read_ms: 0,
            results: previous.map_or_else(Vec::new, |report| report.results.clone()),
            usage: previous.map_or_else(Vec::new, |report| report.usage.clone()),
            reverted: Vec::new(),
            error: Some(error.to_owned()),
        }
    }

    /// The total mobile bytes of every SIM, for the `health_5m` row.
    #[must_use]
    pub fn mobile_bytes(&self) -> MobileBytes {
        let mut total = MobileBytes { rx: 0, tx: 0 };
        for sim in &self.usage {
            total.rx = total.rx.saturating_add(sim.rx);
            total.tx = total.tx.saturating_add(sim.tx);
        }
        total
    }
}

/// Mobile bytes received and sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MobileBytes {
    /// Bytes received.
    pub rx: u64,
    /// Bytes sent.
    pub tx: u64,
}

/// A command of a Set up phone step that exited non-zero, or didn't run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FailedCommand {
    /// The command.
    pub command: String,
    /// Its output, or why it didn't run.
    pub output: String,
}

/// What Set up phone did for one rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProvisionStep {
    /// The rule's id.
    pub rule: String,
    /// The commands it ran. Empty for a report rebuilt from the `provision` event after a
    /// restart.
    pub commands: Vec<String>,
    /// The commands that failed.
    pub failed: Vec<FailedCommand>,
}

/// Set up phone's report, as `ProvisionReport` in the API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionReport {
    /// When it started.
    #[serde(serialize_with = "iso_time::serialize")]
    pub at: Timestamp,
    /// How long it took, in milliseconds.
    pub duration_ms: u64,
    /// What it changed; empty when the phone already complied.
    pub steps: Vec<ProvisionStep>,
    /// Every rule's verdict afterwards.
    pub results: Vec<RuleResult>,
    /// Every rule passes afterwards.
    pub ok: bool,
    /// Why it failed, if it did.
    pub error: Option<String>,
}

impl ProvisionReport {
    /// The report of a run that failed with `error` before it finished.
    #[must_use]
    pub fn failed(at: Timestamp, duration_ms: u64, error: &str) -> Self {
        Self {
            at,
            duration_ms,
            steps: Vec::new(),
            results: Vec::new(),
            ok: false,
            error: Some(error.to_owned()),
        }
    }
}

/// What the phone is busy with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Busy {
    /// A drift check.
    Check,
    /// Set up phone.
    Provision,
    /// Reacting to a SIM change.
    Sim,
}

/// A phone's policy state, as the `policy` field of `GET /api/v1/status`: `{serial, busy, check,
/// provision, dataAlarm}`. The old service sent its monitor's view as it was, so `serial` is
/// there too, ahead of the fields the API schema lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhonePolicy {
    /// The phone's serial.
    pub serial: String,
    /// What it is busy with.
    pub busy: Option<Busy>,
    /// The last drift check.
    pub check: Option<CheckReport>,
    /// The last Set up phone run.
    pub provision: Option<ProvisionReport>,
    /// A SIM used mobile data above the threshold in 24 h, at the last check.
    pub data_alarm: bool,
}
