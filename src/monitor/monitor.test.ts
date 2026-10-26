import { afterEach, expect, test } from "bun:test";
import { B30_POLICY, LOG_BUFFER_COMMAND } from "../../test/b30-policy";
import { recordedTrackLists } from "../../test/fake-adb";
import {
	B30,
	type MonitorPhone,
	startMonitor,
	until,
	withMeteredTraffic,
} from "../../test/monitor-phone";
import { fixture } from "../../test/replay-shell";
import { HEALTH_SCRIPT, IDENTITY_SCRIPT } from "../devices/probe";
import { SIM_LOG_COMMAND } from "./sims";

// The phone is a MonitorPhone: the B30's recorded policy read, with the
// monitor's writes applied to the recording (see test/monitor-phone.ts for why).
// The device manager, inventory, database, and monitor are real.

let stop: (() => Promise<void>) | undefined;
afterEach(async () => {
	await stop?.();
	stop = undefined;
});
const start = async (options: Parameters<typeof startMonitor>[0] = {}) => {
	const setup = await startMonitor(options);
	stop = setup.stop;
	return setup;
};
const detail = (
	events: { kind: string; detail: Record<string, unknown> }[],
	kind: string,
) => events.filter((event) => event.kind === kind).map((event) => event.detail);
const PRESET = [2, 3, 4, 5].map(
	(subId) => `settings put global mobile_data${subId} 0`,
);
const OFF_NOW = ["svc data disable", "settings put global mobile_data 0"];

test("a phone coming online is checked, gets its log buffer and the next SIMs' data keys", async () => {
	const { atConnect, readsAtConnect, monitor, events } = await start();
	// The recorded B30 has 256 KiB log buffers, as after every reboot, and
	// knows sub 1; the next four ids get their data key written as 0.
	expect(atConnect).toEqual([...PRESET, LOG_BUFFER_COMMAND]);
	expect(readsAtConnect).toBe(2);
	const check = monitor.phone(B30)?.check;
	expect(check?.reason).toBe("connect");
	expect(check?.error).toBeNull();
	expect(check?.reverted).toEqual([
		{ rule: "log_buffer", before: "256 KiB", after: "8 MiB", fixed: true },
	]);
	expect(check?.results.every((result) => result.ok)).toBe(true);
	// Routine after a reboot: a quiet event, no drift, no alert.
	expect(detail(events, "log_buffer_set")).toEqual([
		{
			rule: "log_buffer",
			before: "256 KiB",
			after: "8 MiB",
			fixed: true,
			reason: "connect",
		},
	]);
	expect(detail(events, "drift")).toEqual([]);
	expect(detail(events, "alert.raised")).toEqual([]);
	expect(monitor.alerts()).toEqual([]);
});

test("a phone that ignores the buffer size gets a quiet card item, retried next check", async () => {
	const { monitor, phone, events } = await start({
		prepare: (phone) => phone.stuck.add("logd"),
	});
	await monitor.check(B30);
	await monitor.check(B30);
	// Three attempts, none of which took: no event claims it was enlarged.
	expect(phone.applied).toEqual([LOG_BUFFER_COMMAND, LOG_BUFFER_COMMAND]);
	expect(detail(events, "log_buffer_set")).toEqual([]);
	expect(detail(events, "drift")).toEqual([]);
	expect(monitor.alerts().map((alert) => [alert.key, alert.severity])).toEqual([
		[`policy:${B30}:log_buffer`, "info"],
	]);
	expect(monitor.alerts()[0]?.message).toEndWith(
		"The phone ignored the buffer size; it will be retried on the next check.",
	);
	phone.stuck.delete("logd");
	await monitor.check(B30);
	expect(phone.applied).toHaveLength(3);
	expect(monitor.alerts()).toEqual([]);
	expect(detail(events, "log_buffer_set")).toEqual([
		{
			rule: "log_buffer",
			before: "256 KiB",
			after: "8 MiB",
			fixed: true,
			reason: "manual",
		},
	]);
});

