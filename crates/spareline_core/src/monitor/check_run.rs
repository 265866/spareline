//! The decisions of one drift check and one Set up phone run, between the server's reads and
//! commands.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::Timestamp;
use crate::monitor::alert::DataOffCause;
use crate::monitor::apply_plan::{ApplyPlan, CommandResult, RefusedCommand, Upload};
use crate::monitor::check_report::{
    CheckReason, CheckReport, FailedCommand, ProvisionReport, ProvisionStep, Revert,
};
use crate::monitor::monitor_event::MonitorEvent;
use crate::monitor::netstats;
use crate::monitor::policy::{self, Policy};
use crate::monitor::rule::{CheckContext, RuleSelection};
use crate::monitor::rule_result::{self, PlanStep, RuleResult};
use crate::monitor::sim_usage::{self, DATA_WINDOW_SECONDS, SimDataUsage};
use crate::monitor::sim_watch::SimChange;
use crate::monitor::snapshot::{ActiveSub, Snapshot};

/// Which of a batch's rules a group of commands comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuleGroup {
    /// The radio and data rules, which run first.
    Safety,
    /// The rest.
    Other,
}

/// The commands one check or Set up phone run applies, and the steps they came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnforceBatch {
    /// The failing rules' steps.
    pub steps: Vec<PlanStep>,
    /// The batch to apply, or `None` when there is nothing to run and no re-read is needed.
    pub apply: Option<ApplyPlan>,
}

/// What a drift check found after its re-read.
#[derive(Clone, Debug)]
pub struct CheckFinish<'check> {
    /// Why the check ran.
    pub reason: CheckReason,
    /// When it started.
    pub at: Timestamp,
    /// How long the first read took, in milliseconds.
    pub read_ms: u64,
    /// The steps it planned.
    pub steps: &'check [PlanStep],
    /// The verdicts on the first read.
    pub before: &'check [RuleResult],
    /// The verdicts after the fix: the re-read's, or the first read's when nothing ran.
    pub after: &'check [RuleResult],
    /// The read the `after` verdicts came from.
    pub after_snapshot: &'check Snapshot,
    /// The data watch's usage, from the first read.
    pub usage: Vec<SimDataUsage>,
    /// The SIMs inserted since the last read of this connection, with radios off.
    pub inserted: &'check [SimChange],
    /// The subs that were over the threshold at the previous check.
    pub alarmed: &'check [i64],
}

/// What a drift check decided once it finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckFinished {
    /// The report, for the phone's state.
    pub report: CheckReport,
    /// The events to write and publish, in order.
    pub events: Vec<MonitorEvent>,
    /// The subs over the threshold now, for the next check and `dataAlarm`.
    pub alarmed: Vec<i64>,
    /// Mobile data is off after the check. When true, a data-off failure recorded before the
    /// check started is cleared.
    pub data_off: bool,
    /// A data-off failure to record, when data had to go off and is still on.
    pub data_off_failure: Option<DataOffFailureNote>,
    /// The reverts that weren't routine, for the `drift` log line.
    pub drifted: Vec<Revert>,
    /// The routine fixes, for one log line each, named after the fix event with `-` for `_`,
    /// such as `log-buffer-set`, whether or not they took.
    pub routine: Vec<RoutineRevert>,
}

/// A routine fix and the event it logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutineRevert {
    /// The event name, such as `log_buffer_set`.
    pub event: &'static str,
    /// The fix.
    pub revert: Revert,
}

/// A data-off that didn't take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataOffFailureNote {
    /// Why data had to go off.
    pub cause: DataOffCause,
    /// The error, such as `mobile data still on: mobile_data=0, mobile_data9=unset`.
    pub error: String,
}

/// Why a check had to turn data off: a SIM inserted, or a SIM over the threshold.
#[must_use]
pub fn data_off_cause(inserted: &[SimChange], usage: &[SimDataUsage]) -> Option<DataOffCause> {
    if !inserted.is_empty() {
        Some(DataOffCause::Sim)
    } else if usage.iter().any(|sim| sim.over) {
        Some(DataOffCause::DataThreshold)
    } else {
        None
    }
}

/// The SIMs a list of slot changes inserted: each change with a new sub.
#[must_use]
pub fn inserted_sims(changes: &[SimChange]) -> Vec<SimChange> {
    changes
        .iter()
        .filter(|change| change.to.is_some())
        .copied()
        .collect()
}

impl Snapshot {
    /// The sub in each slot, for [`crate::monitor::diff_slots`].
    #[must_use]
    pub fn slot_map(&self) -> BTreeMap<u32, i64> {
        self.subs.iter().map(|sub| (sub.slot, sub.sub_id)).collect()
    }
}

