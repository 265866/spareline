import type { Database } from "bun:sqlite";
import { afterEach, beforeEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { FakeAdb } from "../../test/fake-adb";
import { recordedTrackLists } from "../../test/fake-adb";
import { fixture } from "../../test/replay-shell";
import {
	B30,
	hex,
	NO_RESULT,
	type PartRead,
	RECORDED_IDS,
	smsFakeAdb,
} from "../../test/sms-fake";
import { Bus } from "../bus";
import { openDb } from "../db/db";
import { DeviceManager } from "../devices/manager";
import { IDENTITY_SCRIPT } from "../devices/probe";
import type { DeviceEventRow } from "../inventory/sync";
import { silent } from "../log";
import { SmsIngest } from "./ingest";
import { ISMS_TRIGGER_SCRIPT } from "./parse";
import { commands } from "./provider";
import { type Message, MessageStore } from "./store";

// DeviceManager runs against FakeAdb (recorded adb device lists and B30
// probe output); the ingest reads the B30's recorded provider output. New
// texts cannot arrive (the phone is out of service), so the ones added here
// are stand-ins built in the provider's recorded format.

let db: Database;
let adb: FakeAdb & { parts: Map<number, PartRead> };
let manager: DeviceManager;
let ingest: SmsIngest;
let store: MessageStore;
let bus: Bus<Message>;
let published: { seq: number; committed: boolean }[];
// The number each published message carried when it was published.
let publishedNumbers: (string | null)[];
let dir: string;
let clock: number;

async function online() {
	manager.start();
	adb.send((await recordedTrackLists())[0] ?? []);
	for (let i = 0; i < 200 && !manager.get(B30)?.healthAt; i++)
		await Bun.sleep(5);
	await manager.settled();
	await ingest.settled();
}

beforeEach(async () => {
	db = openDb(":memory:");
	adb = await smsFakeAdb();
	manager = new DeviceManager({
		adb,
		db,
		site: "home",
		events: new Bus<DeviceEventRow>(),
		log: silent,
		identityRetryMs: [],
	});
	store = new MessageStore(db);
	bus = new Bus();
	published = [];
	publishedNumbers = [];
	bus.subscribe((message) =>
		publishedNumbers.push(message.number?.e164 ?? null),
	);
	bus.subscribe((message) =>
		published.push({
			seq: message.seq,
			committed: store.get(message.seq) !== null,
		}),
	);
	dir = mkdtempSync(join(tmpdir(), "spareline-sms-"));
	clock = Date.parse("2026-10-22T08:00:00Z");
	ingest = new SmsIngest({
		devices: manager,
		db,
		store,
		messages: bus,
		attachmentsDir: join(dir, "attachments"),
		log: silent,
		// Ticks are driven by the test.
		triggerMs: 3_600_000,
		reconcileMs: 60_000,
		now: () => new Date(clock),
	});
	ingest.start();
});

afterEach(async () => {
	await ingest.stop();
	await manager.stop();
	rmSync(dir, { recursive: true, force: true });
});

const count = () =>
	db.query<{ n: number }, []>("SELECT count(*) AS n FROM messages").get()?.n;

test("a phone that comes online has its inbox backfilled once, attributed to its SIM's number", async () => {
	await online();
	expect(count()).toBe(8);
	const messages = store.list({ limit: 50, order: "asc" });
	expect(messages.map((m) => m.providerId)).toEqual(RECORDED_IDS);
	// The SIM's recorded (fake) number, detected by the identity probe.
	expect(messages.every((m) => m.number?.e164 === "+15125550101")).toBe(true);
	expect(messages[0]).toMatchObject({
		provider: "sms",
		phone: B30,
		subId: 1,
		from: "550",
		body: "Fake text 1, is the café table still for sale? ✓",
		sentAt: new Date(1792623882000).toISOString(),
		receivedAt: new Date(1792637380314).toISOString(),
		ingestedAt: "2026-10-22T08:00:00.000Z",
		attachments: [],
	});
	// Published after commit, once each, in seq order.
	expect(published).toEqual(
		messages.map((m) => ({ seq: m.seq, committed: true })),
	);
	expect(
		db
			.query(
				"SELECT provider, last_provider_id FROM ingest_cursors ORDER BY provider",
			)
			.all(),
	).toEqual([
		{ provider: "mms", last_provider_id: 0 },
		{ provider: "sms", last_provider_id: 11 },
	]);

	expect(await ingest.reconcile(B30)).toBe(0);
	expect(count()).toBe(8);
	expect(published).toHaveLength(8);
});

test("an isms change pulls only rows after the cursor, and publishes them", async () => {
	await online();
	clock += 3_000;
	await ingest.tick(B30);
	expect(adb.calls.at(-1)).toContain(ISMS_TRIGGER_SCRIPT);

	// Stand-in: what the providers and isms show after one more text.
	const isms = await fixture("nuu-s6702x", "isms.txt");
	adb.answer(
		ISMS_TRIGGER_SCRIPT,
		isms.replace("total records=105", "total records=111"),
	);
	adb.answer(
		commands.smsAfter(11),
		`Row: 0 _id=12, thread_id=4, address=+15125550153, date=1792656062003, date_sent=1792656060000, sub_id=1, read=0, type=1, hex(body)=${hex("Hi, saw your ad")}\n`,
	);
	const before = adb.calls.length;
	clock += 3_000;
	await ingest.tick(B30);
	const commandsRun = adb.calls
		.slice(before)
		.map((call) => call.slice(B30.length + 2));
	expect(commandsRun).toEqual([ISMS_TRIGGER_SCRIPT, commands.smsAfter(11)]);
	expect(published.at(-1)).toEqual({ seq: 9, committed: true });
	expect(store.get(9)).toMatchObject({
		providerId: 12,
		from: "+15125550153",
		body: "Hi, saw your ad",
		number: { e164: "+15125550101" },
	});

	// Unchanged isms: no provider query.
	const quiet = adb.calls.length;
	clock += 3_000;
	await ingest.tick(B30);
	expect(adb.calls.slice(quiet)).toHaveLength(1);
});

test("an isms change that finds nothing yet keeps looking for a few ticks", async () => {
	await online();
	const isms = await fixture("nuu-s6702x", "isms.txt");
	adb.answer(
		ISMS_TRIGGER_SCRIPT,
		isms.replace("total records=105", "total records=106"),
	);
	adb.answer(commands.mmsAfter(0), NO_RESULT);
	for (let i = 0; i < 5; i++) {
		clock += 3_000;
		await ingest.tick(B30);
	}
	const queries = adb.calls.filter((call) => call.includes("content query"));
	// Backfill (sms keys, sms rows, mms keys), then 3 follow-ups of sms + mms.
	expect(
		queries.filter((call) => call.endsWith(commands.smsAfter(11))),
	).toHaveLength(3);
	expect(
		queries.filter((call) => call.endsWith(commands.mmsAfter(0))),
	).toHaveLength(3);
});

test("a cleared provider whose ids start over is reconciled, and the cursor moves back", async () => {
	await online();
	// Stand-in: Messages' data was cleared and one new text took _id 1.
	const row = `Row: 0 _id=1, thread_id=1, address=+15125550154, date=1792700000000, date_sent=1792699990000, sub_id=1, read=0, type=1, hex(body)=${hex("After the reset")}\n`;
	adb.answer(commands.smsKeys(), "Row: 0 _id=1, date=1792700000000\n");
	adb.answer(commands.smsByIds([1]), row);
	clock += 61_000;
	await ingest.tick(B30);
	expect(count()).toBe(9);
	expect(store.get(9)?.body).toBe("After the reset");
	expect(
		db
			.query<{ id: number }, []>(
				"SELECT last_provider_id AS id FROM ingest_cursors WHERE provider = 'sms'",
			)
			.get()?.id,
	).toBe(1);
});

// MMS: the B30's MMS inbox is empty, so these are hand-made provider rows in
// AOSP's column layout (Telephony.Mms, Mms.Addr, Mms.Part), stand-ins for
// what cannot be produced on demand.
const JPEG = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46]);

