// The phone policy as data, built from a profile (profiles/*.json). The
// drift checker and Provision both run one policy's rules, so they cannot
// disagree. Every rule reads through one batched, read-only shell call (the
// policy's readScript); enforce commands only ever turn radios and data off.
import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { z } from "zod";
import {
	parseIsub,
	parseRegistry,
	sectionMarker,
	splitSections,
} from "../devices/parse";

export const SMS_ROLE = "android.app.role.SMS";

// Names shown for SMS apps a profile commonly picks.
const APP_NAMES: Record<string, string> = {
	"com.google.android.apps.messaging": "Google Messages",
};

const PackageName = z
	.string()
	.regex(/^[A-Za-z][\w]*(\.[A-Za-z_][\w]*)+$/, "an Android package name");

// Every group is optional; a profile turns on only what it names.
export const Profile = z
	.object({
		// Mobile data, roaming, Wi-Fi and its scanning and wake-up, Bluetooth
		// and its scanning, and location stay off. It also turns data off when
		// a SIM changes, writes the next SIMs' data keys ahead of time, and
		// watches mobile data use.
		radiosOff: z
			.object({
				// Internet mobile bytes per SIM in 24 h that turn data off again
				// and raise an alert.
				dataThresholdBytes: z.number().int().min(0).default(1_000_000),
			})
			.strict()
			.optional(),
		dataSaver: z.boolean().default(false),
		// The package that must be the default SMS app.
		smsApp: PackageName.optional(),
		// Apps removed for user 0 by Set up phone. `pm uninstall --user 0`
		// deletes the app's data for user 0; `cmd package install-existing
		// <pkg>` brings the app back without it.
		removeApps: z.array(PackageName).default([]),
		screen: z
			.object({
				autoBrightness: z.boolean().optional(),
				brightness: z.number().int().min(0).max(255).optional(),
				timeoutSeconds: z.number().int().min(1).max(86_400).optional(),
				stayOnWhileCharging: z.boolean().optional(),
			})
			.strict()
			.default({}),
		disableSystemUpdates: z.boolean().default(false),
		// The radio log buffer the drift checker keeps at least this large
		// (logcat -G). Some phones ignore Developer options' "Logger buffer
		// sizes" at boot (measured on the NUU B30: 256 KiB after every reboot).
		radioLogBufferMiB: z.number().int().min(1).max(64).optional(),
		// "MMS messages" stays on with mobile data off, on every SIM. adb
		// cannot set it, so it is only checked.
		mmsWithDataOff: z.boolean().default(false),
	})
	.strict();

export type Profile = z.infer<typeof Profile>;

type Namespace = "global" | "secure" | "system";
const NAMESPACES: Namespace[] = ["global", "secure", "system"];

export type ActiveSub = {
	subId: number;
	slot: number;
	// Telephony's enabledMobileDataPolicies; 2 is MMS_ALWAYS_ALLOWED ("MMS
	// messages" with data off). Null when isub does not print the field.
	dataPolicies: number[] | null;
};

// What one read returned, parsed. Null means the read printed nothing
// usable.
export type Snapshot = {
	settings: Record<Namespace, Map<string, string>>;
	subs: ActiveSub[];
	// Every subscription id isub lists, including SIMs no longer inserted.
	knownSubIds: number[];
	// Telephony's own view of mobile data, by slot.
	dataEnabled: { slot: number; enabled: boolean | null }[];
	dataSaver: boolean | null;
	smsRoleHolders: string[] | null;
	// Apps the profile removes that are still installed for user 0.
	bloat: string[];
	// The radio log buffer's size in effect, not the persist.logd.size
	// setting: on the B30 the setting said 8M while the buffer was 256 KiB.
	radioBufferBytes: number | null;
	// The phone's clock (epoch seconds), which netstats buckets use.
	phoneClock: number | null;
	netstats: string;
};

export const MMS_ALWAYS_ALLOWED = 2;

