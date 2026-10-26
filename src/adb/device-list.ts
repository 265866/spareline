// The connection states the adb server reports for a transport, normalized.
// "no permissions" arrives with a long help text and is reduced to its name.
const DEVICE_STATES = [
	"device",
	"unauthorized",
	"offline",
	"no_permissions",
	"connecting",
	"authorizing",
	"bootloader",
	"recovery",
	"rescue",
	"sideload",
	"host",
	"detached",
	"unknown",
] as const;
export type DeviceState = (typeof DEVICE_STATES)[number];

export type DeviceEntry = {
	serial: string;
	state: DeviceState;
	// The state text exactly as adb printed it, kept for "no permissions"
	// and states this list does not name.
	rawState: string;
	usb?: string;
	product?: string;
	model?: string;
	device?: string;
	transportId?: number;
};

const INFO_KEYS = [
	"usb",
	"product",
	"model",
	"device",
	"transport_id",
] as const;

// Parses one `host:devices-l` / `host:track-devices-l` payload. adb prints each
// transport as `%-22s %s` (serial, state) followed by optional ` key:value`
// fields and always ends with ` transport_id:N`, so the fields are taken from
// the end of the line and whatever precedes them is the state.
export function parseDeviceList(payload: string): DeviceEntry[] {
	const entries: DeviceEntry[] = [];
	for (const line of payload.split("\n")) {
		const trimmed = line.trimEnd();
		if (trimmed === "") continue;
		const match = /^(\S+)\s+(.*)$/.exec(trimmed);
		if (!match?.[1] || match[2] === undefined) continue;
		const serial = match[1];
		const words = match[2].split(/\s+/);
		const fields: Partial<Record<(typeof INFO_KEYS)[number], string>> = {};
		while (words.length > 1) {
			const last = words[words.length - 1] ?? "";
			const colon = last.indexOf(":");
			const key = last.slice(0, colon) as (typeof INFO_KEYS)[number];
			if (colon <= 0 || !INFO_KEYS.includes(key)) break;
			// usb:1-1 keeps its prefix in adb's own output; store just the path.
			fields[key] = last.slice(colon + 1);
			words.pop();
		}
		const rawState = words.join(" ");
		const transportId =
			fields.transport_id === undefined
				? undefined
				: Number(fields.transport_id);
		entries.push({
			serial,
			state: normalizeState(rawState),
			rawState,
			...(fields.usb !== undefined && { usb: fields.usb }),
			...(fields.product !== undefined && { product: fields.product }),
			...(fields.model !== undefined && { model: fields.model }),
			...(fields.device !== undefined && { device: fields.device }),
			...(transportId !== undefined &&
				Number.isSafeInteger(transportId) && { transportId }),
		});
	}
	return entries;
}

function normalizeState(raw: string): DeviceState {
	if (raw.startsWith("no permissions")) return "no_permissions";
	return (DEVICE_STATES as readonly string[]).includes(raw)
		? (raw as DeviceState)
		: "unknown";
}
