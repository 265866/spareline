import {
	type ContentRow,
	type ContentRows,
	decodeHex,
	parseContentRows,
	toInteger,
} from "./parse";

// Reading the phone's SMS and MMS providers with the `content` tool. Each
// `content` call starts app_process on the phone (about 1.1 s on the B30), so
// callers batch what they can.

export type Provider = "sms" | "mms";

// One inbound message as the phone's provider holds it, ready to store.
export type ProviderMessage = {
	provider: Provider;
	providerId: number;
	subId: number | null;
	sender: string | null;
	body: string;
	subject: string | null;
	// ISO 8601 UTC.
	sentAt: string | null;
	receivedAt: string;
	// Binary MMS parts still to be read from the phone.
	parts: PartRef[];
};

export type PartRef = {
	partId: number;
	contentType: string;
	filename: string | null;
};

// The provider _id and date of every inbox row, for reconciling.
export type InboxKey = { providerId: number; receivedAt: string };

// hex(body) goes last: see parseContentRows.
export const SMS_COLUMNS = [
	"_id",
	"thread_id",
	"address",
	"date",
	"date_sent",
	"sub_id",
	"read",
	"type",
	"hex(body)",
] as const;

const KEY_COLUMNS = ["_id", "date"] as const;

// MMS rows with m_type 132 (retrieve-conf) are downloaded messages. A
// notification (130) stays in the inbox until the download finishes and has
// no parts yet, so it is skipped until then.
const MMS_RETRIEVED = "m_type=132";

export const MMS_COLUMNS = [
	"_id",
	"date",
	"date_sent",
	"sub_id",
	"m_type",
	"hex(sub)",
] as const;

export const MMS_ADDR_COLUMNS = ["type", "address"] as const;

export const MMS_PART_COLUMNS = [
	"_id",
	"mid",
	"seq",
	"ct",
	"hex(name)",
	"hex(fn)",
	"hex(cl)",
	"hex(text)",
] as const;

// PduHeaders.FROM and PduHeaders.TO in AOSP.
const ADDR_FROM = "137";

function query(
	uri: string,
	columns: readonly string[],
	where: string | null,
): string {
	const projection = columns
		.map((column) => (column.includes("(") ? `'${column}'` : column))
		.join(":");
	const parts = [`content query --uri ${uri} --projection ${projection}`];
	if (where) parts.push(`--where '${where}'`);
	parts.push("--sort '_id ASC'");
	return parts.join(" ");
}

export const commands = {
	smsAfter: (afterId: number) =>
		query("content://sms/inbox", SMS_COLUMNS, `_id>${afterId}`),
	smsByIds: (ids: number[]) =>
		query("content://sms/inbox", SMS_COLUMNS, `_id IN (${ids.join(",")})`),
	smsKeys: () => query("content://sms/inbox", KEY_COLUMNS, null),
	mmsAfter: (afterId: number) =>
		query(
			"content://mms/inbox",
			MMS_COLUMNS,
			`${MMS_RETRIEVED} AND _id>${afterId}`,
		),
	mmsByIds: (ids: number[]) =>
		query(
			"content://mms/inbox",
			MMS_COLUMNS,
			`${MMS_RETRIEVED} AND _id IN (${ids.join(",")})`,
		),
	mmsKeys: () => query("content://mms/inbox", KEY_COLUMNS, MMS_RETRIEVED),
	mmsAddr: (id: number) =>
		`content query --uri content://mms/${id}/addr --projection ${MMS_ADDR_COLUMNS.join(":")}`,
	mmsParts: (ids: number[]) =>
		query("content://mms/part", MMS_PART_COLUMNS, `mid IN (${ids.join(",")})`),
	// Run with Devices.shell: binary stdout, with stderr and the exit status
	// apart (exec: would mix an error message into the bytes).
	readPart: (partId: number) =>
		`content read --uri content://mms/part/${partId}`,
} as const;

function iso(ms: number | null): string | null {
	return ms === null || ms <= 0 ? null : new Date(ms).toISOString();
}

