//! The `device_events` rows the monitor writes, with the exact `detail_json` the old monitor
//! wrote.

use serde::Serialize;

use crate::monitor::alert::{Alert, AlertKind};
use crate::monitor::check_report::{CheckReason, FailedCommand, ProvisionReport, Revert};
use crate::monitor::severity::Severity;
use crate::monitor::sim_usage::{SimDataUsage, UidBytes};

/// How many of a SIM's apps the `data_threshold` event names.
const THRESHOLD_EVENT_UIDS: usize = 5;

/// One event for the phone's `device_events`, which the server writes with the phone's id and
/// publishes on the event bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorEvent {
    /// `drift`: a radio or data setting had changed and the drift checker reverted it, whether
    /// or not the revert took.
    Drift {
        /// What changed and whether the fix took.
        revert: Revert,
        /// Why the check ran.
        reason: CheckReason,
    },
    /// A routine automatic fix that took, under the rule's own event name, such as
    /// `log_buffer_set`.
    RoutineFix {
        /// The event name.
        kind: &'static str,
        /// What changed.
        revert: Revert,
        /// Why the check ran.
        reason: CheckReason,
    },
    /// `data_threshold`: a SIM went over the threshold, once per spell.
    DataThreshold {
        /// The SIM's usage.
        usage: SimDataUsage,
        /// The threshold.
        threshold_bytes: Option<u64>,
        /// Mobile data was off after the check.
        data_off: bool,
    },
    /// `sim.secured`: a newly inserted SIM was checked with data off.
    SimSecured {
        /// The slot.
        slot: u32,
        /// The subscription id.
        sub_id: i64,
        /// Mobile data was off after the check.
        data_off: bool,
        /// "MMS messages" stays on with data off for this SIM.
        mms_with_data_off: bool,
    },
    /// `provision`: Set up phone ran.
    Provision {
        /// Its report.
        report: ProvisionReport,
    },
    /// `alert.raised`.
    AlertRaised {
        /// The new alert.
        alert: Alert,
    },
    /// `alert.cleared`.
    AlertCleared {
        /// The alert that vanished.
        alert: Alert,
    },
}

#[derive(Serialize)]
struct RevertDetail<'event> {
    rule: &'event str,
    before: &'event str,
    after: &'event str,
    fixed: bool,
    reason: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ThresholdDetail<'event> {
    sub_id: i64,
    slot: Option<u32>,
    rx: u64,
    tx: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    threshold_bytes: Option<u64>,
    uids: &'event [UidBytes],
    data_off: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SecuredDetail {
    slot: u32,
    sub_id: i64,
    data_off: bool,
    mms_with_data_off: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProvisionDetail<'event> {
    ok: bool,
    changed: Vec<&'event str>,
    failed_commands: Vec<&'event FailedCommand>,
    failed_rules: Vec<&'event str>,
    duration_ms: u64,
    error: Option<&'event str>,
}

#[derive(Serialize)]
struct RaisedDetail<'event> {
    key: &'event str,
    kind: AlertKind,
    severity: Severity,
    message: &'event str,
}

#[derive(Serialize)]
struct ClearedDetail<'event> {
    key: &'event str,
    kind: AlertKind,
}

