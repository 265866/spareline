//! A rule's verdict as the API reports it.

use serde::Serialize;

use crate::monitor::rule::{CheckContext, Rule, RuleSelection};
use crate::monitor::severity::Severity;
use crate::monitor::snapshot::Snapshot;

/// One rule's verdict, as `RuleResult` in the API: `{id, describe, want, severity, autoFix,
/// instruction, fixable, ok, observed}`, in that order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleResult {
    /// The rule's id.
    pub id: String,
    /// What the rule wants, in words.
    pub describe: String,
    /// What it wants, as the read shows it.
    pub want: String,
    /// How urgent a failure is.
    pub severity: Severity,
    /// Applied automatically by the drift checker on sight.
    pub auto_fix: bool,
    /// When the rule fails: what to do on the phone if adb cannot set it, or what happens next
    /// if the automatic fix did not take.
    pub instruction: Option<String>,
    /// Set up phone can set it over adb.
    pub fixable: bool,
    /// The rule passes.
    pub ok: bool,
    /// What the read showed.
    pub observed: String,
}

/// Every rule's verdict on a read, in rule order.
#[must_use]
pub fn evaluate(snapshot: &Snapshot, context: CheckContext, rules: &[Rule]) -> Vec<RuleResult> {
    rules
        .iter()
        .map(|rule| {
            let outcome = rule.check(snapshot, context);
            RuleResult {
                id: rule.id.to_owned(),
                describe: rule.describe.clone(),
                want: rule.want.clone(),
                severity: rule.severity,
                auto_fix: rule.auto_fix,
                instruction: outcome.instruction.or(rule.instruction).map(str::to_owned),
                fixable: rule.fixable(),
                ok: outcome.ok,
                observed: outcome.observed,
            }
        })
        .collect()
}

/// The commands for one failing rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanStep {
    /// The rule's id.
    pub rule: &'static str,
    /// Its commands, in order.
    pub commands: Vec<String>,
}

/// The commands that would make the failing rules pass: every enforceable one for Set up phone,
/// the drift checker's for a check. A rule with nothing to run now gets no step.
#[must_use]
pub fn plan(
    snapshot: &Snapshot,
    context: CheckContext,
    selection: RuleSelection,
    rules: &[Rule],
) -> Vec<PlanStep> {
    rules
        .iter()
        .filter(|rule| selection == RuleSelection::All || rule.auto_fix)
        .filter(|rule| rule.fixable() && !rule.check(snapshot, context).ok)
        .filter_map(|rule| {
            let commands = rule.enforce(snapshot, context)?;
            (!commands.is_empty()).then_some(PlanStep {
                rule: rule.id,
                commands,
            })
        })
        .collect()
}
