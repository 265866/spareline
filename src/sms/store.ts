import type { Database } from "bun:sqlite";
import type { Provider, ProviderMessage } from "./provider";

// Messages in SQLite: the single writer's inserts, attribution to SIMs and
// numbers, and the reads behind the API.

export type StoredAttachment = {
	contentType: string;
	size: number;
	sha256: string;
	filename: string | null;
};

export type NewMessage = Omit<ProviderMessage, "parts"> & {
	attachments: StoredAttachment[];
};

export type AttachmentView = {
	id: number;
	contentType: string;
	size: number;
	filename: string | null;
	url: string;
};

export type Message = {
	seq: number;
	provider: Provider;
	providerId: number;
	phone: string;
	subId: number | null;
	simId: number | null;
	// The number the text was sent to; null while its SIM is unknown.
	number: { id: number; e164: string | null; note: string } | null;
	from: string | null;
	body: string;
	subject: string | null;
	sentAt: string | null;
	receivedAt: string;
	ingestedAt: string;
	attachments: AttachmentView[];
};

type Attribution = { simId: number | null; numberId: number | null };

// Which SIM and number a message belongs to. sim_placements and
// number_bindings start when the app first saw them, so a message older than
// that (an inbox backfilled on first sight) takes the earliest record after
// it; Android keeps one sub id per ICCID, so (phone, sub id) still names the
// same SIM.
export function attribute(
	db: Database,
	message: { phoneId: number; subId: number | null; receivedAt: string },
): Attribution {
	const at = message.receivedAt;
	let simId: number | null = null;
	if (message.subId !== null && message.subId >= 0) {
		simId =
			db
				.query<{ sim_id: number }, { phone: number; sub: number; at: string }>(
					`SELECT sim_id FROM sim_placements WHERE phone_id = $phone AND sub_id = $sub
					ORDER BY ${NEAREST} LIMIT 1`,
				)
				.get({ phone: message.phoneId, sub: message.subId, at })?.sim_id ??
			null;
	} else {
		// No sub id: only a phone with a single SIM at the time is unambiguous.
		const covering = db
			.query<{ sim_id: number }, { phone: number; at: string }>(
				`SELECT sim_id FROM sim_placements WHERE phone_id = $phone
				AND from_at <= $at AND (to_at IS NULL OR to_at > $at)`,
			)
			.all({ phone: message.phoneId, at });
		simId = covering.length === 1 ? (covering[0]?.sim_id ?? null) : null;
	}
	if (simId === null) return { simId, numberId: null };
	const numberId =
		db
			.query<{ number_id: number }, { sim: number; at: string }>(
				`SELECT number_id FROM number_bindings WHERE sim_id = $sim
				ORDER BY ${NEAREST} LIMIT 1`,
			)
			.get({ sim: simId, at })?.number_id ?? null;
	return { simId, numberId };
}

// Covering $at first, then the earliest record starting after it, then the
// latest one that ended before it.
const NEAREST = `CASE WHEN from_at <= $at AND (to_at IS NULL OR to_at > $at) THEN 0
	WHEN from_at > $at THEN 1 ELSE 2 END,
	CASE WHEN from_at > $at THEN from_at END ASC, to_at DESC`;

// Inserts messages from one phone in one transaction and returns the ones
// that were new, in seq order. The UNIQUE key makes re-ingesting a no-op.
export function insertMessages(
	db: Database,
	phoneId: number,
	messages: NewMessage[],
	ingestedAt: string,
): number[] {
	const insert = db.query<
		{ seq: number },
		[
			number,
			string,
			number,
			number | null,
			number | null,
			number | null,
			string | null,
			string,
			string | null,
			string | null,
			string,
			string,
		]
	>(
		`INSERT INTO messages (phone_id, provider, provider_id, sub_id, sim_id, number_id, sender, body, subject, sent_at, received_at, ingested_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT (phone_id, provider, provider_id, received_at) DO NOTHING RETURNING seq`,
	);
	const insertAttachment = db.query(
		`INSERT INTO attachments (message_seq, content_type, size, sha256, filename)
		VALUES (?, ?, ?, ?, ?)`,
	);
	return db.transaction(() => {
		const seqs: number[] = [];
		for (const message of messages) {
			const { simId, numberId } = attribute(db, {
				phoneId,
				subId: message.subId,
				receivedAt: message.receivedAt,
			});
			const row = insert.get(
				phoneId,
				message.provider,
				message.providerId,
				message.subId,
				simId,
				numberId,
				message.sender,
				message.body,
				message.subject,
				message.sentAt,
				message.receivedAt,
				ingestedAt,
			);
			if (!row) continue;
			for (const attachment of message.attachments)
				insertAttachment.run(
					row.seq,
					attachment.contentType,
					attachment.size,
					attachment.sha256,
					attachment.filename,
				);
			seqs.push(row.seq);
		}
		return seqs;
	})();
}