export function parseSnapshot(
	stdout: string,
	removeApps: readonly string[],
): Snapshot {
	const sections = splitSections(stdout);
	const get = (name: string) => sections.get(name) ?? "";
	const settings = {} as Record<Namespace, Map<string, string>>;
	for (const ns of NAMESPACES) {
		const map = new Map<string, string>();
		for (const line of get(`settings:${ns}`).split(/\r?\n/)) {
			const eq = line.indexOf("=");
			if (eq > 0) map.set(line.slice(0, eq), line.slice(eq + 1).trim());
		}
		settings[ns] = map;
	}
	const isubText = get("isub");
	const subs = parseIsub(isubText).active.map((sub) => ({
		subId: sub.subId,
		slot: sub.slot,
		dataPolicies: dataPolicies(isubText, sub.subId),
	}));
	const dataSaver = /status:\s*(enabled|disabled)/.exec(get("datasaver"))?.[1];
	const clock = get("clock").trim();
	const roles = get("smsrole").trim();
	return {
		settings,
		subs,
		knownSubIds: [
			...isubText.matchAll(/SubscriptionInfoInternal: id=(\d+) /g),
		].map((match) => Number(match[1])),
		dataEnabled: parseRegistry(get("registry")).map((radio) => ({
			slot: radio.slot,
			enabled: radio.dataEnabled,
		})),
		dataSaver: dataSaver === undefined ? null : dataSaver === "enabled",
		smsRoleHolders: sections.has("smsrole")
			? roles.split(/[\s;]+/).filter(Boolean)
			: null,
		bloat: get("bloat")
			.split(/\r?\n/)
			.map((line) => line.trim().replace(/^package:/, ""))
			.filter((name) => removeApps.includes(name)),
		radioBufferBytes: bufferBytes(get("radiobuf")),
		phoneClock: /^\d+$/.test(clock) ? Number(clock) : null,
		netstats: get("netstats"),
	};
}

function dataPolicies(isub: string, subId: number): number[] | null {
	const line = isub
		.split(/\r?\n/)
		.find((candidate) =>
			candidate.includes(`SubscriptionInfoInternal: id=${subId} `),
		);
	const match = line && /enabledMobileDataPolicies=([\d,]*)/.exec(line);
	if (!match) return null;
	return (match[1] ?? "").split(",").filter(Boolean).map(Number);
}

export type Severity = "critical" | "warning" | "info";

export type CheckContext = {
	// scrcpy holds stay_on_while_plugged_in while it streams.
	screenSession: boolean;
};

// `instruction` replaces the rule's own when this outcome needs other words.
export type Outcome = { ok: boolean; observed: string; instruction?: string };

export type Rule = {
	id: string;
	describe: string;
	want: string;
	severity: Severity;
	// Radio and data rules, and the log buffer: the drift checker sets them
	// on sight. Other failing rules raise an alert and wait for Provision.
	autoFix: boolean;
	// Keeps the phone off mobile data and radios. One still failing after
	// the automatic fix is a critical alert.
	safety?: boolean;
	// The event an automatic fix logs; "drift" unless the fix is routine.
	fixEvent?: string;
	// Settings the rule reads (grep -E alternatives). Per-SIM keys carry the
	// subscription id as a suffix, such as mobile_data1.
	settings?: { ns: Namespace; key: string }[];
	check(snapshot: Snapshot, context: CheckContext): Outcome;
	// Commands that make the rule pass; idempotent. Absent when adb cannot
	// set it.
	enforce?(snapshot: Snapshot, context: CheckContext): string[];
	// What the owner does on the phone when adb cannot set it, or what
	// happens next when the automatic fix did not take.
	instruction?: string;
};

// A value of null means "not set", which for the per-SIM data and roaming
// keys is not off: with ro.com.android.mobiledata unset (measured on the
// B30) a new SIM's data defaults to on.
function settingValue(
	snapshot: Snapshot,
	ns: Namespace,
	key: string,
): string | null {
	return snapshot.settings[ns].get(key) ?? null;
}