function answerMms(
	id: number,
	row: { date: number; subId: number; parts: string[] },
) {
	adb.answer(commands.mmsKeys(), `Row: 0 _id=${id}, date=${row.date}\n`);
	adb.answer(
		commands.mmsByIds([id]),
		`Row: 0 _id=${id}, date=${row.date}, date_sent=${row.date - 5}, sub_id=${row.subId}, m_type=132, hex(sub)=${hex("Photo")}\n`,
	);
	adb.answer(
		commands.mmsParts([id]),
		row.parts.length ? `${row.parts.join("\n")}\n` : NO_RESULT,
	);
	adb.answer(
		commands.mmsAddr(id),
		"Row: 0 type=151, address=+15125550101\nRow: 1 type=137, address=+15125550155\n",
	);
}

const PHOTO_PARTS = [
	`Row: 0 _id=29, mid=7, seq=-1, ct=application/smil, hex(name)=${hex("smil.xml")}, hex(fn)=, hex(cl)=, hex(text)=${hex("<smil/>")}`,
	`Row: 1 _id=30, mid=7, seq=0, ct=text/plain, hex(name)=, hex(fn)=, hex(cl)=${hex("text_0.txt")}, hex(text)=${hex("Here it is,\n\nfront view")}`,
	`Row: 2 _id=31, mid=7, seq=1, ct=image/jpeg, hex(name)=${hex("IMG 1, front.jpg")}, hex(fn)=, hex(cl)=, hex(text)=`,
];

