import { beforeAll, expect, test } from "bun:test";
import { fixture } from "../../test/replay-shell";
import {
	evaluate,
	LOG_BUFFER_COMMAND,
	parseSnapshot,
	plan,
	presetDataOffCommands,
	READ_SCRIPT,
	REMOVE,
	RULES,
	radioHazard,
	type Snapshot,
} from "./policy";

// policy-read.txt is READ_SCRIPT's output recorded from a B30 that was
// already set up. Variants below change one recorded line, as a
// person tapping in Settings would; the phone itself must not be put in
// those states on demand (most of them turn a radio on).

let recorded: string;
beforeAll(async () => {
	recorded = await fixture("nuu-s6702x", "policy-read.txt");
});

const idle = { screenSession: false };
const resultsOf = (snapshot: Snapshot, context = idle) =>
	Object.fromEntries(
		evaluate(snapshot, context).map((result) => [
			result.id,
			[result.ok, result.observed],
		]),
	);
const variant = (...pairs: [string | RegExp, string][]) => {
	let text = recorded;
	for (const [from, to] of pairs) {
		const next = text.replace(from, to);
		if (next === text) throw new Error(`fixture has no ${String(from)}`);
		text = next;
	}
	return parseSnapshot(text);
};
// A second SIM in slot 1 as sub 2, in the recorded isub line format.
const withSecondSim = (policies = "") =>
	variant([
		/(Active subscriptions:\n)(.*SubscriptionInfoInternal: id=1 .*)\n/,
		(_: string, header: string, line: string) =>
			`${header}${line}\n${line
				.replace("id=1 ", "id=2 ")
				.replace("simSlotIndex=0", "simSlotIndex=1")
				.replace(
					"enabledMobileDataPolicies=2",
					`enabledMobileDataPolicies=${policies}`,
				)}\n`,
	] as unknown as [RegExp, string]);

test("the read is one shell call covering every rule and the data watch", () => {
	expect(READ_SCRIPT).not.toContain("\n");
	for (const section of [
		"settings:global",
		"settings:secure",
		"settings:system",
		"isub",
		"registry",
		"datasaver",
		"smsrole",
		"bloat",
		"radiobuf",
		"clock",
		"netstats",
	])
		expect(READ_SCRIPT).toContain(`@@sl:${section}\\n`);
	// Read-only: nothing that writes.
	expect(READ_SCRIPT).not.toMatch(
		/settings put|svc |uninstall|netpolicy (set|add)|role add/,
	);
});

test("the provisioned B30 passes every rule but the radio log buffer size", () => {
	const snapshot = parseSnapshot(recorded);
	expect(snapshot.subs).toEqual([{ subId: 1, slot: 0, dataPolicies: [2] }]);
	expect(snapshot.radioBufferBytes).toBe(256 * 1024);
	expect(snapshot.phoneClock).toBeGreaterThan(1_790_000_000);
	expect(resultsOf(snapshot)).toEqual({
		mobile_data: [true, "mobile_data=0, mobile_data1=0"],
		data_roaming: [true, "data_roaming=0, data_roaming1=0"],
		wifi_off: [true, "0"],
		wifi_wakeup_off: [true, "0"],
		wifi_scan_off: [true, "0"],
		ble_scan_off: [true, "0"],
		bluetooth_off: [true, "0"],
		location_off: [true, "0"],
		ota_off: [true, "1"],
		stay_on_off: [true, "0"],
		brightness_manual: [true, "0"],
		brightness_low: [true, "10"],
		screen_timeout: [true, "15000"],
		data_saver: [true, "enabled"],
		sms_role: [true, "com.google.android.apps.messaging"],
		debloat: [true, "none installed"],
		mms_with_data_off: [true, "SIM 1 (sub 1) on"],
		// persist.logd.size reads 8388608 on the B30, but the buffer in effect
		// is 256 KiB.
		log_buffer: [false, "256 KiB"],
	});
	// The buffer is the only thing to fix, and the drift checker fixes it.
	const fix = [{ rule: "log_buffer", commands: [LOG_BUFFER_COMMAND] }];
	expect(plan(snapshot, idle, () => true)).toEqual(fix);
	expect(plan(snapshot, idle, (rule) => rule.autoFix)).toEqual(fix);
	expect(LOG_BUFFER_COMMAND).toBe("logcat -b main,system,radio,crash -G 8M");
	// What the next read shows once logd took the size.
	const resized = variant(["ring buffer is 256 KiB", "ring buffer is 8 MiB"]);
	expect(resultsOf(resized).log_buffer).toEqual([true, "8 MiB"]);
	expect(plan(resized, idle, () => true)).toEqual([]);
});

