// The phone policy as data. The drift checker and Provision both run this
// one list, so they cannot disagree. Every rule reads through one batched,
// read-only shell call (READ_SCRIPT); enforce commands only ever turn radios
// and data off.
import {
	parseIsub,
	parseRegistry,
	sectionMarker,
	splitSections,
} from "../devices/parse";

export const MESSAGES = "com.google.android.apps.messaging";
export const SMS_ROLE = "android.app.role.SMS";

// Apps removed for user 0.
// `pm uninstall --user 0` deletes the app's data for user 0;
// `cmd package install-existing <pkg>` brings the app back without it.
export const REMOVE = [
	"com.android.chrome",
	"com.android.vending",
	"com.google.android.youtube",
	"com.google.android.apps.youtube.music",
	"com.google.android.videos",
	"com.google.android.apps.maps",
	"com.google.android.apps.photos",
	"com.google.android.gm",
	"com.google.android.apps.docs",
	"com.google.android.keep",
	"com.google.android.calendar",
	"com.google.android.apps.tachyon",
	"com.google.android.apps.googleassistant",
	"com.google.android.googlequicksearchbox",
	"com.google.android.apps.walletnfcrel",
	"com.google.android.apps.subscriptions.red",
	"com.google.android.projection.gearhead",
	"com.google.android.apps.nbu.files",
	"com.google.android.apps.adm",
	"com.google.android.apps.safetyhub",
	"com.google.android.apps.wellbeing",
	"com.google.android.apps.restore",
	"com.google.android.feedback",
	"com.google.android.apps.turbo",
	"com.google.android.gms.location.history",
	"com.google.android.calculator",
	"com.google.android.deskclock",
	"com.google.android.dialer",
	"com.google.android.contacts",
	"com.google.android.apps.carrier.carrierwifi",
	"com.google.ambient.streaming",
	"com.google.android.partnersetup",
	"com.google.android.printservice.recommendation",
	"com.android.musicfx",
	"com.nuumobile.livesupport",
	"com.nuumobile.nuualerts",
	"android.autoinstalls.config.nuumobile.s6702x",
] as const;

// The radio log buffer the SIM watch and later diagnosis need; anything
// from 4 MiB up holds hours of radio traffic. The B30's logd ignores
// persist.logd.size (Developer options' "Logger buffer sizes") at boot and
// starts every buffer at 256 KiB (measured: setting 8M, buffers 256 KiB a
// day after a reboot), but `logcat -G` resizes them at once until the next
// reboot (measured), so the drift checker sets it after every boot.
export const MIN_RADIO_BUFFER_BYTES = 4 * 1024 * 1024;
export const LOG_BUFFER_COMMAND = "logcat -b main,system,radio,crash -G 8M";

type Namespace = "global" | "secure" | "system";

// Settings each namespace's read keeps (grep -E alternatives). Per-SIM keys
// carry the subscription id as a suffix, such as mobile_data1.
const SETTING_KEYS: Record<Namespace, string[]> = {
	global: [
		"mobile_data[0-9]*",
		"data_roaming[0-9]*",
		"wifi_on",
		"wifi_wakeup_enabled",
		"wifi_scan_always_enabled",
		"ble_scan_always_enabled",
		"bluetooth_on",
		"ota_disable_automatic_update",
		"stay_on_while_plugged_in",
	],
	secure: ["location_mode"],
	system: ["screen_brightness_mode", "screen_brightness", "screen_off_timeout"],
};

// Every read the policy and the data watch need, in one shell call. grep
// trims the large outputs on the phone so only the lines read cross USB.
const READS: [string, string][] = [
	...(Object.keys(SETTING_KEYS) as Namespace[]).map((ns): [string, string] => [
		`settings:${ns}`,
		`settings list ${ns} | grep -E '^(${SETTING_KEYS[ns].join("|")})='`,
	]),
	["isub", "dumpsys isub"],
	[
		"registry",
		"dumpsys telephony.registry | grep -E '^  Phone Id=|^    mIsDataEnabled='",
	],
	["datasaver", "cmd netpolicy get restrict-background"],
	["smsrole", `cmd role get-role-holders ${SMS_ROLE}`],
	[
		"bloat",
		`pm list packages --user 0 | grep -xF ${REMOVE.map((name) => `-e package:${name}`).join(" ")}`,
	],
	["radiobuf", "logcat -g -b radio"],
	["clock", "date +%s"],
	["netstats", "dumpsys netstats --full --uid"],
];

export const READ_SCRIPT = READS.map(
	([name, command]) => `${sectionMarker(name)}; ${command}`,
).join("; ");

export type ActiveSub = {
	subId: number;
	slot: number;
	// Telephony's enabledMobileDataPolicies; 2 is MMS_ALWAYS_ALLOWED ("MMS
	// messages" with data off). Null when isub does not print the field.
	dataPolicies: number[] | null;
};