function show(value: string | null): string {
	return value ?? "unset";
}

function settingRule(input: {
	id: string;
	describe: string;
	ns: Namespace;
	key: string;
	want: string;
	severity: Severity;
	autoFix: boolean;
	safety?: boolean;
	// A command that sets it better than `settings put`, such as svc.
	command?: string;
	acceptDuringScreenSession?: boolean;
}): Rule {
	return {
		id: input.id,
		describe: input.describe,
		want: `${input.ns} ${input.key} = ${input.want}`,
		severity: input.severity,
		autoFix: input.autoFix,
		safety: input.safety,
		settings: [{ ns: input.ns, key: input.key }],
		check(snapshot, context) {
			const value = settingValue(snapshot, input.ns, input.key);
			if (input.acceptDuringScreenSession && context.screenSession)
				return { ok: true, observed: `${show(value)} (screen session)` };
			return { ok: value === input.want, observed: show(value) };
		},
		enforce(_snapshot, context) {
			if (input.acceptDuringScreenSession && context.screenSession) return [];
			return [
				input.command ?? `settings put ${input.ns} ${input.key} ${input.want}`,
			];
		},
	};
}

// A global key that exists once plus once per SIM (mobile_data,
// mobile_data1, ...). Every copy for an active SIM must be 0.
function perSimOffRule(input: {
	id: string;
	describe: string;
	key: string;
	extra?: (snapshot: Snapshot) => string[];
	first: string[];
}): Rule {
	const keys = (snapshot: Snapshot) => [
		input.key,
		...snapshot.subs.map((sub) => `${input.key}${sub.subId}`),
	];
	return {
		id: input.id,
		describe: input.describe,
		want: `global ${input.key} and ${input.key}<subId> = 0`,
		severity: "critical",
		autoFix: true,
		safety: true,
		settings: [{ ns: "global", key: `${input.key}[0-9]*` }],
		check(snapshot) {
			const values = keys(snapshot).map(
				(key) => [key, settingValue(snapshot, "global", key)] as const,
			);
			const extra = input.extra?.(snapshot) ?? [];
			return {
				ok: values.every(([, value]) => value === "0") && extra.length === 0,
				observed: [
					...values.map(([key, value]) => `${key}=${show(value)}`),
					...extra,
				].join(", "),
			};
		},
		enforce(snapshot) {
			return [
				...input.first,
				...keys(snapshot).map((key) => `settings put global ${key} 0`),
			];
		},
	};
}

function radioRules(): Rule[] {
	return [
		perSimOffRule({
			id: "mobile_data",
			describe: "Mobile data off, on every SIM",
			key: "mobile_data",
			first: ["svc data disable"],
			// Telephony's own flag, in case a setting and the data stack disagree.
			extra: (snapshot) =>
				snapshot.dataEnabled
					.filter((slot) => slot.enabled === true)
					.map((slot) => `slot ${slot.slot} data enabled`),
		}),
		perSimOffRule({
			id: "data_roaming",
			describe: "Data roaming off, on every SIM",
			key: "data_roaming",
			first: [],
		}),
		settingRule({
			id: "wifi_off",
			describe: "Wi-Fi off",
			ns: "global",
			key: "wifi_on",
			want: "0",
			severity: "critical",
			autoFix: true,
			safety: true,
			command: "svc wifi disable",
		}),
		settingRule({
			id: "wifi_wakeup_off",
			describe: "Wi-Fi does not turn itself back on",
			ns: "global",
			key: "wifi_wakeup_enabled",
			want: "0",
			severity: "critical",
			autoFix: true,
			safety: true,
		}),
		settingRule({
			id: "wifi_scan_off",
			describe: "Wi-Fi scanning off",
			ns: "global",
			key: "wifi_scan_always_enabled",
			want: "0",
			severity: "critical",
			autoFix: true,
			safety: true,
		}),
		settingRule({
			id: "ble_scan_off",
			describe: "Bluetooth scanning off",
			ns: "global",
			key: "ble_scan_always_enabled",
			want: "0",
			severity: "critical",
			autoFix: true,
			safety: true,
		}),
		{
			...settingRule({
				id: "bluetooth_off",
				describe: "Bluetooth off",
				ns: "global",
				key: "bluetooth_on",
				want: "0",
				severity: "critical",
				autoFix: true,
				safety: true,
			}),
			// The adapter turns off asynchronously; wait so the re-check sees it.
			enforce: () => [
				"cmd bluetooth_manager disable",
				"cmd bluetooth_manager wait-for-state:STATE_OFF",
			],
		},
		settingRule({
			id: "location_off",
			describe: "Location off",
			ns: "secure",
			key: "location_mode",
			want: "0",
			severity: "critical",
			autoFix: true,
			safety: true,
		}),
	];
}