const mmsCount = () =>
	db
		.query<{ n: number }, []>(
			"SELECT count(*) AS n FROM messages WHERE provider = 'mms'",
		)
		.get()?.n;

test("an MMS becomes one message once two reads agree: text parts as the body, other parts as files", async () => {
	await online();
	adb.parts.set(31, { stdout: JPEG });
	answerMms(7, { date: 1792656100, subId: 1, parts: PHOTO_PARTS });
	clock += 61_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(0);
	clock += 3_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(1);
	const mms = store.list({ limit: 1, order: "desc" })[0];
	expect(mms).toMatchObject({
		provider: "mms",
		providerId: 7,
		from: "+15125550155",
		subject: "Photo",
		body: "Here it is,\n\nfront view",
		sentAt: new Date(1792656095000).toISOString(),
		receivedAt: new Date(1792656100000).toISOString(),
		number: { e164: "+15125550101" },
	});
	expect(mms?.attachments).toEqual([
		{
			id: 1,
			contentType: "image/jpeg",
			size: 8,
			filename: "IMG 1, front.jpg",
			url: "/api/v1/attachments/1",
		},
	]);
	const sha = createHash("sha256").update(JPEG).digest("hex");
	expect(store.attachment(1)?.sha256).toBe(sha);
	expect([...readFileSync(join(dir, "attachments", sha))]).toEqual([...JPEG]);
	expect(
		adb.calls.filter((call) =>
			call.endsWith("content read --uri content://mms/part/31"),
		),
	).toHaveLength(1);
	clock += 61_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(1);
});

// AOSP's DownloadRequest inserts the MMS and then updates its date and
// subscription; a read in between sees the first values and no parts.
test("an MMS read mid-write is stored once, with its final date and parts", async () => {
	await online();
	adb.parts.set(31, { stdout: JPEG });
	answerMms(7, { date: 1792656000, subId: -1, parts: [] });
	clock += 61_000;
	await ingest.tick(B30);
	answerMms(7, { date: 1792656100, subId: 1, parts: PHOTO_PARTS });
	clock += 3_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(0);
	clock += 3_000;
	await ingest.tick(B30);
	clock += 61_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(1);
	expect(store.list({ limit: 1, order: "desc" })[0]).toMatchObject({
		receivedAt: new Date(1792656100000).toISOString(),
		subId: 1,
		attachments: [{ contentType: "image/jpeg", size: 8 }],
	});
});

// Content.java prints provider errors on stderr and exits 0; over exec:
// adbd would merge that text into the bytes.
test("a part read that reports an error is retried, never stored as the attachment", async () => {
	await online();
	adb.parts.set(31, {
		stdout: new TextEncoder().encode("partial"),
		stderr:
			"Error while accessing provider:mms\njava.io.FileNotFoundException: No entry for content://mms/part/31\n",
	});
	answerMms(7, { date: 1792656100, subId: 1, parts: PHOTO_PARTS });
	clock += 61_000;
	await ingest.tick(B30);
	clock += 3_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(0);
	// A shell that dies without its exit status is no better.
	adb.parts.set(31, "no exit packet");
	clock += 61_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(0);
	adb.parts.set(31, { stdout: JPEG });
	clock += 61_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(1);
	const stored = store.list({ limit: 1, order: "desc" })[0];
	expect(stored?.attachments.map((a) => a.size)).toEqual([8]);
	const sha = createHash("sha256").update(JPEG).digest("hex");
	expect(store.attachment(stored?.attachments[0]?.id ?? 0)?.sha256).toBe(sha);
});