// What one READ_SCRIPT run returned, parsed. Null means the read printed
// nothing usable.
export type Snapshot = {
	settings: Record<Namespace, Map<string, string>>;
	subs: ActiveSub[];
	// Every subscription id isub lists, including SIMs no longer inserted.
	knownSubIds: number[];
	// Telephony's own view of mobile data, by slot.
	dataEnabled: { slot: number; enabled: boolean | null }[];
	dataSaver: boolean | null;
	smsRoleHolders: string[] | null;
	// REMOVE packages still installed for user 0.
	bloat: string[];
	// The radio log buffer's size in effect, not the persist.logd.size
	// setting: on the B30 the setting said 8M while the buffer was 256 KiB.
	radioBufferBytes: number | null;
	// The phone's clock (epoch seconds), which netstats buckets use.
	phoneClock: number | null;
	netstats: string;
};

export const MMS_ALWAYS_ALLOWED = 2;

export function parseSnapshot(stdout: string): Snapshot {
	const sections = splitSections(stdout);
	const get = (name: string) => sections.get(name) ?? "";
	const settings = {} as Record<Namespace, Map<string, string>>;
	for (const ns of Object.keys(SETTING_KEYS) as Namespace[]) {
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
			.filter((name) => (REMOVE as readonly string[]).includes(name)),
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

export const RULES: readonly Rule[] = [
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
	settingRule({
		id: "ota_off",
		describe: "Automatic system updates off",
		ns: "global",
		key: "ota_disable_automatic_update",
		want: "1",
		severity: "warning",
		autoFix: false,
	}),
	settingRule({
		id: "stay_on_off",
		describe: "Screen does not stay on while charging",
		ns: "global",
		key: "stay_on_while_plugged_in",
		want: "0",
		severity: "info",
		autoFix: false,
		acceptDuringScreenSession: true,
	}),
	settingRule({
		id: "brightness_manual",
		describe: "Manual brightness",
		ns: "system",
		key: "screen_brightness_mode",
		want: "0",
		severity: "info",
		autoFix: false,
	}),
	settingRule({
		id: "brightness_low",
		describe: "Brightness 10 of 255",
		ns: "system",
		key: "screen_brightness",
		want: "10",
		severity: "info",
		autoFix: false,
	}),
	settingRule({
		id: "screen_timeout",
		describe: "Screen turns off after 15 s",
		ns: "system",
		key: "screen_off_timeout",
		want: "15000",
		severity: "info",
		autoFix: false,
	}),
	{
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
	},
	// Messages is deliberately not on the Data Saver allowlist (decided
	// 2026-10-22). It downloads MMS through the platform MmsService
	// in the phone process, a system uid Data Saver does not restrict
	// (inferred), and allowlisting would only let Messages and RCS use data
	// if data came on. Bring the rule back if an MMS download fails on a live
	// test with data off and Data Saver on.
	{
		id: "sms_role",
		describe: "Google Messages is the default SMS app",
		want: MESSAGES,
		severity: "critical",
		autoFix: false,
		check: (snapshot) => ({
			ok:
				snapshot.smsRoleHolders?.length === 1 &&
				snapshot.smsRoleHolders[0] === MESSAGES,
			observed: snapshot.smsRoleHolders?.join(", ") || "none",
		}),
		enforce: () => [`cmd role add-role-holder ${SMS_ROLE} ${MESSAGES}`],
	},
	{
		id: "debloat",
		describe: "Unneeded apps removed",
		want: `none of the ${REMOVE.length} listed apps installed for user 0`,
		severity: "warning",
		autoFix: false,
		check: (snapshot) => ({
			ok: snapshot.bloat.length === 0,
			observed:
				snapshot.bloat.length === 0
					? "none installed"
					: `installed: ${snapshot.bloat.join(", ")}`,
		}),
		// Only names from REMOVE: parseSnapshot drops anything else.
		enforce: (snapshot) =>
			snapshot.bloat.map((name) => `pm uninstall --user 0 ${name}`),
	},
	{
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
	},
	{
		id: "log_buffer",
		describe: "Radio log buffer at least 4 MiB",
		want: "logcat -g -b radio reports a ring buffer of 4 MiB or more",
		severity: "info",
		autoFix: true,
		// Expected after every reboot of the B30, so not "Setting reverted".
		fixEvent: "log_buffer_set",
		instruction:
			"The phone ignored the buffer size; it will be retried on the next check.",
		enforce: () => [LOG_BUFFER_COMMAND],
		check: (snapshot) =>
			snapshot.radioBufferBytes === null
				? {
						ok: false,
						observed: "unreadable",
						instruction:
							"Couldn't read the buffer size; it will be retried on the next check.",
					}
				: {
						ok: snapshot.radioBufferBytes >= MIN_RADIO_BUFFER_BYTES,
						observed: formatBytes(snapshot.radioBufferBytes),
					},
	},
];

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
	rules: readonly Rule[] = RULES,
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
	rules: readonly Rule[] = RULES,
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
// command the monitor runs on a phone passes through this first.
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