// Fills in the SIM and number of messages stored before either was known.
// A message that gains a number moves to a new seq above every other, so a
// client polling that number with `after=<seq>` sees it; the old seq is left
// as a gap. Idempotent. Returns the new seqs, in order, for publishing.
export function attributePending(db: Database): number[] {
	const pending = db
		.query<
			{
				seq: number;
				phone_id: number;
				sub_id: number | null;
				sim_id: number | null;
				received_at: string;
			},
			[]
		>(
			"SELECT seq, phone_id, sub_id, sim_id, received_at FROM messages WHERE number_id IS NULL ORDER BY seq",
		)
		.all();
	if (pending.length === 0) return [];
	const setSim = db.query("UPDATE messages SET sim_id = ? WHERE seq = ?");
	const move = db.query(
		"UPDATE messages SET seq = ?, sim_id = ?, number_id = ? WHERE seq = ?",
	);
	const moveAttachments = db.query(
		"UPDATE attachments SET message_seq = ? WHERE message_seq = ?",
	);
	// AUTOINCREMENT never reuses a seq it handed out: the next one is above
	// both the highest row and sqlite_sequence, which an UPDATE does not raise.
	const nextSeq = db.query<{ seq: number }, []>(
		"SELECT max(coalesce((SELECT max(seq) FROM messages), 0), coalesce((SELECT seq FROM sqlite_sequence WHERE name = 'messages'), 0)) + 1 AS seq",
	);
	return db.transaction(() => {
		// The attachments follow their message within this transaction.
		db.run("PRAGMA defer_foreign_keys = ON");
		const moved: number[] = [];
		for (const row of pending) {
			const found = attribute(db, {
				phoneId: row.phone_id,
				subId: row.sub_id,
				receivedAt: row.received_at,
			});
			const simId = found.simId ?? row.sim_id;
			if (found.numberId === null) {
				if (simId !== row.sim_id) setSim.run(simId, row.seq);
				continue;
			}
			const seq = (nextSeq.get() as { seq: number }).seq;
			move.run(seq, simId, found.numberId, row.seq);
			moveAttachments.run(seq, row.seq);
			db.run("UPDATE sqlite_sequence SET seq = ? WHERE name = 'messages'", [
				seq,
			]);
			moved.push(seq);
		}
		return moved;
	})();
}

export function getCursor(
	db: Database,
	phoneId: number,
	provider: Provider,
): number {
	return (
		db
			.query<{ last_provider_id: number }, [number, string]>(
				"SELECT last_provider_id FROM ingest_cursors WHERE phone_id = ? AND provider = ?",
			)
			.get(phoneId, provider)?.last_provider_id ?? 0
	);
}

export function setCursor(
	db: Database,
	phoneId: number,
	provider: Provider,
	lastProviderId: number,
): void {
	db.run(
		`INSERT INTO ingest_cursors (phone_id, provider, last_provider_id) VALUES (?, ?, ?)
		ON CONFLICT (phone_id, provider) DO UPDATE SET last_provider_id = excluded.last_provider_id`,
		[phoneId, provider, lastProviderId],
	);
}

// "<provider_id>|<received_at>" for every stored message of a phone.
export function storedKeys(
	db: Database,
	phoneId: number,
	provider: Provider,
): Set<string> {
	return new Set(
		db
			.query<{ key: string }, [number, string]>(
				"SELECT provider_id || '|' || received_at AS key FROM messages WHERE phone_id = ? AND provider = ?",
			)
			.all(phoneId, provider)
			.map((row) => row.key),
	);
}

export type MessageFilter = {
	numberId?: number;
	// Substring of the body, subject, or sender.
	search?: string;
	after?: number;
	before?: number;
	limit: number;
	// "asc": oldest first (polling forward); "desc": newest first.
	order: "asc" | "desc";
};

type MessageDb = {
	seq: number;
	provider: Provider;
	provider_id: number;
	serial: string;
	sub_id: number | null;
	sim_id: number | null;
	number_id: number | null;
	e164: string | null;
	// Null only when the text has no number (LEFT JOIN).
	note: string | null;
	sender: string | null;
	body: string;
	subject: string | null;
	sent_at: string | null;
	received_at: string;
	ingested_at: string;
};

