import type { CallOptions, ShellResult } from "../adb/client";
import {
	isIccid,
	isImei,
	type SubInfoCodes,
	subInfoCall,
	subInfoCodes,
	toE164,
} from "./identifiers";
import {
	type Battery,
	type Isub,
	parseBattery,
	parseIsub,
	parseKeyValues,
	parseParcelString,
	parseRegistry,
	parseSimStates,
	type RadioState,
	sectionMarker,
	splitSections,
} from "./parse";

// The one capability probes need from a phone. AdbClient implements it; tests
// replay recorded output through a stand-in.
export type DeviceShell = {
	shell(
		serial: string,
		command: string,
		options?: CallOptions,
	): Promise<ShellResult>;
};

const PROPS = [
	"ro.product.manufacturer",
	"ro.product.model",
	"ro.build.version.release",
	"ro.build.version.sdk",
	"ro.build.fingerprint",
	"ro.build.version.security_patch",
	"persist.radio.multisim.config",
	"gsm.sim.state",
] as const;

// What changes only when the phone or its SIMs change. Two shell calls: the
// subscriber getters need the SDK (for their binder codes) and the sub ids
// that the first call reports.
export const IDENTITY_SCRIPT = [
	sectionMarker("props"),
	`for p in ${PROPS.join(" ")}; do echo "$p=$(getprop $p)"; done`,
	sectionMarker("isub"),
	"dumpsys isub",
].join("; ");

export function subscriberScript(
	codes: SubInfoCodes,
	slots: number[],
	subIds: number[],
): string {
	const parts: string[] = [];
	for (const slot of slots)
		parts.push(
			sectionMarker(`imei:${slot}`),
			subInfoCall(codes.imeiForSlot, slot),
		);
	for (const subId of subIds)
		parts.push(
			sectionMarker(`iccid:${subId}`),
			subInfoCall(codes.iccidForSub, subId),
			sectionMarker(`number:${subId}`),
			subInfoCall(codes.numberForSub, subId),
		);
	return parts.join("; ");
}

// Cheap, run every minute: battery, SIM card states, and per-slot service and
// signal. telephony.registry is about 400 KB on the B30, almost all of it
// history, so only the lines read here cross USB.
export const HEALTH_SCRIPT = [
	sectionMarker("battery"),
	"dumpsys battery",
	sectionMarker("simstate"),
	"getprop gsm.sim.state",
	sectionMarker("registry"),
	"dumpsys telephony.registry | grep -E '^  Phone Id=|^    m(ServiceState|SignalStrength|IsDataEnabled|DataConnectionState|RadioPowerState)='",
].join("; ");

export type SlotState = {
	slot: number;
	// "present": a loaded SIM with a subscription; "empty": no card;
	// "unknown": anything else (booting, PIN locked, card error), which
	// inventory leaves untouched.
	state: "present" | "empty" | "unknown";
	simState: string | null;
};

export type SimIdentity = {
	slot: number;
	subId: number;
	iccid: string | null;
	iccidPrefix: string | null;
	carrier: string | null;
	countryIso: string | null;
	mcc: string | null;
	mnc: string | null;
	// E.164 when the SIM reports a number that can be normalized.
	number: string | null;
};

export type PhoneIdentity = {
	manufacturer: string | null;
	model: string | null;
	androidVersion: string | null;
	sdk: number | null;
	fingerprint: string | null;
	securityPatch: string | null;
	// By slot; null when unreadable.
	imeis: (string | null)[];
	slots: SlotState[];
	sims: SimIdentity[];
	defaultDataSubId: number | null;
	defaultSmsSubId: number | null;
	subInfoVerified: boolean;
	// Why a field is missing, for the UI and the log.
	warnings: string[];
};

export type PhoneHealth = {
	battery: Battery | null;
	simStates: string[];
	radios: RadioState[];
};