impl MonitorEvent {
    /// The event's `kind` column, such as `drift` or `alert.raised`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Drift { .. } => "drift",
            Self::RoutineFix { kind, .. } => kind,
            Self::DataThreshold { .. } => "data_threshold",
            Self::SimSecured { .. } => "sim.secured",
            Self::Provision { .. } => "provision",
            Self::AlertRaised { .. } => "alert.raised",
            Self::AlertCleared { .. } => "alert.cleared",
        }
    }

    /// The serial of the phone an alert event is about, which the server looks up for the
    /// row's phone id; `None` for the other events, which the server writes for the phone it is
    /// working on.
    #[must_use]
    pub fn alert_serial(&self) -> Option<&str> {
        match self {
            Self::AlertRaised { alert } | Self::AlertCleared { alert } => Some(&alert.serial),
            Self::Drift { .. }
            | Self::RoutineFix { .. }
            | Self::DataThreshold { .. }
            | Self::SimSecured { .. }
            | Self::Provision { .. } => None,
        }
    }

    /// The event's `detail_json`, with the keys in the order the old monitor wrote them.
    ///
    /// # Panics
    ///
    /// Never: the details hold only strings, numbers and booleans, which always serialize.
    #[must_use]
    pub fn detail_json(&self) -> String {
        let detail = match self {
            Self::Drift { revert, reason } | Self::RoutineFix { revert, reason, .. } => {
                serde_json::to_string(&RevertDetail {
                    rule: &revert.rule,
                    before: &revert.before,
                    after: &revert.after,
                    fixed: revert.fixed,
                    reason: reason.as_str(),
                })
            }
            Self::DataThreshold {
                usage,
                threshold_bytes,
                data_off,
            } => serde_json::to_string(&ThresholdDetail {
                sub_id: usage.sub_id,
                slot: usage.slot,
                rx: usage.rx,
                tx: usage.tx,
                threshold_bytes: *threshold_bytes,
                uids: &usage.uids[..usage.uids.len().min(THRESHOLD_EVENT_UIDS)],
                data_off: *data_off,
            }),
            Self::SimSecured {
                slot,
                sub_id,
                data_off,
                mms_with_data_off,
            } => serde_json::to_string(&SecuredDetail {
                slot: *slot,
                sub_id: *sub_id,
                data_off: *data_off,
                mms_with_data_off: *mms_with_data_off,
            }),
            Self::Provision { report } => serde_json::to_string(&ProvisionDetail {
                ok: report.ok,
                changed: report.steps.iter().map(|step| step.rule.as_str()).collect(),
                failed_commands: report.steps.iter().flat_map(|step| &step.failed).collect(),
                failed_rules: report
                    .results
                    .iter()
                    .filter(|result| !result.ok)
                    .map(|result| result.id.as_str())
                    .collect(),
                duration_ms: report.duration_ms,
                error: report.error.as_deref(),
            }),
            Self::AlertRaised { alert } => serde_json::to_string(&RaisedDetail {
                key: &alert.key,
                kind: alert.kind,
                severity: alert.severity,
                message: &alert.message,
            }),
            Self::AlertCleared { alert } => serde_json::to_string(&ClearedDetail {
                key: &alert.key,
                kind: alert.kind,
            }),
        };
        detail.expect("event details hold only strings, numbers and booleans")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timestamp;
    use crate::monitor::check_report::ProvisionStep;

    fn revert() -> Revert {
        Revert {
            rule: "log_buffer".to_owned(),
            before: "256 KiB".to_owned(),
            after: "8 MiB".to_owned(),
            fixed: true,
        }
    }

    #[test]
    fn details_keep_the_old_key_order() {
        assert_eq!(
            MonitorEvent::RoutineFix {
                kind: "log_buffer_set",
                revert: revert(),
                reason: CheckReason::Connect
            }
            .detail_json(),
            r#"{"rule":"log_buffer","before":"256 KiB","after":"8 MiB","fixed":true,"reason":"connect"}"#
        );
        let usage = SimDataUsage {
            sub_id: 1,
            slot: Some(0),
            rx: 1_500_000,
            tx: 0,
            over: true,
            uids: (0..7).map(|uid| UidBytes { uid, bytes: 1 }).collect(),
        };
        assert_eq!(
            MonitorEvent::DataThreshold {
                usage,
                threshold_bytes: Some(1_000_000),
                data_off: true
            }
            .detail_json(),
            r#"{"subId":1,"slot":0,"rx":1500000,"tx":0,"thresholdBytes":1000000,"uids":[{"uid":0,"bytes":1},{"uid":1,"bytes":1},{"uid":2,"bytes":1},{"uid":3,"bytes":1},{"uid":4,"bytes":1}],"dataOff":true}"#
        );
        assert_eq!(
            MonitorEvent::SimSecured {
                slot: 1,
                sub_id: 9,
                data_off: true,
                mms_with_data_off: false
            }
            .detail_json(),
            r#"{"slot":1,"subId":9,"dataOff":true,"mmsWithDataOff":false}"#
        );
    }

    #[test]
    fn the_provision_event_names_what_changed_and_what_failed() {
        let report = ProvisionReport {
            at: Timestamp::from_millis(0),
            duration_ms: 1234,
            steps: vec![ProvisionStep {
                rule: "lock_screen_notice".to_owned(),
                commands: vec!["x".to_owned()],
                failed: vec![FailedCommand {
                    command: "x".to_owned(),
                    output: "not run".to_owned(),
                }],
            }],
            results: Vec::new(),
            ok: false,
            error: None,
        };
        let event = MonitorEvent::Provision { report };
        assert_eq!(event.kind(), "provision");
        assert_eq!(
            event.detail_json(),
            r#"{"ok":false,"changed":["lock_screen_notice"],"failedCommands":[{"command":"x","output":"not run"}],"failedRules":[],"durationMs":1234,"error":null}"#
        );
    }

    #[test]
    fn alert_events_carry_the_alerts_key_and_kind() {
        let alert = Alert {
            key: "policy:B30:ota_off".to_owned(),
            kind: AlertKind::Policy,
            serial: "B30".to_owned(),
            severity: Severity::Warning,
            message:
                "Automatic system updates off: 0 (want global ota_disable_automatic_update = 1)."
                    .to_owned(),
            since: Timestamp::from_millis(0),
        };
        let raised = MonitorEvent::AlertRaised {
            alert: alert.clone(),
        };
        assert_eq!(raised.alert_serial(), Some("B30"));
        assert_eq!(
            raised.detail_json(),
            r#"{"key":"policy:B30:ota_off","kind":"policy","severity":"warning","message":"Automatic system updates off: 0 (want global ota_disable_automatic_update = 1)."}"#
        );
        assert_eq!(
            MonitorEvent::AlertCleared { alert }.detail_json(),
            r#"{"key":"policy:B30:ota_off","kind":"policy"}"#
        );
    }
}