function screenRules(screen: Profile["screen"]): Rule[] {
	const rules: Rule[] = [];
	if (screen.stayOnWhileCharging !== undefined)
		rules.push(
			settingRule({
				id: "stay_on_off",
				describe: screen.stayOnWhileCharging
					? "Screen stays on while charging"
					: "Screen does not stay on while charging",
				ns: "global",
				key: "stay_on_while_plugged_in",
				// 7: on AC, USB, and wireless power.
				want: screen.stayOnWhileCharging ? "7" : "0",
				severity: "info",
				autoFix: false,
				acceptDuringScreenSession: true,
			}),
		);
	if (screen.autoBrightness !== undefined)
		rules.push(
			settingRule({
				id: "brightness_manual",
				describe: screen.autoBrightness
					? "Automatic brightness"
					: "Manual brightness",
				ns: "system",
				key: "screen_brightness_mode",
				want: screen.autoBrightness ? "1" : "0",
				severity: "info",
				autoFix: false,
			}),
		);
	if (screen.brightness !== undefined)
		rules.push(
			settingRule({
				id: "brightness_low",
				describe: `Brightness ${screen.brightness} of 255`,
				ns: "system",
				key: "screen_brightness",
				want: String(screen.brightness),
				severity: "info",
				autoFix: false,
			}),
		);
	if (screen.timeoutSeconds !== undefined)
		rules.push(
			settingRule({
				id: "screen_timeout",
				describe: `Screen turns off after ${screen.timeoutSeconds} s`,
				ns: "system",
				key: "screen_off_timeout",
				want: String(screen.timeoutSeconds * 1000),
				severity: "info",
				autoFix: false,
			}),
		);
	return rules;
}

function dataSaverRule(): Rule {
	return {
		id: "data_saver",
		describe: "Data Saver on",
		want: "restrict-background enabled",
		severity: "warning",
		autoFix: false,
		check: (snapshot) => ({
			ok: snapshot.dataSaver === true,
			observed:
				snapshot.dataSaver === null
					? "unreadable"
					: snapshot.dataSaver
						? "enabled"
						: "disabled",
		}),
		enforce: () => ["cmd netpolicy set restrict-background true"],
	};
}

// The SMS app is deliberately not put on the Data Saver allowlist. Messages
// downloads MMS through the platform MmsService in the phone process, a
// system uid Data Saver does not restrict (inferred), and allowlisting would
// only let it use data if data came on.
function smsAppRule(app: string): Rule {
	return {
		id: "sms_role",
		describe: `${APP_NAMES[app] ?? app} is the default SMS app`,
		want: app,
		severity: "critical",
		autoFix: false,
		check: (snapshot) => ({
			ok:
				snapshot.smsRoleHolders?.length === 1 &&
				snapshot.smsRoleHolders[0] === app,
			observed: snapshot.smsRoleHolders?.join(", ") || "none",
		}),
		enforce: () => [`cmd role add-role-holder ${SMS_ROLE} ${app}`],
	};
}