test("the server log names a log buffer fix log-buffer-set, not drift", async () => {
	const lines: [string, Record<string, unknown> | undefined][] = [];
	await start({
		log: (event, fields) => {
			lines.push([event, fields]);
		},
	});
	expect(lines.filter(([event]) => event === "drift")).toEqual([]);
	expect(lines.filter(([event]) => event === "log-buffer-set")).toEqual([
		[
			"log-buffer-set",
			{
				serial: B30,
				reason: "connect",
				rule: "log_buffer",
				before: "256 KiB",
				after: "8 MiB",
				fixed: true,
			},
		],
	]);
});

test("first sight applies only what the drift checker fixes, safety first", async () => {
	const { atConnect, events, monitor } = await start({
		prepare: (phone) => {
			phone.set("global", "wifi_scan_always_enabled", "1");
			phone.set("system", "screen_off_timeout", "600000");
			phone.setSection("bloat", "package:com.android.vending");
			phone.setSection("smsrole", "com.android.messaging");
		},
	});
	// Radio fix and preset data keys before the log buffer; nothing that
	// only set-up does (screen timeout, SMS app, debloat).
	expect(atConnect).toEqual([
		"settings put global wifi_scan_always_enabled 0",
		...PRESET,
		LOG_BUFFER_COMMAND,
	]);
	expect(detail(events, "provision")).toEqual([]);
	expect(
		monitor
			.phone(B30)
			?.check?.results.filter((result) => !result.ok)
			.map((result) => result.id),
	).toEqual(["screen_timeout", "sms_role", "debloat"]);
});

test("radio drift is reverted on sight and logged with before and after", async () => {
	const { phone, monitor, events } = await start();
	phone.set("global", "wifi_scan_always_enabled", "1");
	phone.set("secure", "location_mode", "3");
	const report = await monitor.check(B30);
	expect(phone.applied).toEqual([
		"settings put global wifi_scan_always_enabled 0",
		"settings put secure location_mode 0",
	]);
	expect(phone.get("global", "wifi_scan_always_enabled")).toBe("0");
	expect(report.reverted).toEqual([
		{ rule: "wifi_scan_off", before: "1", after: "0", fixed: true },
		{ rule: "location_off", before: "3", after: "0", fixed: true },
	]);
	expect(detail(events, "drift")).toEqual([
		{
			rule: "wifi_scan_off",
			before: "1",
			after: "0",
			fixed: true,
			reason: "manual",
		},
		{
			rule: "location_off",
			before: "3",
			after: "0",
			fixed: true,
			reason: "manual",
		},
	]);
	// Read, apply, read again.
	expect(phone.reads).toHaveLength(2);
});

test("other drift raises an alert and changes nothing", async () => {
	const { phone, monitor, events } = await start();
	phone.set("system", "screen_brightness", "200");
	phone.set("global", "ota_disable_automatic_update", "0");
	await monitor.check(B30);
	expect(phone.applied).toEqual([]);
	expect(detail(events, "alert.raised").map((d) => d.key)).toEqual([
		`policy:${B30}:ota_off`,
		`policy:${B30}:brightness_low`,
	]);
	phone.set("system", "screen_brightness", "10");
	await monitor.check(B30);
	expect(detail(events, "alert.cleared")).toEqual([
		{ key: `policy:${B30}:brightness_low`, kind: "policy" },
	]);
});

test("a radio that will not turn off raises a critical alert", async () => {
	const { phone, monitor } = await start();
	phone.set("global", "bluetooth_on", "1");
	phone.stuck.add("global:bluetooth_on");
	const report = await monitor.check(B30);
	expect(report.reverted).toEqual([
		{ rule: "bluetooth_off", before: "1", after: "1", fixed: false },
	]);
	expect(
		monitor.alerts().find((alert) => alert.kind === "drift_unfixed"),
	).toMatchObject({
		key: `drift_unfixed:${B30}:bluetooth_off`,
		severity: "critical",
	});
});

