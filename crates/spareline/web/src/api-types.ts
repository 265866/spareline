// The JSON the server's HTTP API, event stream and screen socket send, as
// the UI reads it. Times are ISO 8601 UTC strings.

export type PhoneStatus =
	| "online"
	| "connecting"
	| "needs_auth"
	| "no_permissions"
	| "offline"
	| "other";

export type ServiceState =
	| "in_service"
	| "out_of_service"
	| "emergency_only"
	| "power_off"
	| "unknown";

export type RadioView = {
	slot: number;
	voice: ServiceState;
	data: ServiceState;
	operator: string | null;
	emergencyOnly: boolean;
	// 0 to 4.
	signalLevel: number;
	signalType: string | null;
	signalDbm: number | null;
	dataEnabled: boolean | null;
	dataConnected: boolean | null;
};

export type NumberSummaryView = {
	id: number;
	e164: string | null;
	source: "detected" | "manual" | "none";
	note: string;
};

export type SlotView = {
	slot: number;
	// Android's SIM state, such as LOADED or ABSENT.
	simState: string | null;
	radio: RadioView | null;
	sim: {
		id: number;
		iccid: string;
		carrier: string | null;
		subId: number;
	} | null;
	number: NumberSummaryView | null;
};

export type BatteryView = {
	level: number;
	temperatureC: number;
	status: "unknown" | "charging" | "discharging" | "not_charging" | "full";
	plugged: ("ac" | "usb" | "wireless" | "dock")[];
};

export type PhoneView = {
	id: number;
	serial: string;
	site: string;
	manufacturer: string | null;
	model: string | null;
	androidVersion: string | null;
	imeis: string[];
	location: string | null;
	hubPort: string | null;
	costCents: number | null;
	acquiredOn: string | null;
	status: PhoneStatus;
	// The raw adb state, or "gone" when adb does not list the phone.
	adbState: string;
	statusSince: string | null;
	firstSeenAt: string;
	lastOnlineAt: string | null;
	battery: BatteryView | null;
	slots: SlotView[];
	healthAt: string | null;
	identityAt: string | null;
	warnings: string[];
	probeError: string | null;
};

export type NumberView = NumberSummaryView & {
	// What the SIM reports, normalized.
	detectedE164: string | null;
	confirmedBy: string | null;
	createdAt: string;
	sim: { id: number; iccid: string; carrier: string | null } | null;
	phone: {
		serial: string;
		model: string | null;
		slot: number;
		// Set when the SIM is no longer in this phone.
		removedAt: string | null;
	} | null;
	phoneStatus: PhoneStatus | null;
	radio: RadioView | null;
	messageCount: number;
	lastMessage: {
		seq: number;
		from: string | null;
		// The first 160 characters of the body.
		preview: string;
		receivedAt: string;
	} | null;
};

export type Severity = "critical" | "warning" | "info";

export type RuleResultView = {
	id: string;
	describe: string;
	want: string;
	severity: Severity;
	// The drift checker applies it on sight: the radio and data rules, and
	// the radio log buffer.
	autoFix: boolean;
	instruction: string | null;
	// Set up can apply it over adb.
	fixable: boolean;
	ok: boolean;
	observed: string;
};

export type SimUsageView = {
	subId: number;
	slot: number | null;
	rx: number;
	tx: number;
	over: boolean;
	uids: { uid: number; bytes: number }[];
};

export type CheckReportView = {
	at: string;
	reason: "connect" | "interval" | "manual" | "sim";
	readMs: number;
	results: RuleResultView[];
	usage: SimUsageView[];
	reverted: { rule: string; before: string; after: string; fixed: boolean }[];
	error: string | null;
};

export type ProvisionReportView = {
	at: string;
	durationMs: number;
	steps: {
		rule: string;
		commands: string[];
		failed: { command: string; output: string }[];
	}[];
	results: RuleResultView[];
	ok: boolean;
	error: string | null;
};

export type PhonePolicyView = {
	busy: "check" | "provision" | "sim" | null;
	check: CheckReportView | null;
	provision: ProvisionReportView | null;
	dataAlarm: boolean;
};

export type AlertView = {
	key: string;
	kind:
		| "offline"
		| "no_permissions"
		| "out_of_service"
		| "battery_hot"
		| "policy"
		| "drift_unfixed"
		| "data_threshold"
		| "check_failed"
		| "sim_unsecured";
	serial: string;
	severity: Severity;
	message: string;
	since: string;
};

export type StatusView = {
	adb: {
		reachable: boolean;
		version: number | null;
		since: string;
		error: string | null;
	};
	phones: {
		serial: string;
		status: PhoneStatus;
		adbState: string;
		since: string;
		usb: string | null;
		battery: BatteryView | null;
		radios: RadioView[];
		simStates: string[];
		healthAt: string | null;
		identityAt: string | null;
		probeError: string | null;
		policy: PhonePolicyView | null;
	}[];
	alerts: AlertView[];
	dataThresholdBytes: number | null;
	// The policy profile in use; null runs no phone rules.
	profile: {
		name: string;
		// What Set up applies, one line per rule.
		setUp: string[];
		// Set up uninstalls apps, which deletes their data.
		removesApps: boolean;
	} | null;
};

export type DeviceEventView = {
	id: number;
	phoneId: number | null;
	serial: string | null;
	at: string;
	kind: string;
	detail: Record<string, unknown>;
};

export type AttachmentView = {
	id: number;
	contentType: string;
	// Bytes.
	size: number;
	filename: string | null;
	// The path to GET the bytes from.
	url: string;
};

export type MessageView = {
	// One increasing cursor across all numbers and phones.
	seq: number;
	provider: "sms" | "mms";
	providerId: number;
	// The serial of the phone that received it.
	phone: string;
	subId: number | null;
	simId: number | null;
	// Null while the SIM is not known yet.
	number: { id: number; e164: string | null; note: string } | null;
	from: string | null;
	body: string;
	subject: string | null;
	sentAt: string | null;
	receivedAt: string;
	ingestedAt: string;
	attachments: AttachmentView[];
};

export type MessagePageView = {
	messages: MessageView[];
	cursor: number;
};

export type ApiKeyView = {
	id: number;
	user: string;
	name: string;
	scope: "read" | "full";
	// The key's first characters, to tell keys apart.
	prefix: string;
	createdAt: string;
	lastUsedAt: string | null;
	revokedAt: string | null;
};

// The screen socket's text frames. Binary frames are scrcpy video packets.
export type ScreenMessage =
	| { t: "hello"; width: number; height: number }
	| { t: "status"; state: "starting" | "restarting" }
	| { t: "stats"; fps: number; kbps: number };