function removeAppsRule(apps: readonly string[]): Rule {
	return {
		id: "debloat",
		describe: "Unneeded apps removed",
		want: `none of the ${apps.length} listed apps installed for user 0`,
		severity: "warning",
		autoFix: false,
		check: (snapshot) => ({
			ok: snapshot.bloat.length === 0,
			observed:
				snapshot.bloat.length === 0
					? "none installed"
					: `installed: ${snapshot.bloat.join(", ")}`,
		}),
		// Only names from the profile: parseSnapshot drops anything else.
		enforce: (snapshot) =>
			snapshot.bloat.map((name) => `pm uninstall --user 0 ${name}`),
	};
}

function mmsRule(): Rule {
	return {
		id: "mms_with_data_off",
		describe: "MMS works with mobile data off, on every SIM",
		want: "enabledMobileDataPolicies includes 2 (MMS_ALWAYS_ALLOWED)",
		severity: "critical",
		autoFix: false,
		instruction:
			"On the phone: Settings > Network & internet > SIMs > (the SIM) > turn on MMS messages.",
		check(snapshot) {
			if (snapshot.subs.length === 0) return { ok: true, observed: "no SIM" };
			const states = snapshot.subs.map((sub) => ({
				sub,
				on: sub.dataPolicies?.includes(MMS_ALWAYS_ALLOWED) ?? false,
			}));
			return {
				ok: states.every((state) => state.on),
				observed: states
					.map(
						(state) =>
							`SIM ${state.sub.slot + 1} (sub ${state.sub.subId}) ${state.on ? "on" : "off"}`,
					)
					.join(", "),
			};
		},
	};
}

export function logBufferCommand(mib: number): string {
	return `logcat -b main,system,radio,crash -G ${mib}M`;
}

// `logcat -G` resizes the buffers at once until the next reboot (measured on
// the B30), so the drift checker sets it after every boot.
function logBufferRule(mib: number): Rule {
	return {
		id: "log_buffer",
		describe: `Radio log buffer at least ${mib} MiB`,
		want: `logcat -g -b radio reports a ring buffer of ${mib} MiB or more`,
		severity: "info",
		autoFix: true,
		// Expected after every reboot on some phones, so not "Setting reverted".
		fixEvent: "log_buffer_set",
		instruction:
			"The phone ignored the buffer size; it will be retried on the next check.",
		enforce: () => [logBufferCommand(mib)],
		check: (snapshot) =>
			snapshot.radioBufferBytes === null
				? {
						ok: false,
						observed: "unreadable",
						instruction:
							"Couldn't read the buffer size; it will be retried on the next check.",
					}
				: {
						ok: snapshot.radioBufferBytes >= mib * 1024 * 1024,
						observed: formatBytes(snapshot.radioBufferBytes),
					},
	};
}

export type Policy = {
	// The profile's file name without .json.
	name: string;
	rules: readonly Rule[];
	// Every read the rules and the data watch need, in one shell call.
	readScript: string;
	// The section the read prints last; its absence means a cut-short read.
	lastSection: string;
	// Namespaces the read lists; an empty one means the settings tool failed.
	namespaces: Namespace[];
	// Radios and data off: data-off on SIM changes, the next SIMs' data keys,
	// and the data watch with its threshold. Null when the profile leaves
	// radios alone.
	radiosOff: { dataThresholdBytes: number } | null;
	parse(stdout: string): Snapshot;
};

