import { z } from "zod";
import { Alert, PhonePolicy } from "./monitor-schema";

// The HTTP contract. The web UI imports only the inferred types from here.

const iso = z.string().describe("ISO 8601 UTC time");

export const PhoneStatus = z
	.enum([
		"online",
		"connecting",
		"needs_auth",
		"no_permissions",
		"offline",
		"other",
	])
	.describe(
		"online: adb 'device'. needs_auth: tap Allow USB debugging on the phone. no_permissions: udev/plugdev problem on the host.",
	);

const ServiceState = z.enum([
	"in_service",
	"out_of_service",
	"emergency_only",
	"power_off",
	"unknown",
]);

export const Radio = z.object({
	slot: z.number().int(),
	voice: ServiceState,
	data: ServiceState,
	operator: z.string().nullable(),
	emergencyOnly: z.boolean(),
	signalLevel: z.number().int().min(0).max(4),
	signalType: z.string().nullable(),
	signalDbm: z.number().int().nullable(),
	dataEnabled: z.boolean().nullable(),
	dataConnected: z.boolean().nullable(),
});

export const NumberSummary = z.object({
	id: z.number().int(),
	e164: z.string().nullable(),
	source: z.enum(["detected", "manual", "none"]),
	note: z.string(),
});

const Slot = z.object({
	slot: z.number().int(),
	simState: z
		.string()
		.nullable()
		.describe("Android SIM state, such as LOADED or ABSENT"),
	radio: Radio.nullable(),
	sim: z
		.object({
			id: z.number().int(),
			iccid: z.string(),
			carrier: z.string().nullable(),
			subId: z.number().int(),
		})
		.nullable(),
	number: NumberSummary.nullable(),
});

export const Battery = z.object({
	level: z.number().int(),
	temperatureC: z.number(),
	status: z.enum([
		"unknown",
		"charging",
		"discharging",
		"not_charging",
		"full",
	]),
	plugged: z.array(z.enum(["ac", "usb", "wireless", "dock"])),
});

export const Phone = z.object({
	id: z.number().int(),
	serial: z.string(),
	site: z.string(),
	manufacturer: z.string().nullable(),
	model: z.string().nullable(),
	androidVersion: z.string().nullable(),
	imeis: z.array(z.string()),
	location: z.string().nullable(),
	hubPort: z.string().nullable(),
	costCents: z.number().int().nullable(),
	acquiredOn: z.string().nullable(),
	status: PhoneStatus,
	adbState: z
		.string()
		.describe("The raw adb state, or 'gone' when adb does not list the phone"),
	statusSince: iso.nullable(),
	firstSeenAt: iso,
	lastOnlineAt: iso.nullable(),
	battery: Battery.nullable(),
	slots: z.array(Slot),
	healthAt: iso.nullable(),
	identityAt: iso.nullable(),
	warnings: z.array(z.string()),
	probeError: z.string().nullable(),
});

export const Sim = z.object({
	id: z.number().int(),
	iccid: z.string(),
	carrier: z.string().nullable(),
	status: z.enum(["active", "spare", "retired"]),
	firstSeenAt: iso,
	lastSeenAt: iso,
	placement: z
		.object({
			phoneId: z.number().int(),
			serial: z.string(),
			slot: z.number().int(),
			subId: z.number().int(),
			since: iso,
		})
		.nullable(),
	lastPlacement: z
		.object({ serial: z.string(), slot: z.number().int(), until: iso })
		.nullable(),
	number: NumberSummary.nullable(),
});

export const PhoneNumber = NumberSummary.extend({
	detectedE164: z
		.string()
		.nullable()
		.describe("What the SIM reports, normalized"),
	confirmedBy: z.string().nullable(),
	createdAt: iso,
	sim: z
		.object({
			id: z.number().int(),
			iccid: z.string(),
			carrier: z.string().nullable(),
		})
		.nullable(),
	phone: z
		.object({
			serial: z.string(),
			model: z.string().nullable(),
			slot: z.number().int(),
			removedAt: iso
				.nullable()
				.describe("Set when the SIM is no longer in this phone"),
		})
		.nullable(),
	phoneStatus: PhoneStatus.nullable(),
	radio: Radio.nullable(),
	messageCount: z.number().int(),
	lastMessage: z
		.object({
			seq: z.number().int(),
			from: z.string().nullable(),
			preview: z.string().describe("The first 160 characters of the body"),
			receivedAt: iso,
		})
		.nullable(),
});

export const AdbStatus = z.object({
	reachable: z.boolean(),
	version: z.number().int().nullable(),
	since: iso,
	error: z.string().nullable(),
});

export const Status = z.object({
	adb: AdbStatus,
	phones: z.array(
		z.object({
			serial: z.string(),
			status: PhoneStatus,
			adbState: z.string(),
			since: iso,
			usb: z.string().nullable(),
			battery: Battery.nullable(),
			radios: z.array(Radio),
			simStates: z.array(z.string()),
			healthAt: iso.nullable(),
			identityAt: iso.nullable(),
			probeError: z.string().nullable(),
			policy: PhonePolicy.nullable(),
		}),
	),
	alerts: z.array(Alert),
	dataThresholdBytes: z.number().int().nullable(),
	profile: z
		.object({
			name: z.string(),
			setUp: z
				.array(z.string())
				.describe("What Set up phone applies, one line per rule"),
		})
		.nullable()
		.describe("The policy profile in use; null runs no phone rules"),
});

export const Health = z.object({
	ok: z.boolean().describe("The adb server is reachable"),
	adbReachable: z.boolean(),
	phonesOnline: z.number().int(),
	phonesOffline: z
		.number()
		.int()
		.describe(
			"Known phones that are not online, including ones waiting for auth",
		),
	alerts: z.number().int().describe("Open alerts"),
	version: z.string(),
});

const DeviceEvent = z.object({
	id: z.number().int(),
	phoneId: z.number().int().nullable(),
	serial: z.string().nullable(),
	at: iso,
	kind: z.string(),
	detail: z.record(z.string(), z.unknown()),
});

export const PhonePatch = z.strictObject({
	location: z.string().max(64).nullable().optional(),
	hubPort: z.string().max(64).nullable().optional(),
	costCents: z.number().int().min(0).nullable().optional(),
	acquiredOn: z.iso.date().nullable().optional(),
});

export const SimPatch = z.strictObject({
	status: z.enum(["active", "spare", "retired"]).optional(),
});

export const NumberPatch = z.strictObject({
	note: z.string().max(2000).optional(),
	e164: z
		.string()
		.max(32)
		.nullable()
		.optional()
		.describe(
			"Set the number by hand (E.164 or a 10-digit US number); null returns to the detected number",
		),
});

export const ApiError = z.object({ error: z.string() });

export type PhoneView = z.infer<typeof Phone>;
export type SlotView = z.infer<typeof Slot>;
export type NumberView = z.infer<typeof PhoneNumber>;
export type StatusView = z.infer<typeof Status>;
export type DeviceEventView = z.infer<typeof DeviceEvent>;