impl Policy {
    /// The data watch's usage per SIM from a read, by the phone's clock, or `now` when the read
    /// had none. Empty without radios off.
    #[must_use]
    pub fn usage(&self, snapshot: &Snapshot, now: Timestamp) -> Vec<SimDataUsage> {
        let Some(threshold) = self.data_threshold_bytes() else {
            return Vec::new();
        };
        let now_seconds = snapshot
            .phone_clock
            .unwrap_or_else(|| now.millis().div_euclid(1000));
        sim_usage::internet_usage(
            &netstats::parse_netstats(&snapshot.netstats),
            now_seconds,
            DATA_WINDOW_SECONDS,
        )
        .into_iter()
        .map(|sim| {
            let slot = snapshot
                .subs
                .iter()
                .find(|sub| sub.sub_id == sim.sub_id)
                .map(|sub| sub.slot);
            sim.with_threshold(slot, threshold)
        })
        .collect()
    }

    fn commands_of(&self, steps: &[PlanStep], group: RuleGroup) -> Vec<String> {
        let safety = group == RuleGroup::Safety;
        steps
            .iter()
            .filter(|step| self.rule(step.rule).is_some_and(|rule| rule.safety) == safety)
            .flat_map(|step| step.commands.iter().cloned())
            .collect()
    }

    fn uploads_of(&self, steps: &[PlanStep]) -> Vec<Upload> {
        steps
            .iter()
            .filter_map(|step| {
                let files = &self.rule(step.rule)?.files;
                (!files.is_empty()).then(|| Upload {
                    files: files.clone(),
                    commands: step.commands.clone(),
                })
            })
            .collect()
    }

    /// A drift check's batch: the auto-fix steps, data-off for `data_off_subs` (the SIMs over
    /// the threshold, then the inserted ones) when the profile keeps radios off, and on connect
    /// with radios off the next SIMs' data keys. Without `radiosOff` the profile asks for no
    /// data-off, so none is planned, whatever the caller passes. Safety first, in case the batch is cut short: the radio and data commands, the
    /// data-off, the preset, then the rest (the log buffer).
    ///
    /// # Errors
    ///
    /// [`RefusedCommand`] when the radio guard refuses a command.
    pub fn drift_batch(
        &self,
        snapshot: &Snapshot,
        context: CheckContext,
        reason: CheckReason,
        data_off_subs: &[i64],
    ) -> Result<EnforceBatch, RefusedCommand> {
        let steps = rule_result::plan(snapshot, context, RuleSelection::AutoFix, &self.rules);
        let extra = if data_off_subs.is_empty() || self.radios_off.is_none() {
            Vec::new()
        } else {
            policy::data_off_commands(data_off_subs)
        };
        let preset = if self.radios_off.is_some() && reason == CheckReason::Connect {
            policy::preset_data_off_commands(snapshot)
        } else {
            Vec::new()
        };
        if steps.is_empty() && extra.is_empty() && preset.is_empty() {
            return Ok(EnforceBatch { steps, apply: None });
        }
        let mut commands = self.commands_of(&steps, RuleGroup::Safety);
        commands.extend(extra);
        commands.extend(preset);
        commands.extend(self.commands_of(&steps, RuleGroup::Other));
        let apply = ApplyPlan::try_new(commands, self.uploads_of(&steps))?;
        Ok(EnforceBatch {
            steps,
            apply: Some(apply),
        })
    }

    /// Set up phone's batch: every enforceable failing rule, safety first, then with radios off
    /// the next SIMs' data keys, then the rest. It always applies, even an empty batch, and
    /// re-reads.
    ///
    /// # Errors
    ///
    /// [`RefusedCommand`] when the radio guard refuses a command.
    pub fn provision_batch(
        &self,
        snapshot: &Snapshot,
        context: CheckContext,
    ) -> Result<EnforceBatch, RefusedCommand> {
        let steps = rule_result::plan(snapshot, context, RuleSelection::All, &self.rules);
        let mut commands = self.commands_of(&steps, RuleGroup::Safety);
        if self.radios_off.is_some() {
            commands.extend(policy::preset_data_off_commands(snapshot));
        }
        commands.extend(self.commands_of(&steps, RuleGroup::Other));
        let apply = ApplyPlan::try_new(commands, self.uploads_of(&steps))?;
        Ok(EnforceBatch {
            steps,
            apply: Some(apply),
        })
    }