export function createPolicy(name: string, profile: Profile): Policy {
	const rules: Rule[] = [];
	if (profile.radiosOff) rules.push(...radioRules());
	if (profile.disableSystemUpdates)
		rules.push(
			settingRule({
				id: "ota_off",
				describe: "Automatic system updates off",
				ns: "global",
				key: "ota_disable_automatic_update",
				want: "1",
				severity: "warning",
				autoFix: false,
			}),
		);
	rules.push(...screenRules(profile.screen));
	if (profile.dataSaver) rules.push(dataSaverRule());
	if (profile.smsApp) rules.push(smsAppRule(profile.smsApp));
	if (profile.removeApps.length > 0)
		rules.push(removeAppsRule(profile.removeApps));
	if (profile.mmsWithDataOff) rules.push(mmsRule());
	if (profile.radioLogBufferMiB)
		rules.push(logBufferRule(profile.radioLogBufferMiB));

	// grep trims the large outputs on the phone so only the lines read cross
	// USB.
	const keys = new Map<Namespace, string[]>();
	for (const rule of rules)
		for (const { ns, key } of rule.settings ?? [])
			keys.set(ns, [...(keys.get(ns) ?? []), key]);
	const namespaces = NAMESPACES.filter((ns) => keys.has(ns));
	const reads: [string, string][] = [
		...namespaces.map((ns): [string, string] => [
			`settings:${ns}`,
			`settings list ${ns} | grep -E '^(${keys.get(ns)?.join("|")})='`,
		]),
		["isub", "dumpsys isub"],
		[
			"registry",
			"dumpsys telephony.registry | grep -E '^  Phone Id=|^    mIsDataEnabled='",
		],
	];
	if (profile.dataSaver)
		reads.push(["datasaver", "cmd netpolicy get restrict-background"]);
	if (profile.smsApp)
		reads.push(["smsrole", `cmd role get-role-holders ${SMS_ROLE}`]);
	if (profile.removeApps.length > 0)
		reads.push([
			"bloat",
			`pm list packages --user 0 | grep -xF ${profile.removeApps.map((app) => `-e package:${app}`).join(" ")}`,
		]);
	if (profile.radioLogBufferMiB) reads.push(["radiobuf", "logcat -g -b radio"]);
	if (profile.radiosOff)
		reads.push(
			["clock", "date +%s"],
			["netstats", "dumpsys netstats --full --uid"],
		);
	return {
		name,
		rules,
		readScript: reads
			.map(([section, command]) => `${sectionMarker(section)}; ${command}`)
			.join("; "),
		lastSection: reads.at(-1)?.[0] ?? "registry",
		namespaces,
		radiosOff: profile.radiosOff
			? { dataThresholdBytes: profile.radiosOff.dataThresholdBytes }
			: null,
		parse: (stdout) => parseSnapshot(stdout, profile.removeApps),
	};
}

// Reads and checks a profile file; throws with every problem named.
export function loadPolicy(path: string): Policy {
	let json: unknown;
	try {
		json = JSON.parse(readFileSync(path, "utf8"));
	} catch (error) {
		throw new Error(
			`cannot read policy profile ${path}: ${error instanceof Error ? error.message : String(error)}`,
		);
	}
	const result = Profile.safeParse(json);
	if (!result.success)
		throw new Error(
			`invalid policy profile ${path}:\n${z.prettifyError(result.error)}`,
		);
	return createPolicy(basename(path, ".json"), result.data);
}

export type RuleResult = Omit<Outcome, "instruction"> & {
	id: string;
	describe: string;
	want: string;
	severity: Severity;
	autoFix: boolean;
	instruction: string | null;
	fixable: boolean;
};

export function evaluate(
	snapshot: Snapshot,
	context: CheckContext,
	rules: readonly Rule[],
): RuleResult[] {
	return rules.map((rule) => {
		const { instruction, ...outcome } = rule.check(snapshot, context);
		return {
			id: rule.id,
			describe: rule.describe,
			want: rule.want,
			severity: rule.severity,
			autoFix: rule.autoFix,
			instruction: instruction ?? rule.instruction ?? null,
			fixable: rule.enforce !== undefined,
			...outcome,
		};
	});
}

