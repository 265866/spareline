import type { Database } from "bun:sqlite";
import { beforeEach, expect, test } from "bun:test";
import { openDb } from "../db/db";
import {
	attribute,
	attributePending,
	insertMessages,
	MessageStore,
	type NewMessage,
} from "./store";

let db: Database;
let store: MessageStore;

const T = (day: number, hour = 12) =>
	new Date(Date.UTC(2026, 9, day, hour)).toISOString();

function phone(serial: string): number {
	return (
		db
			.query<{ id: number }, [string, string]>(
				"INSERT INTO phones (serial, site, last_state, first_seen_at) VALUES (?, 'home', 'device', ?) RETURNING id",
			)
			.get(serial, T(1)) as { id: number }
	).id;
}

function sim(iccid: string): number {
	return (
		db
			.query<{ id: number }, [string, string, string]>(
				"INSERT INTO sims (iccid, first_seen_at, last_seen_at) VALUES (?, ?, ?) RETURNING id",
			)
			.get(iccid, T(1), T(1)) as { id: number }
	).id;
}

function number(e164: string | null): number {
	return (
		db
			.query<{ id: number }, [string | null, string, string]>(
				"INSERT INTO numbers (e164, e164_source, created_at) VALUES (?, ?, ?) RETURNING id",
			)
			.get(e164, e164 ? "detected" : "none", T(1)) as { id: number }
	).id;
}

function place(
	simId: number,
	phoneId: number,
	slot: number,
	subId: number,
	from: string,
	to: string | null = null,
) {
	db.run(
		"INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at, to_at) VALUES (?, ?, ?, ?, ?, ?)",
		[simId, phoneId, slot, subId, from, to],
	);
}

function bind(
	numberId: number,
	simId: number,
	from: string,
	to: string | null = null,
) {
	db.run(
		"INSERT INTO number_bindings (number_id, sim_id, from_at, to_at) VALUES (?, ?, ?, ?)",
		[numberId, simId, from, to],
	);
}

function text(
	providerId: number,
	receivedAt: string,
	subId: number | null = 1,
): NewMessage {
	return {
		provider: "sms",
		providerId,
		subId,
		sender: "+15125550150",
		body: `Fake text ${providerId}`,
		subject: null,
		sentAt: null,
		receivedAt,
		attachments: [],
	};
}

beforeEach(() => {
	db = openDb(":memory:");
	store = new MessageStore(db);
});

test("re-ingesting the same provider rows adds nothing; a reset provider's reused ids are new", () => {
	const a = phone("A");
	expect(insertMessages(db, a, [text(1, T(2)), text(2, T(3))], T(5))).toEqual([
		1, 2,
	]);
	expect(insertMessages(db, a, [text(1, T(2)), text(2, T(3))], T(6))).toEqual(
		[],
	);
	// Messages' data cleared: _id 1 comes back with a different date. (A
	// skipped insert can use up a seq, so seqs only increase, with gaps.)
	const [reset] = insertMessages(db, a, [text(1, T(4))], T(6));
	expect(reset).toBeGreaterThan(2);
	// Another phone's _id 1 is its own message.
	const [other] = insertMessages(db, phone("B"), [text(1, T(2))], T(6));
	expect(other).toBeGreaterThan(reset as number);
	expect(store.maxSeq()).toBe(other as number);
	expect(store.list({ limit: 10, order: "asc" })).toHaveLength(4);
});

test("attribution follows a SIM across phones and slots", () => {
	const a = phone("A");
	const b = phone("B");
	const s1 = sim("8901555000000000011");
	const s2 = sim("8901555000000000029");
	const n1 = number("+15125550101");
	const n2 = number("+15125550102");
	// S1 in A slot 0 (sub 1) from day 10 to day 20, then in B slot 1. Android
	// reuses the sub id for the same ICCID.
	place(s1, a, 0, 1, T(10), T(20));
	place(s1, b, 1, 1, T(20));
	bind(n1, s1, T(10));
	// S2 goes into A slot 0 on day 21 as sub 2.
	place(s2, a, 0, 2, T(21));
	bind(n2, s2, T(21));

	const at = (phoneId: number, subId: number | null, day: number) =>
		attribute(db, { phoneId, subId, receivedAt: T(day) });
	expect(at(a, 1, 15)).toEqual({ simId: s1, numberId: n1 });
	expect(at(b, 1, 25)).toEqual({ simId: s1, numberId: n1 });
	expect(at(a, 2, 25)).toEqual({ simId: s2, numberId: n2 });
	// Received before the app first saw the SIM (an inbox backfill).
	expect(at(a, 1, 3)).toEqual({ simId: s1, numberId: n1 });
	// Delivered to A on sub 1 after S1 left: still S1 (same sub id).
	expect(at(a, 1, 22)).toEqual({ simId: s1, numberId: n1 });
	// No sub id: unambiguous only while one SIM is in the phone.
	expect(at(a, null, 25)).toEqual({ simId: s2, numberId: n2 });
	expect(at(a, null, 20.5)).toEqual({ simId: null, numberId: null });
	// A sub id the phone never had.
	expect(at(a, 7, 25)).toEqual({ simId: null, numberId: null });
});

test("a number change on the same SIM splits its messages at the change", () => {
	const a = phone("A");
	const s = sim("8901555000000000011");
	const before = number("+15125550101");
	const after = number("+15125550109");
	place(s, a, 0, 1, T(10));
	bind(before, s, T(10), T(15));
	bind(after, s, T(15));
	expect(attribute(db, { phoneId: a, subId: 1, receivedAt: T(12) })).toEqual({
		simId: s,
		numberId: before,
	});
	expect(attribute(db, { phoneId: a, subId: 1, receivedAt: T(16) })).toEqual({
		simId: s,
		numberId: after,
	});
});