test("radio drift plans exactly the commands that turn it off", () => {
	const drifted = variant(
		["wifi_on=0", "wifi_on=1"],
		["bluetooth_on=0", "bluetooth_on=1"],
		["wifi_scan_always_enabled=0", "wifi_scan_always_enabled=1"],
		["location_mode=0", "location_mode=3"],
		["mobile_data1=0", "mobile_data1=1"],
		["data_roaming=0", "data_roaming=1"],
		["ring buffer is 256 KiB", "ring buffer is 8 MiB"],
	);
	expect(plan(drifted, idle, (rule) => rule.autoFix)).toEqual([
		{
			rule: "mobile_data",
			commands: [
				"svc data disable",
				"settings put global mobile_data 0",
				"settings put global mobile_data1 0",
			],
		},
		{
			rule: "data_roaming",
			commands: [
				"settings put global data_roaming 0",
				"settings put global data_roaming1 0",
			],
		},
		{ rule: "wifi_off", commands: ["svc wifi disable"] },
		{
			rule: "wifi_scan_off",
			commands: ["settings put global wifi_scan_always_enabled 0"],
		},
		{
			rule: "bluetooth_off",
			commands: [
				"cmd bluetooth_manager disable",
				"cmd bluetooth_manager wait-for-state:STATE_OFF",
			],
		},
		{ rule: "location_off", commands: ["settings put secure location_mode 0"] },
	]);
});

test("a new SIM whose per-SIM data key is unset counts as data on", () => {
	const snapshot = withSecondSim();
	const mobile = evaluate(snapshot, idle).find((r) => r.id === "mobile_data");
	expect(mobile).toMatchObject({
		ok: false,
		observed: "mobile_data=0, mobile_data1=0, mobile_data2=unset",
	});
	expect(
		plan(snapshot, idle, (rule) => rule.autoFix).find(
			(step) => step.rule === "mobile_data",
		)?.commands,
	).toEqual([
		"svc data disable",
		"settings put global mobile_data 0",
		"settings put global mobile_data1 0",
		"settings put global mobile_data2 0",
	]);
	expect(resultsOf(snapshot).mms_with_data_off).toEqual([
		false,
		"SIM 1 (sub 1) on, SIM 2 (sub 2) off",
	]);
	expect(resultsOf(withSecondSim("1,2")).mms_with_data_off?.[0]).toBe(true);
});

test("telephony reporting data enabled fails the data rule even with settings off", () => {
	const snapshot = variant(["mIsDataEnabled=false", "mIsDataEnabled=true"]);
	expect(resultsOf(snapshot).mobile_data).toEqual([
		false,
		"mobile_data=0, mobile_data1=0, slot 0 data enabled",
	]);
});

test("check-only rules carry the on-phone instruction and no commands", () => {
	const snapshot = variant([
		"enabledMobileDataPolicies=2",
		"enabledMobileDataPolicies=",
	]);
	const results = evaluate(snapshot, idle);
	const mms = results.find((result) => result.id === "mms_with_data_off");
	expect(mms).toMatchObject({ ok: false, fixable: false });
	expect(mms?.instruction).toContain("MMS messages");
	expect(
		plan(snapshot, idle, () => true).map((step) => step.rule),
	).not.toContain("mms_with_data_off");
});

test("the log buffer is set automatically, with no action asked of the owner", () => {
	const buffer = evaluate(parseSnapshot(recorded), idle).find(
		(result) => result.id === "log_buffer",
	);
	expect(buffer).toMatchObject({
		ok: false,
		fixable: true,
		autoFix: true,
		severity: "info",
		instruction:
			"The phone ignored the buffer size; it will be retried on the next check.",
	});
	expect(RULES.find((rule) => rule.id === "log_buffer")?.fixEvent).toBe(
		"log_buffer_set",
	);
});