    /// Finishes a drift check: the report, the events, the new alarm set, and whether a
    /// data-off failed.
    #[must_use]
    pub fn finish_check(&self, check: &CheckFinish<'_>) -> CheckFinished {
        let mobile = result_of(check.after, "mobile_data");
        let data_off = mobile.is_some_and(|result| result.ok);
        let data_off_failure = if data_off {
            None
        } else {
            data_off_cause(check.inserted, &check.usage).map(|cause| DataOffFailureNote {
                cause,
                error: format!(
                    "mobile data still on: {}",
                    mobile.map_or("undefined", |result| result.observed.as_str())
                ),
            })
        };
        let reverted: Vec<Revert> = check
            .steps
            .iter()
            .map(|step| {
                let after = result_of(check.after, step.rule);
                Revert {
                    rule: step.rule.to_owned(),
                    before: observed(result_of(check.before, step.rule)),
                    after: observed(after),
                    fixed: after.is_some_and(|result| result.ok),
                }
            })
            .collect();
        let mut events = Vec::new();
        let mut drifted = Vec::new();
        let mut routine = Vec::new();
        for revert in &reverted {
            match self.rule(&revert.rule).and_then(|rule| rule.fix_event) {
                Some(event) => routine.push(RoutineRevert {
                    event,
                    revert: revert.clone(),
                }),
                None => drifted.push(revert.clone()),
            }
        }
        for revert in &drifted {
            events.push(MonitorEvent::Drift {
                revert: revert.clone(),
                reason: check.reason,
            });
        }
        for fix in &routine {
            if !fix.revert.fixed {
                continue;
            }
            events.push(MonitorEvent::RoutineFix {
                kind: fix.event,
                revert: fix.revert.clone(),
                reason: check.reason,
            });
        }
        let over: Vec<&SimDataUsage> = check.usage.iter().filter(|sim| sim.over).collect();
        for sim in &over {
            if !check.alarmed.contains(&sim.sub_id) {
                events.push(MonitorEvent::DataThreshold {
                    usage: (*sim).clone(),
                    threshold_bytes: self.data_threshold_bytes(),
                    data_off,
                });
            }
        }
        for change in check.inserted {
            let Some(sub_id) = change.to else {
                continue;
            };
            let sub = check
                .after_snapshot
                .subs
                .iter()
                .find(|sub| sub.sub_id == sub_id);
            events.push(MonitorEvent::SimSecured {
                slot: change.slot,
                sub_id,
                data_off,
                mms_with_data_off: sub.is_some_and(ActiveSub::mms_with_data_off),
            });
        }
        CheckFinished {
            report: CheckReport {
                at: check.at,
                reason: check.reason,
                read_ms: check.read_ms,
                results: check.after.to_vec(),
                usage: check.usage.clone(),
                reverted,
                error: None,
            },
            events,
            alarmed: over.iter().map(|sim| sim.sub_id).collect(),
            data_off,
            data_off_failure,
            drifted,
            routine,
        }
    }
}

fn result_of<'r>(results: &'r [RuleResult], id: &str) -> Option<&'r RuleResult> {
    results.iter().find(|result| result.id == id)
}

/// A verdict's observation, or an empty string for a rule without one.
fn observed(result: Option<&RuleResult>) -> String {
    result.map_or_else(String::new, |result| result.observed.clone())
}

impl ProvisionReport {
    /// The report of a finished Set up phone run: each step with the commands that exited
    /// non-zero or didn't run, and whether every rule passes afterwards.
    #[must_use]
    pub fn finished(
        at: Timestamp,
        duration_ms: u64,
        steps: &[PlanStep],
        ran: &[CommandResult],
        results: Vec<RuleResult>,
    ) -> Self {
        Self {
            at,
            duration_ms,
            steps: steps
                .iter()
                .map(|step| ProvisionStep {
                    rule: step.rule.to_owned(),
                    commands: step.commands.clone(),
                    failed: ran
                        .iter()
                        .filter(|run| {
                            step.commands.contains(&run.command) && run.exit_code != Some(0)
                        })
                        .map(|run| FailedCommand {
                            command: run.command.clone(),
                            output: run.output.clone(),
                        })
                        .collect(),
                })
                .collect(),
            ok: results.iter().all(|result| result.ok),
            results,
            error: None,
        }
    }

    /// The last run rebuilt from its `provision` event after a restart, with empty commands and
    /// results. A field the detail lacks takes the old defaults: 0 ms, no steps, not ok, no
    /// error.
    #[must_use]
    pub fn from_event(at: Timestamp, detail: &Value) -> Self {
        Self {
            at,
            duration_ms: detail
                .get("durationMs")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            steps: detail.get("changed").and_then(Value::as_array).map_or_else(
                Vec::new,
                |changed| {
                    changed
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|rule| ProvisionStep {
                            rule: rule.to_owned(),
                            commands: Vec::new(),
                            failed: Vec::new(),
                        })
                        .collect()
                },
            ),
            results: Vec::new(),
            ok: detail.get("ok").and_then(Value::as_bool).unwrap_or(false),
            error: detail
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    }
}