test("texts from an unknown SIM wait unassigned, then get their number once, idempotently", () => {
	const a = phone("A");
	insertMessages(db, a, [text(1, T(2)), text(2, T(3))], T(5));
	expect(store.list({ limit: 10, order: "asc" }).map((m) => m.number)).toEqual([
		null,
		null,
	]);
	expect(attributePending(db)).toEqual([]);

	// The identity probe finds the SIM; its number is not detected yet, so
	// the binding points at a number row without e164.
	const s = sim("8901555000000000011");
	const n = number(null);
	place(s, a, 0, 1, T(6));
	bind(n, s, T(6));
	expect(attributePending(db)).toEqual([3, 4]);
	expect(attributePending(db)).toEqual([]);
	const messages = store.list({ limit: 10, order: "asc" });
	expect(messages.map((m) => [m.seq, m.simId, m.number])).toEqual([
		[3, s, { id: n, e164: null, note: "" }],
		[4, s, { id: n, e164: null, note: "" }],
	]);
	// Detection fills in the number row; the messages follow without a rewrite.
	db.run("UPDATE numbers SET e164 = '+15125550101', e164_source = 'detected'");
	expect(store.get(3)?.number).toEqual({
		id: n,
		e164: "+15125550101",
		note: "",
	});
});

test("a message that gains its number moves above every seq, with its attachments, so cursors see it", () => {
	const a = phone("A");
	const s1 = sim("8901555000000000011");
	const n1 = number("+15125550101");
	place(s1, a, 0, 1, T(1));
	bind(n1, s1, T(1));
	// Seq 1 on sub 2 (unknown SIM, with a picture), seq 2 on the known SIM.
	insertMessages(
		db,
		a,
		[
			{
				...text(1, T(2), 2),
				provider: "mms",
				attachments: [
					{
						contentType: "image/png",
						size: 3,
						sha256: "a".repeat(64),
						filename: null,
					},
				],
			},
			text(2, T(3), 1),
		],
		T(5),
	);
	const n2 = number("+15125550102");
	const s2 = sim("8901555000000000029");
	place(s2, a, 1, 2, T(6));
	bind(n2, s2, T(6));
	// A client of number 2 that had polled up to the newest seq (2).
	expect(attributePending(db)).toEqual([3]);
	expect(
		store.list({ numberId: n2, after: 2, limit: 10, order: "asc" }),
	).toEqual([
		expect.objectContaining({
			seq: 3,
			provider: "mms",
			attachments: [expect.objectContaining({ contentType: "image/png" })],
		}),
	]);
	expect(store.get(1)).toBeNull();
	expect(db.query("PRAGMA foreign_key_check").all()).toEqual([]);
	// The next new message still gets a seq above the moved one.
	const [next] = insertMessages(db, a, [text(3, T(7), 1)], T(8));
	expect(next).toBe(4);
});

test("list filters: number, search, paging both ways", () => {
	const a = phone("A");
	const s1 = sim("8901555000000000011");
	const s2 = sim("8901555000000000029");
	const n1 = number("+15125550101");
	const n2 = number("+15125550102");
	place(s1, a, 0, 1, T(1));
	place(s2, a, 1, 2, T(1));
	bind(n1, s1, T(1));
	bind(n2, s2, T(1));
	insertMessages(
		db,
		a,
		[
			{ ...text(1, T(2), 1), body: "100% interested" },
			{ ...text(2, T(3), 2), body: "Is it still for sale?" },
			{ ...text(3, T(4), 2), body: "under_score" },
		],
		T(5),
	);
	const seqs = (filter: Parameters<MessageStore["list"]>[0]) =>
		store.list(filter).map((m) => m.seq);
	expect(seqs({ limit: 10, order: "desc" })).toEqual([3, 2, 1]);
	expect(seqs({ limit: 10, order: "desc", numberId: n1 })).toEqual([1]);
	expect(seqs({ limit: 10, order: "asc", numberId: n2 })).toEqual([2, 3]);
	expect(seqs({ limit: 10, order: "asc", search: "SALE" })).toEqual([2]);
	// LIKE wildcards in the search are literal.
	expect(seqs({ limit: 10, order: "asc", search: "%" })).toEqual([1]);
	expect(seqs({ limit: 10, order: "asc", search: "_" })).toEqual([3]);
	expect(seqs({ limit: 10, order: "asc", search: "5550150" })).toEqual([
		1, 2, 3,
	]);
	// The receiving number in any format, and its note.
	const search = (text: string) =>
		seqs({ limit: 10, order: "asc", search: text });
	expect(search("5550101")).toEqual([1]);
	expect(search("(512) 555-0102")).toEqual([2, 3]);
	expect(search("+1 512 555 0101")).toEqual([1]);
	expect(search("512.555.0150")).toEqual([1, 2, 3]);
	db.run("UPDATE numbers SET note = 'Facebook ad, blue couch' WHERE id = ?", [
		n2,
	]);
	expect(search("facebook AD")).toEqual([2, 3]);
	expect(store.get(2)?.number?.note).toBe("Facebook ad, blue couch");
	expect(search("ad 0101")).toEqual([]);
	expect(seqs({ limit: 1, order: "desc", before: 3 })).toEqual([2]);
	expect(seqs({ limit: 10, order: "asc", after: 1 })).toEqual([2, 3]);
});