// How many subscription ids past the highest one isub knows get
// mobile_data<subId> 0 written ahead of time.
export const PRESET_SUB_IDS = 4;

// Guess: Android hands out subscription ids in sequence, so the next SIMs
// this phone sees get the next ids. Writing their per-SIM data key as 0
// ahead of time means a new SIM starts with data off even before the SIM
// watch reacts. A key for an id that is never used is harmless.
export function presetDataOffCommands(snapshot: Snapshot): string[] {
	const highest = Math.max(0, ...snapshot.knownSubIds);
	const commands: string[] = [];
	for (let subId = highest + 1; subId <= highest + PRESET_SUB_IDS; subId++)
		if (snapshot.settings.global.get(`mobile_data${subId}`) !== "0")
			commands.push(`settings put global mobile_data${subId} 0`);
	return commands;
}

// The commands that would make the failing rules pass. `which` picks the
// rules: all of them for Provision, the radio and data ones for drift.
export function plan(
	snapshot: Snapshot,
	context: CheckContext,
	which: (rule: Rule) => boolean,
	rules: readonly Rule[],
): { rule: string; commands: string[] }[] {
	return rules
		.filter((rule) => which(rule) && rule.enforce)
		.filter((rule) => !rule.check(snapshot, context).ok)
		.map((rule) => ({
			rule: rule.id,
			commands: rule.enforce?.(snapshot, context) ?? [],
		}))
		.filter((step) => step.commands.length > 0);
}

// Settings that must never be set to anything but off (or, for airplane
// mode, never written at all).
const RADIO_KEYS =
	/\bsettings\s+put\s+(global|secure)\s+(mobile_data\d*|data_roaming\d*|wifi_on|wifi_wakeup_enabled|wifi_scan_always_enabled|ble_scan_always_enabled|bluetooth_on|location_mode)\s+(\S+)/g;

const ENABLING: [RegExp, string][] = [
	[/\bsvc\s+(wifi|data|bluetooth|nfc)\s+enable\b/, "svc ... enable"],
	[/\bcmd\s+bluetooth_manager\s+enable\b/, "bluetooth_manager enable"],
	[/\bcmd\s+wifi\s+set-wifi-enabled\s+enabled\b/, "cmd wifi enable"],
	[/\bcmd\s+phone\s+data\s+enable\b/, "cmd phone data enable"],
	[/\bcmd\s+location\s+set-location-enabled\s+true\b/, "location enable"],
	[/\bairplane_mode_on\b|\bairplane-mode\b/, "airplane mode change"],
	[/\brestrict-background\s+false\b/, "Data Saver off"],
];

// Why `command` could turn a radio or data on, or null when it cannot. Every
// command the monitor runs on a phone passes through this first, whatever
// the profile.
export function radioHazard(command: string): string | null {
	for (const [pattern, reason] of ENABLING)
		if (pattern.test(command)) return reason;
	for (const put of command.matchAll(RADIO_KEYS))
		if (put[3] !== "0") return `${put[2]} set to ${put[3]}`;
	return null;
}

const BUFFER_UNITS: Record<string, number> = {
	B: 1,
	KiB: 1024,
	MiB: 1024 * 1024,
	GiB: 1024 * 1024 * 1024,
};

// `logcat -g -b radio`: "radio: ring buffer is 256 KiB (252 KiB consumed,
// 1 MiB readable), max entry is 5120 B, ...".
function bufferBytes(text: string): number | null {
	const match = /ring buffer is ([\d.]+) ?(B|KiB|MiB|GiB)\b/.exec(text);
	const unit = match?.[2] && BUFFER_UNITS[match[2]];
	return match?.[1] && unit ? Math.round(Number(match[1]) * unit) : null;
}

function formatBytes(bytes: number): string {
	for (const [unit, size] of Object.entries(BUFFER_UNITS).reverse())
		if (bytes >= size && bytes % size === 0) return `${bytes / size} ${unit}`;
	return `${bytes} B`;
}