test("a check whose commands fail records the error and raises an alert", async () => {
	const { phone, monitor, events } = await start();
	phone.set("global", "wifi_on", "1");
	phone.failApply = new Error("device offline");
	const report = await monitor.check(B30);
	expect(report.error).toBe("device offline");
	expect(monitor.phone(B30)?.check?.error).toBe("device offline");
	expect(
		monitor.alerts().find((alert) => alert.kind === "check_failed"),
	).toMatchObject({
		key: `check_failed:${B30}`,
		message: "Policy check failed: device offline",
	});
	expect(detail(events, "drift")).toEqual([]);

	phone.failApply = null;
	const next = await monitor.check(B30);
	expect(next.error).toBeNull();
	expect(monitor.alerts().some((alert) => alert.kind === "check_failed")).toBe(
		false,
	);
});

test("a read with no settings is a failed check, not drift", async () => {
	const { phone, monitor, events } = await start();
	phone.setSection("settings:global", "");
	const report = await monitor.check(B30);
	expect(report.error).toBe("policy read returned no global settings");
	expect(phone.applied).toEqual([]);
	expect(detail(events, "drift")).toEqual([]);
	expect(monitor.alerts().some((alert) => alert.kind === "check_failed")).toBe(
		true,
	);
});

test("scrcpy's stay-awake is left alone while a screen session is active", async () => {
	let streaming = true;
	const { phone, monitor } = await start({
		isScreenSessionActive: (serial) => serial === B30 && streaming,
	});
	phone.set("global", "stay_on_while_plugged_in", "3");
	await monitor.check(B30);
	await monitor.provision(B30);
	expect(phone.applied).not.toContain(
		"settings put global stay_on_while_plugged_in 0",
	);
	expect(monitor.alerts().some((a) => a.key.endsWith(":stay_on_off"))).toBe(
		false,
	);
	streaming = false;
	await monitor.check(B30);
	expect(monitor.alerts().some((a) => a.key.endsWith(":stay_on_off"))).toBe(
		true,
	);
});

test("set-up applies every enforceable rule once; a second run changes nothing", async () => {
	const { phone, monitor, events, devices } = await start();
	phone.set("global", "wifi_on", "1");
	phone.set("system", "screen_off_timeout", "600000");
	phone.bigRadioBuffer();
	phone.setSection("datasaver", "Restrict background status: disabled");
	phone.setSection(
		"bloat",
		"package:com.android.vending\npackage:com.google.android.youtube",
	);
	const first = await monitor.provision(B30);
	expect(first).toMatchObject({ ok: true, error: null });
	expect(phone.applied).toEqual([
		"svc wifi disable",
		"settings put system screen_off_timeout 15000",
		"cmd netpolicy set restrict-background true",
		"pm uninstall --user 0 com.android.vending",
		"pm uninstall --user 0 com.google.android.youtube",
	]);
	expect(first.steps.map((step) => step.rule)).toEqual([
		"wifi_off",
		"screen_timeout",
		"data_saver",
		"debloat",
	]);
	const second = await monitor.provision(B30);
	expect(second).toMatchObject({ ok: true, steps: [] });
	expect(phone.applied).toHaveLength(5);
	expect(detail(events, "provision").map((d) => d.changed)).toEqual([
		["wifi_off", "screen_timeout", "data_saver", "debloat"],
		[],
	]);
	expect(monitor.alerts()).toEqual([]);

	// A reconnect checks; it does not set up again.
	for (const list of (await recordedTrackLists()).slice(1)) phone.send(list);
	await until(
		() =>
			monitor.phone(B30)?.check?.reason === "connect" &&
			!monitor.phone(B30)?.busy,
		"reconnect check",
	);
	await devices.settled();
	await monitor.settled();
	expect(detail(events, "provision")).toHaveLength(2);
});

const nowOf = (phone: MonitorPhone) => Number(phone.section("clock"));

