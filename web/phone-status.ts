import type {
	AlertView,
	CheckReportView,
	ProvisionReportView,
	RuleResultView,
} from "../src/api/monitor-schema";

// The rules a phone fails, most severe first.
export function problems(results: RuleResultView[]): RuleResultView[] {
	const rank = { critical: 0, warning: 1, info: 2 } as const;
	return results
		.filter((result) => !result.ok)
		.sort((a, b) => rank[a.severity] - rank[b.severity]);
}

// One failing rule in plain words: what should hold, what the phone has
// now, and what to tap when adb cannot fix it.
export function problemText(result: RuleResultView): string {
	const now = `${result.describe}: now ${result.observed}.`;
	return result.instruction ? `${now} ${result.instruction}` : now;
}

// The rule an alert is about ("policy:<serial>:<rule id>").
const ruleOf = (alert: AlertView) => alert.key.split(":").at(-1);

// The alerts for the banner above the phones: all but what the phone's card
// already lists, which is a failing rule and a failed check (the server
// raises check_failed only for online phones, which always have a card). A
// radio still on after the automatic fix (drift_unfixed) stays, as it is
// critical.
export function bannerAlerts(
	alerts: AlertView[],
	resultsFor: (serial: string) => RuleResultView[] | undefined,
): AlertView[] {
	return alerts.filter((alert) => {
		if (alert.kind === "check_failed") return false;
		if (alert.kind !== "policy") return true;
		return !resultsFor(alert.serial)?.some(
			(result) => result.id === ruleOf(alert) && !result.ok,
		);
	});
}

// An alert in plain words. A rule's alert carries the rule's technical
// target ("want ..."), so it is worded from the phone's rule result instead.
export function alertText(
	alert: AlertView,
	results: RuleResultView[] | undefined,
): string {
	if (alert.kind === "policy") {
		const result = results?.find((candidate) => candidate.id === ruleOf(alert));
		if (result && !result.ok) return problemText(result);
	}
	return alert.message;
}

// A phone is not set up while it fails a rule that only Set up fixes: one
// adb can set (fixable) that the drift checker does not revert on its own
// (not autoFix), such as the removed apps, the default SMS app, the screen
// settings, Data Saver, and automatic updates. Rules adb cannot set (MMS
// with data off) and rules the drift checker sets on sight (radios, data,
// the radio log buffer) do not count.
export function needsSetup(results: RuleResultView[]): boolean {
	return results.some(
		(result) => !result.ok && result.fixable && !result.autoFix,
	);
}

export type SafetyProblem = {
	id: string;
	severity: RuleResultView["severity"];
	text: string;
};

// What a phone card says about safety. A check that failed is a problem of
// its own, listed with the failing rules of the last check that read them;
// a phone whose checks never read any rule is not checked yet, not OK.
export type SafetyStatus =
	| { kind: "unchecked" }
	| { kind: "ok"; rules: number; at: string }
	| { kind: "problems"; problems: SafetyProblem[] };

export function safetyStatus(
	check: CheckReportView | null | undefined,
): SafetyStatus {
	if (!check) return { kind: "unchecked" };
	const failing: SafetyProblem[] = problems(check.results).map((result) => ({
		id: result.id,
		severity: result.severity,
		text: problemText(result),
	}));
	if (check.error)
		failing.unshift({
			id: "check_failed",
			severity: "warning",
			text: `Last check failed: ${check.error}`,
		});
	if (failing.length > 0) return { kind: "problems", problems: failing };
	if (check.results.length === 0) return { kind: "unchecked" };
	return { kind: "ok", rules: check.results.length, at: check.at };
}

// The card's note about the last set-up: only a set-up that failed. One that
// finished with rules still failing needs no note; the card lists them.
export function provisionNote(
	provision: ProvisionReportView | null | undefined,
): string | null {
	return provision?.error ? `Set up failed: ${provision.error}` : null;
}