export async function probeIdentity(
	shell: DeviceShell,
	serial: string,
	options: CallOptions = {},
): Promise<PhoneIdentity> {
	const first = await run(shell, serial, IDENTITY_SCRIPT, options);
	const props = parseKeyValues(first.get("props") ?? "");
	const isub = parseIsub(first.get("isub") ?? "");
	const sdk = toNumber(props["ro.build.version.sdk"]);
	const fingerprint = nonEmpty(props["ro.build.fingerprint"]);
	const warnings: string[] = [];
	const slotCount = slotCountOf(props, isub);
	const slots = slotStates(
		slotCount,
		isub,
		parseSimStates(props["gsm.sim.state"] ?? ""),
	);
	const subs = isub.active.filter((sub) =>
		slots.some((slot) => slot.slot === sub.slot && slot.state === "present"),
	);

	const lookup = subInfoCodes({ sdk, fingerprint });
	const imeis: (string | null)[] = Array.from(
		{ length: slotCount },
		() => null,
	);
	const sims: SimIdentity[] = subs.map((sub) => ({
		slot: sub.slot,
		subId: sub.subId,
		iccid: null,
		iccidPrefix: sub.iccidPrefix,
		carrier: sub.displayName,
		countryIso: sub.countryIso,
		mcc: sub.mcc,
		mnc: sub.mnc,
		number: null,
	}));
	if (lookup.codes) {
		if (!lookup.verified)
			warnings.push(
				`iphonesubinfo codes for SDK ${sdk} are unverified on this build`,
			);
		const second = await run(
			shell,
			serial,
			subscriberScript(
				lookup.codes,
				imeis.map((_, slot) => slot),
				sims.map((sim) => sim.subId),
			),
			options,
		);
		imeis.forEach((_, slot) => {
			imeis[slot] = identifier(
				second.get(`imei:${slot}`),
				isImei,
				`IMEI of slot ${slot}`,
				warnings,
			);
		});
		for (const sim of sims) {
			sim.iccid = identifier(
				second.get(`iccid:${sim.subId}`),
				isIccid,
				`ICCID of sub ${sim.subId}`,
				warnings,
			);
			const raw = identifier(
				second.get(`number:${sim.subId}`),
				(value) => value.trim() !== "",
				`number of sub ${sim.subId}`,
				warnings,
			);
			sim.number = raw === null ? null : toE164(raw, sim.countryIso);
			if (raw !== null && sim.number === null)
				warnings.push(
					`number of sub ${sim.subId} is not a recognizable phone number`,
				);
		}
	} else {
		warnings.push(lookup.reason);
	}
	return {
		manufacturer: nonEmpty(props["ro.product.manufacturer"]),
		model: nonEmpty(props["ro.product.model"]),
		androidVersion: nonEmpty(props["ro.build.version.release"]),
		sdk,
		fingerprint,
		securityPatch: nonEmpty(props["ro.build.version.security_patch"]),
		imeis,
		slots,
		sims,
		defaultDataSubId: isub.defaultDataSubId,
		defaultSmsSubId: isub.defaultSmsSubId,
		subInfoVerified: lookup.codes !== null && lookup.verified,
		warnings,
	};
}

export async function probeHealth(
	shell: DeviceShell,
	serial: string,
	options: CallOptions = {},
): Promise<PhoneHealth> {
	const sections = await run(shell, serial, HEALTH_SCRIPT, options);
	return {
		battery: parseBattery(sections.get("battery") ?? ""),
		simStates: parseSimStates(sections.get("simstate") ?? ""),
		radios: parseRegistry(sections.get("registry") ?? ""),
	};
}

async function run(
	shell: DeviceShell,
	serial: string,
	script: string,
	options: CallOptions,
): Promise<Map<string, string>> {
	const result = await shell.shell(serial, script, options);
	// grep exits 1 when it matches nothing, which is not a probe failure; the
	// sections it would fill are simply empty.
	if (result.exitCode > 1)
		throw new Error(
			`probe script exited ${result.exitCode}: ${new TextDecoder().decode(result.stderr).trim()}`,
		);
	return splitSections(new TextDecoder().decode(result.stdout));
}

function identifier(
	output: string | undefined,
	valid: (value: string) => boolean,
	what: string,
	warnings: string[],
): string | null {
	if (output === undefined) {
		warnings.push(`${what}: no output`);
		return null;
	}
	const parsed = parseParcelString(output);
	if (parsed.kind === "invalid") {
		warnings.push(`${what}: ${parsed.reason}`);
		return null;
	}
	if (parsed.kind === "exception") {
		warnings.push(
			`${what}: binder exception ${parsed.code} ${parsed.message ?? ""}`.trim(),
		);
		return null;
	}
	if (parsed.value === null || parsed.value === "") return null;
	if (!valid(parsed.value)) {
		warnings.push(`${what}: unexpected value shape, ignored`);
		return null;
	}
	return parsed.value;
}

function slotCountOf(props: Record<string, string>, isub: Isub): number {
	if (isub.activeModemCount !== null && isub.activeModemCount > 0)
		return isub.activeModemCount;
	const config = props["persist.radio.multisim.config"];
	if (config === "dsds" || config === "dsda") return 2;
	if (config === "tsts") return 3;
	return Math.max(1, parseSimStates(props["gsm.sim.state"] ?? "").length);
}

function slotStates(
	count: number,
	isub: Isub,
	propStates: string[],
): SlotState[] {
	return Array.from({ length: count }, (_, slot) => {
		const simState = isub.simStates.get(slot) ?? propStates[slot] ?? null;
		const hasSub = isub.active.some((sub) => sub.slot === slot);
		let state: SlotState["state"] = "unknown";
		if (simState === "ABSENT") state = "empty";
		else if (simState === "LOADED" && hasSub && isub.allLoaded !== false)
			state = "present";
		return { slot, state, simState };
	});
}

function nonEmpty(value: string | undefined): string | null {
	return value === undefined || value.trim() === "" ? null : value.trim();
}

function toNumber(value: string | undefined): number | null {
	return value !== undefined && /^\d+$/.test(value) ? Number(value) : null;
}