test("internet data above the threshold turns data off again and turns the phone red", async () => {
	const { phone, monitor, events } = await start({
		dataThresholdBytes: 1_000_000,
	});
	phone.setSection(
		"netstats",
		withMeteredTraffic(phone.section("netstats"), {
			bytes: 1_500_000,
			now: nowOf(phone),
		}),
	);
	const report = await monitor.check(B30);
	expect(report.usage).toEqual([
		{
			subId: 1,
			slot: 0,
			rx: 1_500_000,
			tx: 0,
			uids: [{ uid: 10101, bytes: 1_500_000 }],
			over: true,
		},
	]);
	expect(phone.applied).toEqual([
		"svc data disable",
		"settings put global mobile_data 0",
		"settings put global mobile_data1 0",
	]);
	expect(detail(events, "data_threshold")).toEqual([
		{
			subId: 1,
			slot: 0,
			rx: 1_500_000,
			tx: 0,
			thresholdBytes: 1_000_000,
			uids: [{ uid: 10101, bytes: 1_500_000 }],
			dataOff: true,
		},
	]);
	expect(monitor.phone(B30)?.dataAlarm).toBe(true);
	expect(monitor.alerts().some((a) => a.kind === "data_threshold")).toBe(true);
	// Still over on the next cycle: data is turned off again, logged once.
	await monitor.check(B30);
	expect(phone.applied).toHaveLength(6);
	expect(detail(events, "data_threshold")).toHaveLength(1);
});

test("MMS and video-call traffic of the phone process does not count", async () => {
	const { phone, monitor } = await start({ dataThresholdBytes: 1_000 });
	phone.setSection(
		"netstats",
		withMeteredTraffic(phone.section("netstats"), {
			bytes: 5_000_000,
			now: nowOf(phone),
			uid: 1001,
		}),
	);
	const report = await monitor.check(B30);
	expect(report.usage).toEqual([]);
	expect(phone.applied).toEqual([]);
	expect(monitor.phone(B30)?.dataAlarm).toBe(false);
});

// A SIM appearing in slot 1 as `subId`, in the recorded isub line format. A
// SIM cannot be inserted on demand, so the stand-in changes isub and the SIM
// state, and the test feeds the log line the RIL would print.
function insertSim(phone: MonitorPhone, subId: number) {
	const isub = phone.section("isub");
	const line = isub
		.split("\n")
		.find((candidate) => candidate.includes("SubscriptionInfoInternal: id=1 "));
	if (!line) throw new Error("recorded isub has no sub 1");
	const second = line
		.replace("id=1 ", `id=${subId} `)
		.replace("simSlotIndex=0", "simSlotIndex=1")
		.replace("enabledMobileDataPolicies=2", "enabledMobileDataPolicies=");
	phone.setSection(
		"isub",
		isub.replace(
			"Active subscriptions:\n",
			`Active subscriptions:\n${second}\n`,
		),
	);
	phone.simState = "LOADED,LOADED";
}

const kindOf = (call: string) =>
	call.endsWith(B30_POLICY.readScript)
		? "read"
		: call.includes("@@sl:run:")
			? "apply"
			: call === `${B30}: dumpsys isub`
				? "isub"
				: "other";

test("a new SIM: data off at once, then its own key, then a verified check and an inventory read", async () => {
	const { phone, monitor, events } = await start();
	const identityReads = () =>
		phone.calls.filter((call) => call.endsWith(IDENTITY_SCRIPT)).length;
	const before = identityReads();
	await until(() => phone.streams.length === 1, "radio log stream");
	expect(phone.streams[0]?.service).toBe(`shell,v2,raw:${SIM_LOG_COMMAND}`);
	const callsBefore = phone.calls.length;

	insertSim(phone, 9);
	phone.streams[0]?.push(
		"D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_IN [PHONE1]\n",
	);
	await until(() => detail(events, "sim.secured").length === 1, "sim.secured");
	await monitor.settled();

	expect(phone.applied.slice(0, 5)).toEqual([
		...OFF_NOW,
		"svc data disable",
		"settings put global mobile_data 0",
		"settings put global mobile_data9 0",
	]);
	const sequence = phone.calls
		.slice(callsBefore)
		.map(kindOf)
		.filter((kind) => kind !== "other");
	expect(sequence.slice(0, 4)).toEqual(["apply", "isub", "apply", "read"]);
	expect(detail(events, "sim.secured")).toEqual([
		{ slot: 1, subId: 9, dataOff: true, mmsWithDataOff: false },
	]);
	expect(phone.get("global", "mobile_data9")).toBe("0");
	expect(
		monitor.alerts().find((alert) => alert.key.endsWith(":mms_with_data_off")),
	).toMatchObject({ severity: "critical" });
	await until(() => identityReads() > before, "identity refresh");
});

