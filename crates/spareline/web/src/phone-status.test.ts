import { describe, expect, test } from "bun:test";
import type { AlertView, CheckReportView, RuleResultView } from "./api-types";
import {
	alertText,
	bannerAlerts,
	needsSetup,
	problems,
	problemText,
	provisionNote,
	safetyStatus,
} from "./phone-status";

// The rule results the server reports for a set-up NUU B30 under the B30
// example profile (nuu-b30-no-data.json), whose radio log buffer is still
// 256 KiB, and those results with rules failing the way tapping in Settings
// would make them.

const rule = (
	id: string,
	describe: string,
	want: string,
	severity: RuleResultView["severity"],
	observed: string,
	extra: Partial<RuleResultView> = {},
): RuleResultView => ({
	id,
	describe,
	want,
	severity,
	autoFix: false,
	instruction: null,
	fixable: true,
	ok: true,
	observed,
	...extra,
});

const radio = { autoFix: true };

const SET_UP_B30: readonly RuleResultView[] = [
	rule(
		"mobile_data",
		"Mobile data off, on every SIM",
		"global mobile_data and mobile_data<subId> = 0",
		"critical",
		"mobile_data=0, mobile_data1=0",
		radio,
	),
	rule(
		"data_roaming",
		"Data roaming off, on every SIM",
		"global data_roaming and data_roaming<subId> = 0",
		"critical",
		"data_roaming=0, data_roaming1=0",
		radio,
	),
	rule("wifi_off", "Wi-Fi off", "global wifi_on = 0", "critical", "0", radio),
	rule(
		"wifi_wakeup_off",
		"Wi-Fi does not turn itself back on",
		"global wifi_wakeup_enabled = 0",
		"critical",
		"0",
		radio,
	),
	rule(
		"wifi_scan_off",
		"Wi-Fi scanning off",
		"global wifi_scan_always_enabled = 0",
		"critical",
		"0",
		radio,
	),
	rule(
		"ble_scan_off",
		"Bluetooth scanning off",
		"global ble_scan_always_enabled = 0",
		"critical",
		"0",
		radio,
	),
	rule(
		"bluetooth_off",
		"Bluetooth off",
		"global bluetooth_on = 0",
		"critical",
		"0",
		radio,
	),
	rule(
		"location_off",
		"Location off",
		"secure location_mode = 0",
		"critical",
		"0",
		radio,
	),
	rule(
		"ota_off",
		"Automatic system updates off",
		"global ota_disable_automatic_update = 1",
		"warning",
		"1",
	),
	rule(
		"stay_on_off",
		"Screen does not stay on while charging",
		"global stay_on_while_plugged_in = 0",
		"info",
		"0",
	),
	rule(
		"brightness_manual",
		"Manual brightness",
		"system screen_brightness_mode = 0",
		"info",
		"0",
	),
	rule(
		"brightness_low",
		"Brightness 10 of 255",
		"system screen_brightness = 10",
		"info",
		"10",
	),
	rule(
		"screen_timeout",
		"Screen turns off after 15 s",
		"system screen_off_timeout = 15000",
		"info",
		"15000",
	),
	rule(
		"data_saver",
		"Data Saver on",
		"restrict-background enabled",
		"warning",
		"enabled",
	),
	rule(
		"sms_role",
		"Google Messages is the default SMS app",
		"com.google.android.apps.messaging",
		"critical",
		"com.google.android.apps.messaging",
	),
	rule(
		"debloat",
		"Unneeded apps removed",
		"none of the 37 listed apps installed for user 0",
		"warning",
		"none installed",
	),
	rule(
		"mms_with_data_off",
		"MMS works with mobile data off, on every SIM",
		"enabledMobileDataPolicies includes 2 (MMS_ALWAYS_ALLOWED)",
		"critical",
		"SIM 1 (sub 1) on",
		{
			fixable: false,
			instruction:
				"On the phone: Settings > Network & internet > SIMs > (the SIM) > turn on MMS messages.",
		},
	),
	rule(
		"log_buffer",
		"Radio log buffer at least 8 MiB",
		"logcat -g -b radio reports a ring buffer of 8 MiB or more",
		"info",
		"256 KiB",
		{
			autoFix: true,
			ok: false,
			instruction:
				"The phone ignored the buffer size; it will be retried on the next check.",
		},
	),
	rule(
		"lock_screen_notifications_off",
		"Notifications hidden on the lock screen",
		"secure lock_screen_show_notifications = 0",
		"warning",
		"0",
	),
	rule(
		"lock_screen_notice",
		'Lock screen shows "Managed by spareline"',
		"lock screen wallpaper named spareline-7d53969d19f0d42e",
		"info",
		"spareline-7d53969d19f0d42e",
	),
];