test("an unreadable part gives up after five reconciles, not counting follow-up reads", async () => {
	await online();
	answerMms(8, {
		date: 1792656200,
		subId: 1,
		parts: [
			"Row: 0 _id=40, mid=8, seq=0, ct=image/png, hex(name)=, hex(fn)=, hex(cl)=, hex(text)=",
		],
	});
	// First sight, then a confirming follow-up read (not counted).
	clock += 61_000;
	await ingest.tick(B30);
	clock += 3_000;
	await ingest.tick(B30);
	for (let attempt = 1; attempt <= 4; attempt++) {
		clock += 61_000;
		await ingest.tick(B30);
		clock += 3_000;
		await ingest.tick(B30);
		expect(mmsCount()).toBe(0);
	}
	clock += 61_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(1);
	expect(store.list({ limit: 1, order: "desc" })[0]).toMatchObject({
		provider: "mms",
		attachments: [],
	});
});

test("an MMS row without an integer _id is skipped, and the rest still ingest", async () => {
	await online();
	adb.parts.set(31, { stdout: JPEG });
	answerMms(7, { date: 1792656100, subId: 1, parts: PHOTO_PARTS });
	// Stand-in for corrupted output: rows whose _id is not a number.
	adb.answer(
		commands.mmsKeys(),
		"Row: 0 _id=7, date=1792656100\nRow: 1 _id=x, date=1792656101\n",
	);
	adb.answer(
		commands.mmsByIds([7]),
		`Row: 0 _id=7, date=1792656100, date_sent=1792656095, sub_id=1, m_type=132, hex(sub)=\nRow: 1 _id=x, date=1792656101, date_sent=0, sub_id=1, m_type=132, hex(sub)=\n`,
	);
	clock += 61_000;
	await ingest.tick(B30);
	clock += 3_000;
	await ingest.tick(B30);
	expect(mmsCount()).toBe(1);
	expect(adb.calls.some((call) => call.includes("mid IN (7,"))).toBe(false);
});

test("ingest waits for the phone's identity, so the first texts already have their number", async () => {
	// Hold the identity probe: until it is applied, the SIM is not placed.
	let release = () => {};
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	const shell = adb.shell.bind(adb);
	adb.shell = async (serial: string, command: string) => {
		if (command === IDENTITY_SCRIPT) await gate;
		return shell(serial, command);
	};
	manager.start();
	adb.send((await recordedTrackLists())[0] ?? []);
	for (let i = 0; i < 200 && manager.get(B30)?.status !== "online"; i++)
		await Bun.sleep(5);
	await Bun.sleep(50);
	expect(adb.calls.some((call) => call.includes("content query"))).toBe(false);
	release();
	for (let i = 0; i < 200 && count() !== 8; i++) await Bun.sleep(5);
	await manager.settled();
	await ingest.settled();
	expect(publishedNumbers).toHaveLength(8);
	expect(publishedNumbers.every((e164) => e164 === "+15125550101")).toBe(true);
});

test("texts stored without a number are published again, at new seqs, once the SIM is known", async () => {
	await online();
	// Stand-in: texts on sub 2, a SIM the identity probe has not placed yet.
	clock += 3_000;
	adb.answer(
		ISMS_TRIGGER_SCRIPT,
		(await fixture("nuu-s6702x", "isms.txt")).replace(
			"total records=105",
			"total records=112",
		),
	);
	adb.answer(
		commands.smsAfter(11),
		`Row: 0 _id=12, thread_id=5, address=+15125550158, date=1792656062003, date_sent=1792656060000, sub_id=2, read=0, type=1, hex(body)=${hex("On the second SIM")}\n`,
	);
	await ingest.tick(B30);
	expect(store.get(9)?.number).toBeNull();
	db.run(
		"INSERT INTO sims (iccid, first_seen_at, last_seen_at) VALUES ('8901555000000000029', ?1, ?1)",
		["2026-10-22T08:00:10.000Z"],
	);
	db.run(
		"INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (2, 1, 1, 2, ?)",
		["2026-10-22T08:00:10.000Z"],
	);
	db.run(
		"INSERT INTO numbers (e164, e164_source, created_at) VALUES ('+15125550102', 'detected', ?)",
		["2026-10-22T08:00:10.000Z"],
	);
	db.run(
		"INSERT INTO number_bindings (number_id, sim_id, from_at) VALUES (2, 2, ?)",
		["2026-10-22T08:00:10.000Z"],
	);
	const before = published.length;
	ingest.attributePending();
	expect(published.slice(before)).toEqual([{ seq: 10, committed: true }]);
	expect(publishedNumbers.at(-1)).toBe("+15125550102");
	expect(store.get(9)).toBeNull();
	expect(
		store.list({ numberId: 2, after: 8, limit: 10, order: "asc" }),
	).toEqual([expect.objectContaining({ seq: 10, providerId: 12 })]);
});