test("the data-off on a SIM signal does not wait for queued work", async () => {
	const { phone, monitor } = await start();
	await until(() => phone.streams.length === 1, "radio log stream");
	let release = () => {};
	// A check that is stuck on the phone holds the queue.
	phone.hold = new Promise<void>((resolve) => {
		release = resolve;
	});
	const stuck = monitor.check(B30);
	await Bun.sleep(5);
	phone.hold = null;
	insertSim(phone, 9);
	phone.streams[0]?.push(
		"D/RILJ    ( 1937): [UNSL]< UNSOL_RESPONSE_SIM_STATUS_CHANGED [PHONE1]\n",
	);
	await until(() => phone.applied.length >= 2, "immediate data off");
	expect(phone.applied).toEqual(OFF_NOW);
	release();
	await stuck;
	await monitor.settled();
});

test("a failed data-off for a new SIM raises a critical alert until data is off", async () => {
	const { phone, monitor } = await start();
	await until(() => phone.streams.length === 1, "radio log stream");
	phone.failApply = new Error("adb: device offline");
	insertSim(phone, 9);
	phone.streams[0]?.push(
		"D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_IN [PHONE1]\n",
	);
	await until(
		() => monitor.alerts().some((alert) => alert.kind === "sim_unsecured"),
		"sim_unsecured",
	);
	await until(() => monitor.phone(B30)?.check?.reason === "sim", "sim check");
	await monitor.settled();
	expect(
		monitor.alerts().find((alert) => alert.kind === "sim_unsecured"),
	).toMatchObject({ severity: "critical", key: `sim_unsecured:${B30}` });
	expect(monitor.phone(B30)?.check?.error).toBe("adb: device offline");

	phone.failApply = null;
	await monitor.check(B30);
	expect(monitor.alerts().some((alert) => alert.kind === "sim_unsecured")).toBe(
		false,
	);
	expect(phone.get("global", "mobile_data9")).toBe("0");
});

test("the SIM state poll catches a change the radio log missed", async () => {
	const { phone, events } = await start({ simPollMs: 10 });
	await until(
		() => phone.calls.some((c) => c.endsWith("gsm.sim.state")),
		"poll",
	);
	insertSim(phone, 9);
	await until(() => detail(events, "sim.secured").length === 1, "sim.secured");
});

test("a removed SIM gets only the immediate data-off and re-reads inventory", async () => {
	const { phone, monitor, events } = await start();
	await until(() => phone.streams.length === 1, "radio log stream");
	const identityReads = () =>
		phone.calls.filter((call) => call.endsWith(IDENTITY_SCRIPT)).length;
	const before = identityReads();
	phone.setSection(
		"isub",
		phone
			.section("isub")
			.split("\n")
			.filter(
				(line) => !/SubscriptionInfoInternal: id=1 .*simSlotIndex=0/.test(line),
			)
			.join("\n"),
	);
	phone.simState = "ABSENT,ABSENT";
	phone.streams[0]?.push(
		"D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_OUT [PHONE0]\n",
	);
	await until(() => identityReads() > before, "identity refresh");
	await monitor.settled();
	expect(phone.applied).toEqual(OFF_NOW);
	expect(detail(events, "sim.secured")).toEqual([]);
});

