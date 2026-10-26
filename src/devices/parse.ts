// Parsers for the text that Android shell tools print. Each takes the exact
// output of one command (see probe.ts) and is tested against recorded output
// from real phones in test/fixtures/device/.

const SECTION = "@@sl:";

// Splits the output of a batched probe script into its sections. Each section
// starts with a line `@@sl:<name>` that the script prints before running the
// section's command.
export function splitSections(stdout: string): Map<string, string> {
	const sections = new Map<string, string>();
	let name: string | undefined;
	let lines: string[] = [];
	const flush = () => {
		if (name !== undefined) sections.set(name, lines.join("\n").trim());
	};
	for (const line of stdout.split(/\r?\n/)) {
		if (line.startsWith(SECTION)) {
			flush();
			name = line.slice(SECTION.length).trim();
			lines = [];
		} else {
			lines.push(line);
		}
	}
	flush();
	return sections;
}

export function sectionMarker(name: string): string {
	return `printf '\\n${SECTION}${name}\\n'`;
}

// `key=value` lines, as printed by the probe's getprop loop.
export function parseKeyValues(text: string): Record<string, string> {
	const out: Record<string, string> = {};
	for (const line of text.split(/\r?\n/)) {
		const eq = line.indexOf("=");
		if (eq > 0) out[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
	}
	return out;
}

// `getprop gsm.sim.state`: one comma-separated state per slot, such as
// "LOADED,ABSENT".
export function parseSimStates(text: string): string[] {
	const value = text.trim();
	return value === "" ? [] : value.split(",").map((state) => state.trim());
}

export type Battery = {
	level: number;
	// Tenths of a degree Celsius, as the battery service reports it.
	temperatureDeciC: number;
	voltageMv: number | null;
	status: BatteryStatus;
	health: string;
	plugged: ("ac" | "usb" | "wireless" | "dock")[];
};

export type BatteryStatus =
	| "unknown"
	| "charging"
	| "discharging"
	| "not_charging"
	| "full";

const BATTERY_STATUS: Record<string, BatteryStatus> = {
	"1": "unknown",
	"2": "charging",
	"3": "discharging",
	"4": "not_charging",
	"5": "full",
};

const BATTERY_HEALTH: Record<string, string> = {
	"1": "unknown",
	"2": "good",
	"3": "overheat",
	"4": "dead",
	"5": "over_voltage",
	"6": "failure",
	"7": "cold",
};

// `dumpsys battery`.
export function parseBattery(text: string): Battery | null {
	const fields = new Map<string, string>();
	for (const line of text.split(/\r?\n/)) {
		const match = /^\s+([A-Za-z][A-Za-z ]*):\s*(.*)$/.exec(line);
		if (match?.[1] && match[2] !== undefined)
			fields.set(match[1].trim().toLowerCase(), match[2].trim());
	}
	const level = toInt(fields.get("level"));
	const scale = toInt(fields.get("scale")) ?? 100;
	const temperature = toInt(fields.get("temperature"));
	if (level === null || temperature === null) return null;
	const plugged: Battery["plugged"] = [];
	if (fields.get("ac powered") === "true") plugged.push("ac");
	if (fields.get("usb powered") === "true") plugged.push("usb");
	if (fields.get("wireless powered") === "true") plugged.push("wireless");
	if (fields.get("dock powered") === "true") plugged.push("dock");
	return {
		level: scale > 0 ? Math.round((level * 100) / scale) : level,
		temperatureDeciC: temperature,
		voltageMv: toInt(fields.get("voltage")),
		status: BATTERY_STATUS[fields.get("status") ?? ""] ?? "unknown",
		health: BATTERY_HEALTH[fields.get("health") ?? ""] ?? "unknown",
		plugged,
	};
}

export type ParcelString =
	| { kind: "value"; value: string | null }
	| { kind: "exception"; code: number; message: string | null }
	| { kind: "invalid"; reason: string };

// The reply of `service call <service> <code> ...` for a binder method that
// returns a String, such as IPhoneSubInfo's getters. The tool prints the reply
// Parcel as 32-bit little-endian words in hex:
//
//   Result: Parcel(
//   0x00000000: 00000000 0000000f 00350033 ...  '........3.5.....'
//   0x00000010: ...                              '...)
//
// or on one line for short replies: `Result: Parcel(\t00000000 ffffffff   '........')`.
// The reply is an int32 exception code (0 = none), then a String16: an int32
// length in UTF-16 code units (-1 = null) and the code units, padded to 4 bytes.
export function parseParcelString(text: string): ParcelString {
	const start = text.indexOf("Result: Parcel(");
	if (start < 0)
		return {
			kind: "invalid",
			reason: firstLine(text) || "no Parcel in output",
		};
	const words: number[] = [];
	for (const rawLine of text
		.slice(start + "Result: Parcel(".length)
		.split("\n")) {
		const line = rawLine.replace(/^\s*0x[0-9a-fA-F]+:\s*/, "").trimStart();
		const match = /^(?:[0-9a-fA-F]{8}(?: +|$))+/.exec(line);
		if (match)
			for (const word of match[0].trim().split(/ +/))
				words.push(Number.parseInt(word, 16));
		if (/'\)\s*$/.test(rawLine)) break;
	}
	const bytes = new Uint8Array(words.length * 4);
	const view = new DataView(bytes.buffer);
	words.forEach((word, index) => {
		view.setUint32(index * 4, word, true);
	});
	if (bytes.length < 4) {
		const detail = text.slice(start + "Result: Parcel(".length).trim();
		return {
			kind: "invalid",
			reason: detail.replace(/\)$/, "") || "empty Parcel",
		};
	}
	const code = view.getInt32(0, true);
	const string16 = readString16(view, 4);
	if (code !== 0)
		return {
			kind: "exception",
			code,
			message: string16.kind === "value" ? string16.value : null,
		};
	return string16;
}

function readString16(
	view: DataView,
	offset: number,
):
	| { kind: "value"; value: string | null }
	| { kind: "invalid"; reason: string } {
	if (view.byteLength < offset + 4)
		return { kind: "invalid", reason: "Parcel ends before the string length" };
	const length = view.getInt32(offset, true);
	if (length === -1) return { kind: "value", value: null };
	if (length < 0 || view.byteLength < offset + 4 + length * 2)
		return {
			kind: "invalid",
			reason: `Parcel string length ${length} exceeds the reply`,
		};
	const units: number[] = [];
	for (let i = 0; i < length; i++)
		units.push(view.getUint16(offset + 4 + i * 2, true));
	return { kind: "value", value: String.fromCharCode(...units) };
}

export type SubscriptionRecord = {
	subId: number;
	slot: number;
	// dumpsys masks the ICCID, number, and IMSI; the ICCID keeps a prefix.
	iccidPrefix: string | null;
	displayName: string | null;
	carrierName: string | null;
	mcc: string | null;
	mnc: string | null;
	countryIso: string | null;
};

export type Isub = {
	activeModemCount: number | null;
	slotSubIds: Map<number, number>;
	simStates: Map<number, string>;
	allLoaded: boolean | null;
	defaultDataSubId: number | null;
	defaultSmsSubId: number | null;
	active: SubscriptionRecord[];
};

// `dumpsys isub` on Android 14 (SubscriptionManagerService).
export function parseIsub(text: string): Isub {
	const slotSubIds = new Map<number, number>();
	const simStates = new Map<number, string>();
	const active: SubscriptionRecord[] = [];
	let section = "";
	for (const line of text.split(/\r?\n/)) {
		if (/^\S.*:\s*$/.test(line)) section = line.trim();
		const slotMatch = /Logical SIM slot (\d+): subId=(-?\d+)/.exec(line);
		if (slotMatch) slotSubIds.set(Number(slotMatch[1]), Number(slotMatch[2]));
		const stateMatch = /^mSimState\[(\d+)\]=(\S+)/.exec(line.trim());
		if (stateMatch?.[2]) simStates.set(Number(stateMatch[1]), stateMatch[2]);
		if (section === "Active subscriptions:" && line.includes("simSlotIndex=")) {
			const record = parseSubscriptionRecord(line);
			if (record) active.push(record);
		}
	}
	return {
		activeModemCount: toInt(/Active modem count=(\d+)/.exec(text)?.[1]),
		slotSubIds,
		simStates,
		allLoaded: matchBool(/areAllSubscriptionsLoaded=(\w+)/.exec(text)?.[1]),
		defaultDataSubId: toInt(/^defaultDataSubId=(-?\d+)/m.exec(text)?.[1]),
		defaultSmsSubId: toInt(/^defaultSmsSubId=(-?\d+)/m.exec(text)?.[1]),
		active,
	};
}

// One `SubscriptionInfoInternal: id=1 iccId=... carrierName=Emergency calls only ...`
// record. Values can contain spaces, so each value runs to the next ` key=`.
function parseSubscriptionRecord(line: string): SubscriptionRecord | null {
	const body = line.slice(line.indexOf("SubscriptionInfoInternal:"));
	const fields = new Map<string, string>();
	const keyPattern = /(?:^|\s)([a-zA-Z][a-zA-Z0-9]*)=/g;
	const keys = [...body.matchAll(keyPattern)];
	keys.forEach((match, index) => {
		const valueStart = (match.index ?? 0) + match[0].length;
		const valueEnd = keys[index + 1]?.index ?? body.length;
		if (match[1] && !fields.has(match[1]))
			fields.set(match[1], body.slice(valueStart, valueEnd).trim());
	});
	const subId = toInt(fields.get("id"));
	const slot = toInt(fields.get("simSlotIndex"));
	if (subId === null || slot === null || slot < 0) return null;
	const iccid = fields.get("iccId") ?? "";
	return {
		subId,
		slot,
		iccidPrefix: /^(\d+)/.exec(iccid)?.[1] ?? null,
		displayName: nonEmpty(fields.get("displayName")),
		carrierName: nonEmpty(fields.get("carrierName")),
		mcc: nonEmpty(fields.get("mcc")),
		mnc: nonEmpty(fields.get("mnc")),
		countryIso: nonEmpty(fields.get("countryIso")),
	};
}

export type ServiceState =
	| "in_service"
	| "out_of_service"
	| "emergency_only"
	| "power_off"
	| "unknown";

export type RadioState = {
	slot: number;
	voice: ServiceState;
	data: ServiceState;
	operator: string | null;
	emergencyOnly: boolean;
	// SignalStrength's level, 0 (none) to 4 (great), from its primary RAT.
	signalLevel: number;
	signalType: string | null;
	// rsrp for LTE, ssRsrp for NR, rscp for WCDMA, rssi for GSM.
	signalDbm: number | null;
	dataEnabled: boolean | null;
	dataConnected: boolean | null;
};

const SERVICE_STATE: Record<string, ServiceState> = {
	"0": "in_service",
	"1": "out_of_service",
	"2": "emergency_only",
	"3": "power_off",
};

// `dumpsys telephony.registry`, reduced by the probe to the `Phone Id=` headers
// and the m*= lines it reads. Phone Id is the logical slot.
export function parseRegistry(text: string): RadioState[] {
	const states: RadioState[] = [];
	let current: Record<string, string> | undefined;
	let slot = -1;
	const finish = () => {
		if (current) states.push(radioState(slot, current));
	};
	for (const line of text.split(/\r?\n/)) {
		const header = /^\s*Phone Id=(\d+)\s*$/.exec(line);
		if (header) {
			finish();
			slot = Number(header[1]);
			current = {};
			continue;
		}
		const field = /^\s+(m[A-Za-z]+)=(.*)$/.exec(line);
		if (
			current &&
			field?.[1] &&
			field[2] !== undefined &&
			!(field[1] in current)
		)
			current[field[1]] = field[2];
	}
	finish();
	return states;
}

function radioState(slot: number, fields: Record<string, string>): RadioState {
	const service = fields.mServiceState ?? "";
	const signal = fields.mSignalStrength ?? "";
	const operator =
		nonNull(/mOperatorAlphaLong=([^,]*)/.exec(service)?.[1]) ??
		nonNull(/mOperatorAlphaShort=([^,]*)/.exec(service)?.[1]);
	const { level, type, dbm } = primarySignal(signal);
	const connection = toInt(fields.mDataConnectionState);
	return {
		slot,
		voice:
			SERVICE_STATE[/mVoiceRegState=(\d+)/.exec(service)?.[1] ?? ""] ??
			"unknown",
		data:
			SERVICE_STATE[/mDataRegState=(\d+)/.exec(service)?.[1] ?? ""] ??
			"unknown",
		operator,
		emergencyOnly: /mIsEmergencyOnly=true/.test(service),
		signalLevel: level,
		signalType: type,
		signalDbm: dbm,
		dataEnabled: matchBool(fields.mIsDataEnabled),
		// TelephonyManager.DATA_CONNECTED is 2; -1 means unknown (no SIM).
		dataConnected:
			connection === null || connection < 0 ? null : connection === 2,
	};
}

const SIGNAL_DBM_FIELD: Record<string, RegExp> = {
	Lte: /\brsrp=(-?\d+)/,
	Nr: /\bssRsrp = (-?\d+)/,
	Wcdma: /\brscp=(-?\d+)/,
	Tdscdma: /\brscp=(-?\d+)/,
	Gsm: /\brssi=(-?\d+)/,
	Cdma: /\bcdmaDbm=(-?\d+)/,
};

// Values Android prints when a field is unavailable (CellInfo.UNAVAILABLE).
const UNAVAILABLE = 2147483647;

function primarySignal(signal: string): {
	level: number;
	type: string | null;
	dbm: number | null;
} {
	const type = /primary=CellSignalStrength(\w+)/.exec(signal)?.[1] ?? null;
	if (!type) return { level: 0, type: null, dbm: null };
	const start = signal.indexOf(`m${type}=CellSignalStrength${type}`);
	if (start < 0) return { level: 0, type, dbm: null };
	const rest = signal.slice(start);
	const end = rest.search(/,m[A-Z][a-z]+=CellSignalStrength|,primary=/);
	const segment = end < 0 ? rest : rest.slice(0, end);
	const level = toInt(/\b(?:mLevel|level) ?= ?(\d+)/.exec(segment)?.[1]) ?? 0;
	const pattern = SIGNAL_DBM_FIELD[type];
	const raw = pattern ? toInt(pattern.exec(segment)?.[1]) : null;
	const dbm = raw === null || Math.abs(raw) === UNAVAILABLE ? null : raw;
	// A slot without a SIM still names a primary RAT, with every value unset.
	if (level === 0 && dbm === null) return { level, type: null, dbm };
	return { level, type: type.toUpperCase(), dbm };
}

function toInt(value: string | undefined | null): number | null {
	if (value === undefined || value === null || !/^-?\d+$/.test(value.trim()))
		return null;
	return Number(value.trim());
}

function matchBool(value: string | undefined): boolean | null {
	if (value === "true") return true;
	if (value === "false") return false;
	return null;
}

function nonEmpty(value: string | undefined): string | null {
	return value === undefined || value === "" ? null : value;
}

function nonNull(value: string | undefined): string | null {
	const trimmed = value?.trim();
	return !trimmed || trimmed === "null" ? null : trimmed;
}

function firstLine(text: string): string {
	return text.trim().split("\n")[0]?.trim() ?? "";
}