test("the radio buffer passes from 4 MiB, by the size logcat reports in effect", () => {
	const sized = (size: string) =>
		resultsOf(variant(["ring buffer is 256 KiB", `ring buffer is ${size}`]))
			.log_buffer;
	expect(sized("8 MiB")).toEqual([true, "8 MiB"]);
	expect(sized("4 MiB")).toEqual([true, "4 MiB"]);
	expect(sized("4095 KiB")).toEqual([false, "4095 KiB"]);
	expect(sized("1 MiB")).toEqual([false, "1 MiB"]);
	expect(
		resultsOf(
			variant([
				/radio: ring buffer is .*/,
				"logcat: Unable to open log device",
			]),
		).log_buffer,
	).toEqual([false, "unreadable"]);
});

test("scrcpy's stay-awake setting is accepted only during a screen session", () => {
	const awake = variant([
		"stay_on_while_plugged_in=0",
		"stay_on_while_plugged_in=3",
	]);
	expect(resultsOf(awake).stay_on_off).toEqual([false, "3"]);
	expect(resultsOf(awake, { screenSession: true }).stay_on_off).toEqual([
		true,
		"3 (screen session)",
	]);
	expect(
		plan(awake, { screenSession: true }, () => true).map((s) => s.rule),
	).not.toContain("stay_on_off");
	expect(
		plan(awake, idle, () => true).find((s) => s.rule === "stay_on_off")
			?.commands,
	).toEqual(["settings put global stay_on_while_plugged_in 0"]);
});

test("debloat uninstalls only listed apps that are still installed", () => {
	const snapshot = variant([
		"@@sl:bloat\n",
		"@@sl:bloat\npackage:com.android.chrome\npackage:com.example.notlisted\npackage:com.google.android.dialer\n",
	]);
	expect(resultsOf(snapshot).debloat).toEqual([
		false,
		"installed: com.android.chrome, com.google.android.dialer",
	]);
	const commands =
		plan(snapshot, idle, () => true).find((step) => step.rule === "debloat")
			?.commands ?? [];
	expect(commands).toEqual([
		"pm uninstall --user 0 com.android.chrome",
		"pm uninstall --user 0 com.google.android.dialer",
	]);
	for (const command of commands)
		expect(REMOVE as readonly string[]).toContain(
			command.replace("pm uninstall --user 0 ", ""),
		);
});

test("the radio guard refuses anything that could turn a radio or data on", () => {
	for (const command of [
		"svc data enable",
		"svc wifi enable",
		"svc bluetooth enable",
		"cmd bluetooth_manager enable",
		"cmd wifi set-wifi-enabled enabled",
		"cmd phone data enable",
		"cmd location set-location-enabled true",
		"settings put global mobile_data 1",
		"settings put global mobile_data2 1",
		"settings put global data_roaming1 1",
		"settings put global wifi_on 1",
		"settings put global bluetooth_on 1",
		"settings put global wifi_scan_always_enabled 1",
		"settings put global ble_scan_always_enabled 1",
		"settings put global wifi_wakeup_enabled 1",
		"settings put secure location_mode 3",
		"settings put global airplane_mode_on 0",
		"cmd connectivity airplane-mode disable",
		"cmd netpolicy set restrict-background false",
		"settings put global mobile_data 0; settings put global wifi_on 1",
	])
		expect([command, radioHazard(command)]).not.toEqual([command, null]);
	for (const command of [
		"svc data disable",
		"settings put global mobile_data1 0",
		"settings put secure location_mode 0",
		"cmd netpolicy set restrict-background true",
	])
		expect(radioHazard(command)).toBeNull();
});

test("the radio guard allows the log buffer command but nothing enabling beside it", () => {
	// "-b radio" names a log buffer, not the radio.
	expect(radioHazard(LOG_BUFFER_COMMAND)).toBeNull();
	expect(radioHazard("logcat -b radio -G 8M")).toBeNull();
	for (const enabling of [
		"svc data enable",
		"svc wifi enable",
		"cmd bluetooth_manager enable",
		"settings put global mobile_data1 1",
		"settings put secure location_mode 3",
		"cmd connectivity airplane-mode disable",
	]) {
		const command = `${LOG_BUFFER_COMMAND}; ${enabling}`;
		expect([command, radioHazard(command)]).not.toEqual([command, null]);
		expect([enabling, radioHazard(enabling)]).not.toEqual([enabling, null]);
	}
});