test("health: 5-minute aggregates and the offline alert", async () => {
	const { monitor, clock, db, devices, phone } = await start();
	monitor.sampleHealth();
	clock.now = new Date(clock.now.getTime() + 5 * 60_000);
	monitor.sampleHealth();
	expect(
		db
			.query<
				{ samples: number; battery_pct: number; service_json: string },
				[]
			>("SELECT samples, battery_pct, service_json FROM health_5m")
			.all(),
	).toEqual([{ samples: 1, battery_pct: 85, service_json: '{"0":0}' }]);

	phone.send([]);
	await until(() => devices.get(B30)?.status === "offline", "offline");
	monitor.sampleHealth();
	expect(monitor.alerts().some((alert) => alert.kind === "offline")).toBe(
		false,
	);
	clock.now = new Date(clock.now.getTime() + 6 * 60_000);
	monitor.sampleHealth();
	expect(monitor.alerts().map((alert) => alert.kind)).toEqual(["offline"]);
});

test("health: the hot battery alert raises above 45 °C and clears below 43 °C", async () => {
	const { monitor, devices, phone } = await start();
	const health = await fixture("nuu-s6702x", "health.txt");
	const at = async (deciC: number) => {
		phone.answer(
			HEALTH_SCRIPT,
			health.replace(/temperature: \d+/, `temperature: ${deciC}`),
		);
		devices.tick();
		await devices.settled();
		monitor.sampleHealth();
		return monitor.alerts().find((alert) => alert.kind === "battery_hot");
	};
	expect(await at(450)).toBeUndefined();
	expect((await at(468))?.message).toBe("Battery at 46.8 °C (over 45 °C).");
	expect((await at(440))?.message).toBe("Battery at 44.0 °C (over 45 °C).");
	expect(await at(429)).toBeUndefined();
});

test("health: out of service for 30 minutes alerts; three in-service samples clear it", async () => {
	const { monitor, devices, phone, clock } = await start();
	// The recorded health is from a place where the B30 has no
	// service; the in-service recording is from the same phone elsewhere.
	const outOfService = await fixture("nuu-s6702x", "health.txt");
	const inService = await fixture("nuu-s6702x", "health-in-service.txt");
	const sample = async (text: string, minutes: number) => {
		phone.answer(HEALTH_SCRIPT, text);
		devices.tick();
		await devices.settled();
		clock.now = new Date(clock.now.getTime() + minutes * 60_000);
		monitor.sampleHealth();
		return monitor.alerts().some((alert) => alert.kind === "out_of_service");
	};
	expect(await sample(outOfService, 0)).toBe(false);
	expect(await sample(outOfService, 31)).toBe(true);
	expect(await sample(inService, 1)).toBe(true);
	expect(await sample(inService, 1)).toBe(true);
	// One no-service sample restarts the count, but not the spell.
	expect(await sample(outOfService, 1)).toBe(true);
	expect(await sample(inService, 1)).toBe(true);
	expect(await sample(inService, 1)).toBe(true);
	expect(await sample(inService, 1)).toBe(false);
});

test("a SIM whose data-off failed keeps its alert while the phone is offline", async () => {
	const { phone, monitor, devices, events } = await start();
	await until(() => phone.streams.length === 1, "radio log stream");
	phone.failApply = new Error("adb: device offline");
	insertSim(phone, 9);
	phone.streams[0]?.push(
		"D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_IN [PHONE1]\n",
	);
	await until(() => monitor.phone(B30)?.check?.reason === "sim", "sim check");
	await monitor.settled();
	phone.send([]);
	await until(() => devices.get(B30)?.status === "offline", "offline");
	monitor.sampleHealth();
	expect(
		monitor.alerts().find((alert) => alert.kind === "sim_unsecured"),
	).toMatchObject({ key: `sim_unsecured:${B30}`, severity: "critical" });
	expect(
		detail(events, "alert.cleared").filter((d) => d.kind === "sim_unsecured"),
	).toEqual([]);
});