test("a phone that goes away stops being polled", async () => {
	await online();
	adb.send([]);
	for (let i = 0; i < 100 && manager.get(B30)?.status === "online"; i++)
		await Bun.sleep(5);
	const before = adb.calls.length;
	await ingest.tick(B30);
	expect(adb.calls.length).toBe(before);
});

test("a failed reconcile is retried after 15 s, not on every tick", async () => {
	await online();
	const keysCalls = () =>
		adb.calls.filter((call) => call.endsWith(commands.smsKeys())).length;
	const before = keysCalls();
	adb.failing = new Error("adb transport closed (stand-in)");
	clock += 61_000;
	await ingest.tick(B30);
	adb.failing = undefined;
	clock += 3_000;
	await ingest.tick(B30);
	expect(keysCalls()).toBe(before);
	clock += 13_000;
	await ingest.tick(B30);
	expect(keysCalls()).toBe(before + 1);
});

test("an MMS with an unreadable part is retried on reconciles only, not re-read every tick", async () => {
	await online();
	answerMms(8, {
		date: 1792656200,
		subId: 1,
		parts: [
			"Row: 0 _id=40, mid=8, seq=0, ct=image/png, hex(name)=, hex(fn)=, hex(cl)=, hex(text)=",
		],
	});
	const before = adb.calls.length;
	// 360 simulated seconds of 3 s ticks: 6 reconciles. The first sees the
	// MMS, the next tick confirms it (one read), and the other 5 retry it.
	for (let tick = 0; tick < 120; tick++) {
		clock += 3_000;
		await ingest.tick(B30);
	}
	const calls = adb.calls.slice(before);
	const partReads = calls.filter((call) =>
		call.includes("content read"),
	).length;
	const queries = calls.filter((call) => call.includes("content query")).length;
	// One read when two reads first agree, then one per reconcile.
	expect(partReads).toBeLessThanOrEqual(6);
	expect(queries).toBeLessThanOrEqual(40);
	expect(mmsCount()).toBe(1);
});

test("a stored MMS is not read again by the trigger's follow-up queries", async () => {
	await online();
	adb.parts.set(31, { stdout: JPEG });
	answerMms(7, { date: 1792656100, subId: 1, parts: PHOTO_PARTS });
	// Stand-in: isms changes as the MMS arrives; the follow-ups find it after
	// the MMS cursor (0), and nothing after it once it is passed.
	adb.answer(
		commands.mmsAfter(0),
		`Row: 0 _id=7, date=1792656100, date_sent=1792656095, sub_id=1, m_type=132, hex(sub)=${hex("Photo")}\n`,
	);
	adb.answer(commands.mmsAfter(7), NO_RESULT);
	adb.answer(
		ISMS_TRIGGER_SCRIPT,
		(await fixture("nuu-s6702x", "isms.txt")).replace(
			"total records=105",
			"total records=106",
		),
	);
	const before = adb.calls.length;
	for (let tick = 0; tick < 4; tick++) {
		clock += 3_000;
		await ingest.tick(B30);
	}
	expect(mmsCount()).toBe(1);
	const calls = adb.calls.slice(before);
	expect(calls.filter((call) => call.includes("content read"))).toHaveLength(1);
	expect(calls.filter((call) => call.includes("/addr"))).toHaveLength(1);
	expect(
		db
			.query<{ id: number }, []>(
				"SELECT last_provider_id AS id FROM ingest_cursors WHERE provider = 'mms'",
			)
			.get()?.id,
	).toBe(7);
});