test("no rule's enforce command ever enables a radio or data", () => {
	const everythingWrong = variant(
		["wifi_on=0", "wifi_on=1"],
		["bluetooth_on=0", "bluetooth_on=1"],
		["wifi_wakeup_enabled=0", "wifi_wakeup_enabled=1"],
		["wifi_scan_always_enabled=0", "wifi_scan_always_enabled=1"],
		["ble_scan_always_enabled=0", "ble_scan_always_enabled=1"],
		["location_mode=0", "location_mode=3"],
		["mobile_data=0", "mobile_data=1"],
		["mobile_data1=0", "mobile_data1=1"],
		["data_roaming=0", "data_roaming=1"],
		["ota_disable_automatic_update=1", "ota_disable_automatic_update=0"],
		["stay_on_while_plugged_in=0", "stay_on_while_plugged_in=7"],
		["screen_brightness=10", "screen_brightness=255"],
		["status: enabled", "status: disabled"],
		["@@sl:bloat\n", "@@sl:bloat\npackage:com.android.vending\n"],
	);
	const snapshots = [
		parseSnapshot(recorded),
		everythingWrong,
		withSecondSim(),
		parseSnapshot(""),
	];
	let commands = 0;
	for (const snapshot of snapshots)
		for (const context of [idle, { screenSession: true }])
			for (const rule of RULES)
				for (const command of rule.enforce?.(snapshot, context) ?? []) {
					commands++;
					expect([command, radioHazard(command)]).toEqual([command, null]);
				}
	expect(commands).toBeGreaterThan(40);
});

test("the next four subscription ids get their data key preset, once", () => {
	const snapshot = parseSnapshot(recorded);
	expect(snapshot.knownSubIds).toContain(1);
	expect(presetDataOffCommands(snapshot)).toEqual([
		"settings put global mobile_data2 0",
		"settings put global mobile_data3 0",
		"settings put global mobile_data4 0",
		"settings put global mobile_data5 0",
	]);
	// Keys already 0 are skipped; a SIM seen before (sub 7, no longer in a
	// slot) moves the range past it.
	const later = variant(
		["mobile_data1=0", "mobile_data1=0\nmobile_data2=0\nmobile_data8=1"],
		[
			/(All subscriptions:\n)/,
			"$1  [SubscriptionInfoInternal: id=7 iccId=890155500[****] simSlotIndex=-1 ]\n",
		],
	);
	expect(later.knownSubIds).toContain(7);
	expect(presetDataOffCommands(later)).toEqual([
		"settings put global mobile_data8 0",
		"settings put global mobile_data9 0",
		"settings put global mobile_data10 0",
		"settings put global mobile_data11 0",
	]);
	for (const command of presetDataOffCommands(later))
		expect(radioHazard(command)).toBeNull();
});

test("safety rules are exactly the radio and data rules", () => {
	const safety = RULES.filter((rule) => rule.safety).map((rule) => rule.id);
	expect(safety).toEqual([
		"mobile_data",
		"data_roaming",
		"wifi_off",
		"wifi_wakeup_off",
		"wifi_scan_off",
		"ble_scan_off",
		"bluetooth_off",
		"location_off",
	]);
	// The same set the critical drift alert used before the flag.
	expect(
		RULES.filter((rule) => rule.autoFix && rule.severity === "critical").map(
			(rule) => rule.id,
		),
	).toEqual(safety);
	expect(RULES.find((rule) => rule.id === "log_buffer")?.safety).toBeFalsy();
});

test("an unreadable buffer size says so instead of blaming the phone", () => {
	const unreadable = evaluate(
		variant([/radio: ring buffer is .*/, "logcat: Unable to open log device"]),
		idle,
	).find((result) => result.id === "log_buffer");
	expect(unreadable).toMatchObject({
		ok: false,
		observed: "unreadable",
		instruction:
			"Couldn't read the buffer size; it will be retried on the next check.",
	});
});