test("a SIM signal during a running data-off gets a data-off of its own", async () => {
	const { phone, monitor } = await start();
	await until(() => phone.streams.length === 1, "radio log stream");
	let release = () => {};
	phone.hold = new Promise<void>((resolve) => {
		release = resolve;
	});
	const signal = () =>
		phone.streams[0]?.push(
			"D/RILJ    ( 1937): [UNSL]< UNSOL_RESPONSE_SIM_STATUS_CHANGED [PHONE0]\n",
		);
	signal();
	await Bun.sleep(5);
	signal();
	await Bun.sleep(5);
	phone.hold = null;
	release();
	await until(() => phone.applied.length >= 4, "second data-off");
	await monitor.settled();
	expect(phone.applied).toEqual([...OFF_NOW, ...OFF_NOW]);
});

test("a check that read before a data-off failed does not clear it", async () => {
	const { phone, monitor } = await start();
	await until(() => phone.streams.length === 1, "radio log stream");
	phone.duringRead = async () => {
		phone.failApply = new Error("adb: device offline");
		phone.streams[0]?.push(
			"D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_IN [PHONE1]\n",
		);
		await until(
			() => monitor.alerts().some((alert) => alert.kind === "sim_unsecured"),
			"sim_unsecured",
		);
		phone.failApply = null;
	};
	const stale = await monitor.check(B30);
	expect(stale.results.find((r) => r.id === "mobile_data")?.ok).toBe(true);
	expect(monitor.alerts().some((alert) => alert.kind === "sim_unsecured")).toBe(
		true,
	);
	await monitor.settled();
	// A check that starts after the failure and finds data off clears it.
	await monitor.check(B30);
	expect(monitor.alerts().some((alert) => alert.kind === "sim_unsecured")).toBe(
		false,
	);
});

test("a failed data-off for data over the threshold says so", async () => {
	const { phone, monitor } = await start({ dataThresholdBytes: 1_000_000 });
	phone.setSection(
		"netstats",
		withMeteredTraffic(phone.section("netstats"), {
			bytes: 1_500_000,
			now: nowOf(phone),
		}),
	);
	phone.failApply = new Error("adb: device offline");
	await monitor.check(B30);
	expect(
		monitor.alerts().find((alert) => alert.kind === "sim_unsecured")?.message,
	).toBe(
		"Mobile data went over the threshold and turning mobile data off failed: adb: device offline",
	);
});

test("without a policy: no reads or commands, set-up refused, health alerts still raised", async () => {
	const { phone, monitor, devices, clock, readsAtConnect, atConnect } =
		await start({ policy: null });
	expect(readsAtConnect).toBe(0);
	expect(atConnect).toEqual([]);
	expect(monitor.phone(B30)?.check ?? null).toBeNull();
	expect(monitor.dataThresholdBytes).toBeNull();
	await expect(monitor.check(B30)).rejects.toThrow("no policy profile");
	await expect(monitor.provision(B30)).rejects.toThrow("no policy profile");

	// A SIM change still reaches the inventory, without any data-off.
	await until(() => phone.streams.length === 1, "radio log stream");
	const identityReads = () =>
		phone.calls.filter((call) => call.endsWith(IDENTITY_SCRIPT)).length;
	const before = identityReads();
	insertSim(phone, 9);
	phone.streams[0]?.push(
		"D/RILJ    ( 1937): [UNSL]< MTK: RIL_UNSOL_SIM_PLUG_IN [PHONE1]\n",
	);
	await until(() => identityReads() > before, "identity refresh");
	await monitor.settled();
	expect(phone.applied).toEqual([]);
	expect(phone.reads).toEqual([]);

	phone.send([]);
	await until(() => devices.get(B30)?.status !== "online", "offline");
	clock.now = new Date(clock.now.getTime() + 6 * 60_000);
	monitor.sampleHealth();
	expect(monitor.alerts().map((alert) => alert.kind)).toEqual(["offline"]);
});
