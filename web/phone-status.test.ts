import { beforeAll, describe, expect, test } from "bun:test";
import type {
	AlertView,
	CheckReportView,
	RuleResultView,
} from "../src/api/monitor-schema";
import { evaluate, parseSnapshot } from "../src/monitor/policy";
import { fixture } from "../test/replay-shell";
import {
	alertText,
	bannerAlerts,
	needsSetup,
	problems,
	problemText,
	provisionNote,
	safetyStatus,
} from "./phone-status";

// Rule results from the real policy, evaluated on READ_SCRIPT's output
// recorded from the B30 (set up, radio log buffer 256 KiB), and on that
// recording with lines changed the way tapping in Settings would.

let recorded: string;
beforeAll(async () => {
	recorded = await fixture("nuu-s6702x", "policy-read.txt");
});

const resultsOf = (text: string): RuleResultView[] =>
	evaluate(parseSnapshot(text), { screenSession: false });

const change = (from: string | RegExp, to: string) => {
	const next = recorded.replace(from, to);
	if (next === recorded) throw new Error(`fixture has no ${String(from)}`);
	return next;
};

// The recorded read is from before the drift checker set the buffer; a card
// shows it only when the phone ignored the size, so the text says it will be
// retried and asks nothing of the owner.
test("a radio log buffer the phone did not take reads as a retry, not a task", () => {
	const failing = problems(resultsOf(recorded));
	expect(failing.map((result) => result.id)).toEqual(["log_buffer"]);
	expect(problemText(failing[0] as RuleResultView)).toBe(
		"Radio log buffer at least 4 MiB: now 256 KiB. The phone ignored the buffer size; it will be retried on the next check.",
	);
});

test("problems list the most severe first", () => {
	const text = change(
		/^screen_brightness=10$/m,
		"screen_brightness=200",
	).replace(
		"Restrict background status: enabled",
		"Restrict background status: disabled",
	);
	expect(problems(resultsOf(text)).map((result) => result.id)).toEqual([
		"data_saver",
		"brightness_low",
		"log_buffer",
	]);
});

test("an alert about a rule is worded from the rule, without the technical target", () => {
	const results = resultsOf(recorded);
	const alert: AlertView = {
		key: "policy:SPARELINE00001:log_buffer",
		kind: "policy",
		serial: "SPARELINE00001",
		severity: "info",
		message:
			"Radio log buffer at least 4 MiB: 256 KiB (want logcat -g -b radio reports a ring buffer of 4 MiB or more). On the phone: …",
		since: "2026-10-22T00:00:00.000Z",
	};
	const text = alertText(alert, results);
	expect(text).toStartWith("Radio log buffer at least 4 MiB: now 256 KiB.");
	expect(text).not.toContain("want");
	// Other alerts, or a rule the phone no longer fails, keep their message.
	const offline: AlertView = {
		...alert,
		key: "offline:X",
		kind: "offline",
		message: "Offline for 5 min",
	};
	expect(alertText(offline, results)).toBe("Offline for 5 min");
	expect(alertText(alert, undefined)).toBe(alert.message);
});

test("the set-up B30 does not need setting up; its log buffer is not a set-up rule", () => {
	expect(needsSetup(resultsOf(recorded))).toBe(false);
});

test("a phone that fails any rule only Set up fixes needs setting up", () => {
	const notSetUp = [
		change(
			"Restrict background status: enabled",
			"Restrict background status: disabled",
		),
		change(/^screen_brightness=10$/m, "screen_brightness=200"),
		change(/^screen_off_timeout=15000$/m, "screen_off_timeout=60000"),
	];
	for (const text of notSetUp) expect(needsSetup(resultsOf(text))).toBe(true);
});

test("a radio rule the drift checker reverts does not mean the phone needs setting up", () => {
	const results = resultsOf(recorded).map((result) =>
		result.id === "wifi_off" ? { ...result, ok: false } : result,
	);
	expect(needsSetup(results)).toBe(false);
});

test("the banner leaves out a rule alert the phone's card already lists, and keeps the rest", () => {
	const results = resultsOf(recorded);
	const alert = (kind: AlertView["kind"], key: string): AlertView => ({
		key,
		kind,
		serial: "SPARELINE00001",
		severity: "warning",
		message: key,
		since: "2026-10-22T00:00:00.000Z",
	});
	const logBuffer = alert("policy", "policy:SPARELINE00001:log_buffer");
	const offline = alert("offline", "offline:SPARELINE00001");
	const unsecured = alert("sim_unsecured", "sim_unsecured:SPARELINE00001");
	const radio = alert("drift_unfixed", "drift_unfixed:SPARELINE00001:wifi_off");
	// The card lists a failed check as Last check failed.
	const failedCheck = alert("check_failed", "check_failed:SPARELINE00001");
	// A rule alert whose phone has no results to show it on stays.
	const elsewhere = {
		...logBuffer,
		serial: "OTHER",
		key: "policy:OTHER:log_buffer",
	};
	const resultsFor = (serial: string) =>
		serial === "SPARELINE00001" ? results : undefined;
	expect(
		bannerAlerts(
			[logBuffer, offline, unsecured, radio, elsewhere, failedCheck],
			resultsFor,
		),
	).toEqual([offline, unsecured, radio, elsewhere]);
	// A phone whose log buffer did not take: its only alert is the item its
	// card already lists.
	expect(bannerAlerts([logBuffer], resultsFor)).toEqual([]);
});

describe("safetyStatus", () => {
	const report = (
		results: RuleResultView[],
		error: string | null = null,
	): CheckReportView => ({
		at: "2026-10-22T00:00:00.000Z",
		reason: "connect",
		readMs: error ? 0 : 640,
		results,
		usage: [],
		reverted: [],
		error,
	});
	const passing = () =>
		resultsOf(recorded).map((result) => ({ ...result, ok: true }));

	test("a phone with no check, or whose checks never read a rule, is not checked yet", () => {
		expect(safetyStatus(null)).toEqual({ kind: "unchecked" });
		expect(safetyStatus(report([]))).toEqual({ kind: "unchecked" });
	});

	test("a phone passing every rule is OK", () => {
		expect(safetyStatus(report(passing()))).toMatchObject({ kind: "ok" });
	});

	test("a failed check is a problem, even before any rule was read", () => {
		expect(safetyStatus(report([], "phone is not online"))).toEqual({
			kind: "problems",
			problems: [
				{
					id: "check_failed",
					severity: "warning",
					text: "Last check failed: phone is not online",
				},
			],
		});
	});

	test("a failed check is listed first, with the last check's failing rules", () => {
		const status = safetyStatus(report(resultsOf(recorded), "timed out"));
		expect(
			status.kind === "problems" && status.problems.map((p) => p.id),
		).toEqual(["check_failed", "log_buffer"]);
		// Stale results that all passed still do not make a failed check OK.
		expect(safetyStatus(report(passing(), "timed out")).kind).toBe("problems");
	});
});

test("only a failed set-up gets a note; one that left check-only rules failing does not", () => {
	const finished = {
		at: "2026-10-22T00:00:00.000Z",
		durationMs: 1200,
		steps: [],
		results: resultsOf(recorded),
		ok: false,
		error: null,
	};
	expect(provisionNote(finished)).toBeNull();
	expect(provisionNote(null)).toBeNull();
	expect(provisionNote({ ...finished, error: "adb: device offline" })).toBe(
		"Set up failed: adb: device offline",
	);
});