// The set-up B30's results, with each rule in `observed` failing on that
// value.
const resultsOf = (observed: Record<string, string> = {}): RuleResultView[] => {
	for (const id of Object.keys(observed))
		if (!SET_UP_B30.some((result) => result.id === id))
			throw new Error(`the B30 profile has no rule ${id}`);
	return SET_UP_B30.map((result) =>
		result.id in observed
			? { ...result, ok: false, observed: observed[result.id] as string }
			: { ...result },
	);
};

// The B30's buffer is from before the drift checker set it; a card shows it
// only when the phone ignored the size, so the text says it will be retried
// and asks nothing of the owner.
test("a radio log buffer the phone did not take reads as a retry, not a task", () => {
	const failing = problems(resultsOf());
	expect(failing.map((result) => result.id)).toEqual(["log_buffer"]);
	expect(problemText(failing[0] as RuleResultView)).toBe(
		"Radio log buffer at least 8 MiB: now 256 KiB. The phone ignored the buffer size; it will be retried on the next check.",
	);
});

test("problems list the most severe first", () => {
	const results = resultsOf({
		brightness_low: "200",
		data_saver: "disabled",
	});
	expect(problems(results).map((result) => result.id)).toEqual([
		"data_saver",
		"brightness_low",
		"log_buffer",
	]);
});

test("an alert about a rule is worded from the rule, without the technical target", () => {
	const results = resultsOf();
	const alert: AlertView = {
		key: "policy:SPARELINE00001:log_buffer",
		kind: "policy",
		serial: "SPARELINE00001",
		severity: "info",
		message:
			"Radio log buffer at least 8 MiB: 256 KiB (want logcat -g -b radio reports a ring buffer of 8 MiB or more). On the phone: …",
		since: "2026-10-22T00:00:00.000Z",
	};
	const text = alertText(alert, results);
	expect(text).toStartWith("Radio log buffer at least 8 MiB: now 256 KiB.");
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
	expect(needsSetup(resultsOf())).toBe(false);
});

test("a phone that fails any rule only Set up fixes needs setting up", () => {
	const notSetUp = [
		resultsOf({ data_saver: "disabled" }),
		resultsOf({ brightness_low: "200" }),
		resultsOf({ screen_timeout: "60000" }),
	];
	for (const results of notSetUp) expect(needsSetup(results)).toBe(true);
});

test("a radio rule the drift checker reverts does not mean the phone needs setting up", () => {
	const results = resultsOf().map((result) =>
		result.id === "wifi_off" ? { ...result, ok: false } : result,
	);
	expect(needsSetup(results)).toBe(false);
});

test("the banner leaves out a rule alert the phone's card already lists, and keeps the rest", () => {
	const results = resultsOf();
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
	const passing = () => resultsOf().map((result) => ({ ...result, ok: true }));

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
		const status = safetyStatus(report(resultsOf(), "timed out"));
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
		results: resultsOf(),
		ok: false,
		error: null,
	};
	expect(provisionNote(finished)).toBeNull();
	expect(provisionNote(null)).toBeNull();
	expect(provisionNote({ ...finished, error: "adb: device offline" })).toBe(
		"Set up failed: adb: device offline",
	);
});
