// Parsers for `content query` and `dumpsys isms` output on the phone.

export type ContentRow = Record<string, string | null>;

export type ContentRows = {
	rows: ContentRow[];
	// Row lines that did not match the projection, kept for the log.
	malformed: string[];
};

// Parses `content query` output: one `Row: N col=value, col=value` line per
// row, `NULL` for SQL null, and "No result found." when empty. The tool does
// not escape values, so a value containing ", " would be ambiguous; each value
// therefore runs up to the next expected `, <column>=` of the projection, in
// order. Text that can hold arbitrary characters (bodies, file names) is
// selected as hex(...), which leaves only digits and A-F.
export function parseContentRows(
	output: string,
	columns: readonly string[],
): ContentRows {
	const lines: string[] = [];
	// The output's final newline ends the last row; any other line that does
	// not start a row, even an empty one, continues a value with newlines.
	const text = output.replace(/\r\n/g, "\n").replace(/\n$/, "");
	for (const line of text.split("\n")) {
		if (/^Row: \d+ /.test(line)) lines.push(line);
		else if (lines.length > 0) lines[lines.length - 1] += `\n${line}`;
	}
	const rows: ContentRow[] = [];
	const malformed: string[] = [];
	for (const line of lines) {
		const row = parseRow(line.replace(/^Row: \d+ /, ""), columns);
		if (row) rows.push(row);
		else malformed.push(line);
	}
	return { rows, malformed };
}

function parseRow(text: string, columns: readonly string[]): ContentRow | null {
	const row: ContentRow = {};
	let position = 0;
	for (let i = 0; i < columns.length; i++) {
		const column = columns[i] as string;
		const key = `${column}=`;
		if (!text.startsWith(key, position)) return null;
		const start = position + key.length;
		const next = columns[i + 1];
		let end = text.length;
		if (next !== undefined) {
			end = text.indexOf(`, ${next}=`, start);
			if (end < 0) return null;
		}
		const value = text.slice(start, end);
		row[column] = value === "NULL" ? null : value;
		position = end + 2;
	}
	return row;
}

// SQLite's hex() of a TEXT value is its UTF-8 bytes; hex(NULL) is empty.
export function decodeHex(hex: string | null): string {
	if (!hex) return "";
	if (!/^(?:[0-9A-Fa-f]{2})*$/.test(hex))
		throw new Error("hex text has an odd length or a non-hex character");
	return new TextDecoder().decode(Buffer.from(hex, "hex"));
}

export function toInteger(value: string | null | undefined): number | null {
	return value != null && /^-?\d+$/.test(value) ? Number(value) : null;
}

// What `dumpsys isms` shows of inbound SMS handling. Any change between two
// reads means the phone handled an inbound SMS (or a WAP push for an MMS)
// since the first one, so the providers are worth querying.
export type IsmsActivity = {
	// "total records=N" of each InboundSmsHandler state machine; it grows with
	// every message the handler processes.
	handlerRecords: number[];
	// The newest EVENT_NEW_SMS or EVENT_BROADCAST_SMS record time, as printed
	// ("10-21 19:50:27.212": no year, phone clock).
	lastEventAt: string | null;
	// "Delivering SMS to: ..." lines in the dispatcher log, and the newest one.
	deliveries: number;
	lastDeliveryAt: string | null;
};

// Keeps only the lines parseIsms reads; full `dumpsys isms` is about 15 KB.
export const ISMS_TRIGGER_SCRIPT =
	"dumpsys isms | grep -E 'InboundSmsHandler:$|total records=|what=EVENT_(NEW|BROADCAST)_SMS|Delivering SMS to'";

export function parseIsms(output: string): IsmsActivity {
	const activity: IsmsActivity = {
		handlerRecords: [],
		lastEventAt: null,
		deliveries: 0,
		lastDeliveryAt: null,
	};
	let inHandler = false;
	for (const line of output.replace(/\r\n/g, "\n").split("\n")) {
		const trimmed = line.trim();
		if (/InboundSmsHandler:$/.test(trimmed)) {
			inHandler = true;
			continue;
		}
		const records = /^total records=(\d+)$/.exec(trimmed);
		if (records) {
			if (inHandler) activity.handlerRecords.push(Number(records[1]));
			inHandler = false;
			continue;
		}
		const event =
			/^rec\[\d+\]: time=(\S+ \S+) .*what=EVENT_(?:NEW|BROADCAST)_SMS\b/.exec(
				trimmed,
			);
		if (event?.[1]) {
			if (activity.lastEventAt === null || event[1] > activity.lastEventAt)
				activity.lastEventAt = event[1];
			continue;
		}
		const delivery = /^(\S+) - Delivering SMS to: /.exec(trimmed);
		if (delivery?.[1]) {
			activity.deliveries++;
			if (
				activity.lastDeliveryAt === null ||
				delivery[1] > activity.lastDeliveryAt
			)
				activity.lastDeliveryAt = delivery[1];
		}
	}
	return activity;
}

export function activitySignature(activity: IsmsActivity): string {
	return JSON.stringify(activity);
}