const SELECT_MESSAGES = `SELECT m.seq, m.provider, m.provider_id, p.serial, m.sub_id, m.sim_id, m.number_id, n.e164, n.note,
	m.sender, m.body, m.subject, m.sent_at, m.received_at, m.ingested_at
	FROM messages m JOIN phones p ON p.id = m.phone_id LEFT JOIN numbers n ON n.id = m.number_id`;

export class MessageStore {
	constructor(private readonly db: Database) {}

	list(filter: MessageFilter): Message[] {
		const where: string[] = [];
		const params: (string | number)[] = [];
		if (filter.numberId !== undefined) {
			where.push("m.number_id = ?");
			params.push(filter.numberId);
		}
		const search = filter.search;
		if (search) {
			const like = (text: string) =>
				`%${text.replace(/[\\%_]/g, (c) => `\\${c}`)}%`;
			const terms: [string, string][] = [
				"m.body",
				"m.sender",
				"m.subject",
				"n.e164",
				"n.note",
			].map((field) => [field, like(search)]);
			// "(512) 555" or "+1 512" finds a number or sender by its digits.
			const digits = search.replace(/\D/g, "");
			if (/^[\d\s()+.-]+$/.test(search) && digits !== "")
				terms.push(["n.e164", like(digits)], ["m.sender", like(digits)]);
			where.push(
				`(${terms.map(([field]) => `${field} LIKE ? ESCAPE '\\'`).join(" OR ")})`,
			);
			params.push(...terms.map(([, pattern]) => pattern));
		}
		if (filter.after !== undefined) {
			where.push("m.seq > ?");
			params.push(filter.after);
		}
		if (filter.before !== undefined) {
			where.push("m.seq < ?");
			params.push(filter.before);
		}
		const sql = `${SELECT_MESSAGES}${where.length ? ` WHERE ${where.join(" AND ")}` : ""}
			ORDER BY m.seq ${filter.order === "asc" ? "ASC" : "DESC"} LIMIT ?`;
		const rows = this.db
			.query<MessageDb, (string | number)[]>(sql)
			.all(...params, filter.limit);
		return this.views(rows);
	}

	get(seq: number): Message | null {
		const row = this.db
			.query<MessageDb, [number]>(`${SELECT_MESSAGES} WHERE m.seq = ?`)
			.get(seq);
		return row ? (this.views([row])[0] ?? null) : null;
	}

	maxSeq(): number {
		return (
			this.db
				.query<{ seq: number | null }, []>(
					"SELECT max(seq) AS seq FROM messages",
				)
				.get()?.seq ?? 0
		);
	}

	attachment(
		id: number,
	): { sha256: string; contentType: string; filename: string | null } | null {
		const row = this.db
			.query<
				{ sha256: string; content_type: string; filename: string | null },
				[number]
			>("SELECT sha256, content_type, filename FROM attachments WHERE id = ?")
			.get(id);
		return row
			? {
					sha256: row.sha256,
					contentType: row.content_type,
					filename: row.filename,
				}
			: null;
	}

	private views(rows: MessageDb[]): Message[] {
		const attachments = new Map<number, AttachmentView[]>();
		if (rows.length > 0) {
			const found = this.db
				.query<
					{
						id: number;
						message_seq: number;
						content_type: string;
						size: number;
						filename: string | null;
					},
					number[]
				>(
					`SELECT id, message_seq, content_type, size, filename FROM attachments
					WHERE message_seq IN (${rows.map(() => "?").join(",")}) ORDER BY id`,
				)
				.all(...rows.map((row) => row.seq));
			for (const row of found) {
				const list = attachments.get(row.message_seq) ?? [];
				list.push({
					id: row.id,
					contentType: row.content_type,
					size: row.size,
					filename: row.filename,
					url: `/api/v1/attachments/${row.id}`,
				});
				attachments.set(row.message_seq, list);
			}
		}
		return rows.map((row) => ({
			seq: row.seq,
			provider: row.provider,
			providerId: row.provider_id,
			phone: row.serial,
			subId: row.sub_id,
			simId: row.sim_id,
			number:
				row.number_id === null
					? null
					: { id: row.number_id, e164: row.e164, note: row.note ?? "" },
			from: row.sender,
			body: row.body,
			subject: row.subject,
			sentAt: row.sent_at,
			receivedAt: row.received_at,
			ingestedAt: row.ingested_at,
			attachments: attachments.get(row.seq) ?? [],
		}));
	}
}