function required(row: ContentRow, column: string): number {
	const value = toInteger(row[column]);
	if (value === null)
		throw new Error(`provider row has no integer ${column}: ${row[column]}`);
	return value;
}

// SMS dates are milliseconds since the epoch.
export function smsMessage(row: ContentRow): ProviderMessage {
	const receivedAt = iso(required(row, "date"));
	if (!receivedAt) throw new Error("SMS row has no date");
	return {
		provider: "sms",
		providerId: required(row, "_id"),
		subId: toInteger(row.sub_id),
		sender: row.address ?? null,
		body: decodeHex(row["hex(body)"] ?? null),
		subject: null,
		sentAt: iso(toInteger(row.date_sent)),
		receivedAt,
		parts: [],
	};
}

export function inboxKeys(rows: ContentRow[], provider: Provider): InboxKey[] {
	const scale = provider === "mms" ? 1000 : 1;
	// A row without an integer _id and date cannot be fetched or stored.
	return rows.flatMap((row) => {
		const providerId = toInteger(row._id);
		const date = toInteger(row.date);
		const receivedAt = date === null ? null : iso(date * scale);
		return providerId === null || receivedAt === null
			? []
			: [{ providerId, receivedAt }];
	});
}

export type MmsPartRow = {
	partId: number;
	mid: number;
	seq: number;
	contentType: string;
	filename: string | null;
	text: string;
};

export function mmsPart(row: ContentRow): MmsPartRow {
	const name = decodeHex(row["hex(name)"] ?? null);
	const fn = decodeHex(row["hex(fn)"] ?? null);
	const cl = decodeHex(row["hex(cl)"] ?? null);
	return {
		partId: required(row, "_id"),
		mid: required(row, "mid"),
		seq: toInteger(row.seq) ?? 0,
		contentType: (row.ct ?? "application/octet-stream").toLowerCase(),
		filename: name || fn || cl || null,
		text: decodeHex(row["hex(text)"] ?? null),
	};
}

// MMS dates are seconds since the epoch. Text parts become the body; SMIL
// (the slide layout) is dropped; every other part is an attachment.
export function mmsMessage(
	row: ContentRow,
	addresses: ContentRow[],
	parts: MmsPartRow[],
): ProviderMessage {
	const date = required(row, "date");
	const receivedAt = iso(date * 1000);
	if (!receivedAt) throw new Error("MMS row has no date");
	const sentSeconds = toInteger(row.date_sent);
	const subject = decodeHex(row["hex(sub)"] ?? null) || null;
	const ordered = [...parts].sort(
		(a, b) => a.seq - b.seq || a.partId - b.partId,
	);
	const text = ordered
		.filter((part) => part.contentType === "text/plain")
		.map((part) => part.text)
		.filter((value) => value !== "");
	return {
		provider: "mms",
		providerId: required(row, "_id"),
		subId: toInteger(row.sub_id),
		sender:
			addresses.find((address) => address.type === ADDR_FROM)?.address ?? null,
		body: text.join("\n"),
		subject,
		sentAt: iso(sentSeconds === null ? null : sentSeconds * 1000),
		receivedAt,
		parts: ordered
			.filter(
				(part) =>
					part.contentType !== "text/plain" &&
					part.contentType !== "application/smil",
			)
			.map((part) => ({
				partId: part.partId,
				contentType: part.contentType,
				filename: part.filename,
			})),
	};
}

// `content` reports provider errors on stderr, sometimes with exit code 0.
export function checkContentOutput(
	result: { stdout: string; stderr: string; exitCode: number },
	columns: readonly string[],
): ContentRows {
	const stderr = result.stderr.trim();
	if (result.exitCode !== 0 || /Exception|Error while/i.test(stderr))
		throw new Error(
			`content query failed (exit ${result.exitCode}): ${stderr.split("\n")[0] ?? ""}`,
		);
	const stdout = result.stdout.trim();
	if (stdout !== "No result found." && !stdout.startsWith("Row: "))
		throw new Error(
			`unexpected content query output: ${stdout.slice(0, 120) || "(empty)"}`,
		);
	return parseContentRows(result.stdout, columns);
}
