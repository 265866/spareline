import type { Database } from "bun:sqlite";
import { afterEach, beforeEach, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { FakeAdb } from "../../test/fake-adb";
import { recordedTrackLists } from "../../test/fake-adb";
import { fixture } from "../../test/replay-shell";
import { signedIn, testSession } from "../../test/session";
import { B30, hex, type PartRead, smsFakeAdb } from "../../test/sms-fake";
import { Bus } from "../bus";
import { openDb } from "../db/db";
import { DeviceManager } from "../devices/manager";
import { Inventory } from "../inventory/store";
import { type DeviceEventRow, insertEvents } from "../inventory/sync";
import { silent } from "../log";
import { saveAttachment } from "../sms/attachments";
import { SmsIngest } from "../sms/ingest";
import { ISMS_TRIGGER_SCRIPT } from "../sms/parse";
import { commands } from "../sms/provider";
import { insertMessages, type Message, MessageStore } from "../sms/store";
import { type App, createApp } from "./app";
import type { MessagePageView, MessageView } from "./message-schema";
import type { NumberView } from "./schema";

// The B30's recorded inbox (8 texts, fake senders and bodies) is backfilled
// through the real ingest; DeviceManager runs on FakeAdb's recordings. Texts
// that "arrive" during a test are stand-ins in the provider's format, since
// the phone cannot receive any on demand.

let db: Database;
let adb: FakeAdb & { parts: Map<number, PartRead> };
let manager: DeviceManager;
let ingest: SmsIngest;
let store: MessageStore;
let messages: Bus<Message>;
let events: Bus<DeviceEventRow>;
let app: App;
let dir: string;
const NUMBER = "+15125550101";

beforeEach(async () => {
	db = openDb(":memory:");
	adb = await smsFakeAdb();
	events = new Bus<DeviceEventRow>();
	manager = new DeviceManager({
		adb,
		db,
		site: "home",
		events,
		log: silent,
		identityRetryMs: [],
	});
	store = new MessageStore(db);
	messages = new Bus();
	dir = mkdtempSync(join(tmpdir(), "spareline-api-"));
	const attachmentsDir = join(dir, "attachments");
	ingest = new SmsIngest({
		devices: manager,
		db,
		store,
		messages,
		attachmentsDir,
		log: silent,
		triggerMs: 3_600_000,
		reconcileMs: 3_600_000,
	});
	const session = testSession(db);
	app = signedIn(
		createApp({
			auth: session.auth,
			inventory: new Inventory(db),
			devices: manager,
			events,
			messages: { store, bus: messages, attachmentsDir },
			webDir: null,
			version: "test",
			heartbeatMs: 50,
		}),
		session.cookie,
	);
	ingest.start();
	manager.start();
	adb.send((await recordedTrackLists())[0] ?? []);
	for (let i = 0; i < 200 && !manager.get(B30)?.healthAt; i++)
		await Bun.sleep(5);
	await manager.settled();
	await ingest.settled();
});

afterEach(async () => {
	await ingest.stop();
	await manager.stop();
	rmSync(dir, { recursive: true, force: true });
});

const get = (path: string, headers: Record<string, string> = {}) =>
	app.handle(new Request(`http://localhost${path}`, { headers }));
const json = async <T>(path: string): Promise<T> => {
	const response = await get(path);
	expect(response.status).toBe(200);
	return (await response.json()) as T;
};
const seqs = (page: MessagePageView) => page.messages.map((m) => m.seq);

// Stand-in for one more text reaching the B30: the provider row and the isms
// change the ingest's next tick finds.
async function arrive(id: number, body: string) {
	const isms = await fixture("nuu-s6702x", "isms.txt");
	adb.answer(
		ISMS_TRIGGER_SCRIPT,
		isms.replace("total records=105", `total records=${105 + id}`),
	);
	adb.answer(
		commands.smsAfter(id - 1),
		`Row: 0 _id=${id}, thread_id=4, address=+15125550153, date=${1792656000000 + id}, date_sent=1792656000000, sub_id=1, read=0, type=1, hex(body)=${hex(body)}\n`,
	);
	adb.answer(commands.smsAfter(id), "No result found.\n");
	await ingest.tick(B30);
}

test("numbers list message counts and the last message", async () => {
	const { numbers } = await json<{ numbers: NumberView[] }>("/api/v1/numbers");
	expect(numbers).toHaveLength(1);
	expect(numbers[0]).toMatchObject({
		e164: NUMBER,
		messageCount: 8,
		lastMessage: {
			seq: 8,
			from: "+15125550151",
			preview: "Fake text 8, is the café table still for sale? ✓",
		},
	});
});

test("a number's messages: newest first, paging with before, any key form", async () => {
	const page = await json<MessagePageView>(
		`/api/v1/numbers/${encodeURIComponent(NUMBER)}/messages?limit=3`,
	);
	expect(seqs(page)).toEqual([8, 7, 6]);
	expect(page.cursor).toBe(8);
	expect(page.messages[0]).toEqual({
		seq: 8,
		provider: "sms",
		providerId: 11,
		phone: B30,
		subId: 1,
		simId: 1,
		number: { id: 1, e164: NUMBER, note: "" },
		from: "+15125550151",
		body: "Fake text 8, is the café table still for sale? ✓",
		subject: null,
		sentAt: new Date(1792637413000).toISOString(),
		receivedAt: new Date(1792637427282).toISOString(),
		ingestedAt: expect.any(String),
		attachments: [],
	} satisfies MessageView);
	const older = await json<MessagePageView>(
		`/api/v1/numbers/${NUMBER}/messages?before=6`,
	);
	expect(seqs(older)).toEqual([5, 4, 3, 2, 1]);
	for (const key of ["15125550101", "5125550101", "1", "+1 512 555 0101"])
		expect(
			seqs(
				await json<MessagePageView>(
					`/api/v1/numbers/${encodeURIComponent(key)}/messages?limit=1`,
				),
			),
		).toEqual([8]);
	for (const key of ["+15125550188", "99", "abc"]) {
		const response = await get(`/api/v1/numbers/${key}/messages`);
		expect(response.status).toBe(404);
		expect(await response.json()).toEqual({ error: `no number ${key}` });
	}
	expect((await get(`/api/v1/numbers/${NUMBER}/messages?wait=31`)).status).toBe(
		422,
	);
});

test("long-poll returns as soon as a text arrives, oldest first", async () => {
	expect(
		seqs(
			await json<MessagePageView>(`/api/v1/numbers/${NUMBER}/messages?after=6`),
		),
	).toEqual([7, 8]);
	const started = performance.now();
	const waiting = json<MessagePageView>(
		`/api/v1/numbers/${NUMBER}/messages?after=8&wait=10`,
	);
	await Bun.sleep(100);
	await arrive(12, "Still available?");
	const page = await waiting;
	expect(performance.now() - started).toBeLessThan(2_000);
	expect(seqs(page)).toEqual([9]);
	expect(page.cursor).toBe(9);
	expect(page.messages[0]?.body).toBe("Still available?");
});

test("long-poll times out cleanly, and ignores other numbers' texts", async () => {
	const started = performance.now();
	const waiting = json<MessagePageView>(
		`/api/v1/numbers/${NUMBER}/messages?after=8&wait=1`,
	);
	// A text from a phone with no known SIM: unassigned, so not this number's.
	db.run(
		"INSERT INTO phones (serial, site, last_state, first_seen_at) VALUES ('OTHER', 'home', 'device', '2026-10-22T00:00:00.000Z')",
	);
	const [seq] = insertMessages(
		db,
		2,
		[
			{
				provider: "sms",
				providerId: 1,
				subId: 1,
				sender: "+15125550157",
				body: "Wrong number",
				subject: null,
				sentAt: null,
				receivedAt: "2026-10-22T00:00:01.000Z",
				attachments: [],
			},
		],
		"2026-10-22T00:00:02.000Z",
	);
	messages.publish(store.get(seq as number) as Message);
	expect(await waiting).toEqual({ messages: [], cursor: 8 });
	const elapsed = performance.now() - started;
	expect(elapsed).toBeGreaterThanOrEqual(950);
	expect(elapsed).toBeLessThan(2_000);
	// It is in the all-numbers log, without a number.
	const all = await json<MessagePageView>("/api/v1/messages?after=8");
	expect(all.messages.map((m) => [m.seq, m.number])).toEqual([[9, null]]);
});

test("all messages: newest first, after, search; before with after is refused", async () => {
	expect(seqs(await json<MessagePageView>("/api/v1/messages?limit=2"))).toEqual(
		[8, 7],
	);
	const after = await json<MessagePageView>("/api/v1/messages?after=6&limit=1");
	expect(seqs(after)).toEqual([7]);
	expect(after.cursor).toBe(7);
	expect(
		seqs(await json<MessagePageView>("/api/v1/messages?q=fake%20text%203")),
	).toEqual([3]);
	expect(
		seqs(await json<MessagePageView>("/api/v1/messages?q=5550152")),
	).toEqual([7]);
	await app.handle(
		new Request("http://localhost/api/v1/numbers/1", {
			method: "PATCH",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({ note: "Facebook ad, blue couch" }),
		}),
	);
	const byNote = await json<MessagePageView>(
		"/api/v1/messages?q=facebook%20AD",
	);
	expect(seqs(byNote)).toHaveLength(8);
	expect(byNote.messages[0]?.number?.note).toBe("Facebook ad, blue couch");
	expect(
		seqs(await json<MessagePageView>("/api/v1/messages?q=craigslist")),
	).toEqual([]);
	expect(
		seqs(
			await json<MessagePageView>(
				`/api/v1/messages?q=${encodeURIComponent("(512) 555-0101")}`,
			),
		),
	).toHaveLength(8);
	for (const path of [
		"/api/v1/messages?before=5&after=2",
		`/api/v1/numbers/${NUMBER}/messages?before=5&after=2`,
	]) {
		const response = await get(path);
		expect(response.status).toBe(400);
		expect(await response.json()).toEqual({
			error: "use before or after, not both",
		});
	}
	expect((await get("/api/v1/messages/3")).status).toBe(404);
});

test("SSE opens with the current cursor, so a reconnect before any event resumes, not restarts", async () => {
	const lastDevice = Number(
		db
			.query<{ id: number }, []>("SELECT max(id) AS id FROM device_events")
			.get()?.id,
	);
	for (const [path, cursor] of [
		["/api/v1/events", String(lastDevice)],
		["/api/v1/events?types=message", "8"],
		["/api/v1/events?types=device,message", `${lastDevice}:8`],
	] as const) {
		const controller = new AbortController();
		const response = await app.handle(
			new Request(`http://localhost${path}`, { signal: controller.signal }),
		);
		const text = await readEvents(response, (t) => t.includes("\n\n"));
		controller.abort();
		expect(text).toStartWith(`retry: 3000\nid: ${cursor}\n\n`);
	}
	const bad = await get("/api/v1/events?types=device,foo");
	expect(bad.status).toBe(400);
});

test("attachments stream with their content type; scriptable types download sandboxed", async () => {
	const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
	const html = new TextEncoder().encode("<script>alert(1)</script>");
	const attachmentsDir = join(dir, "attachments");
	insertMessages(
		db,
		1,
		[
			{
				provider: "mms",
				providerId: 3,
				subId: 1,
				sender: "+15125550155",
				body: "",
				subject: null,
				sentAt: null,
				receivedAt: "2026-10-22T00:00:01.000Z",
				attachments: [
					{
						contentType: "image/png",
						filename: "a b.png",
						...saveAttachment(attachmentsDir, png),
					},
					{
						contentType: "text/html",
						filename: null,
						...saveAttachment(attachmentsDir, html),
					},
				],
			},
		],
		"2026-10-22T00:00:02.000Z",
	);
	const image = await get("/api/v1/attachments/1");
	expect(image.status).toBe(200);
	expect(image.headers.get("content-type")).toBe("image/png");
	expect(image.headers.get("content-disposition")).toBe(
		'inline; filename="a_b.png"',
	);
	expect(new Uint8Array(await image.arrayBuffer())).toEqual(png);
	const page = await get("/api/v1/attachments/2");
	expect(page.headers.get("content-disposition")).toBe(
		'attachment; filename="attachment-2"',
	);
	expect(page.headers.get("content-security-policy")).toContain("sandbox");
	expect((await get("/api/v1/attachments/9")).status).toBe(404);
});

async function readEvents(
	response: Response,
	until: (text: string) => boolean,
): Promise<string> {
	const reader = (response.body as ReadableStream<Uint8Array>).getReader();
	let text = "";
	while (!until(text)) {
		const { value, done } = await reader.read();
		if (done) break;
		text += new TextDecoder().decode(value);
	}
	await reader.cancel();
	return text;
}

test("SSE resumes from Last-Event-ID, then streams new texts with a heartbeat", async () => {
	const controller = new AbortController();
	const response = await app.handle(
		new Request("http://localhost/api/v1/events?types=message", {
			headers: { "last-event-id": "6" },
			signal: controller.signal,
		}),
	);
	expect(response.headers.get("content-type")).toBe("text/event-stream");
	const reading = readEvents(
		response,
		(text) => text.includes("id: 9\n") && text.includes(": keep-alive"),
	);
	await Bun.sleep(20);
	await arrive(12, "Hello again");
	const text = await reading;
	controller.abort();
	const ids = [...text.matchAll(/^id: (.+)\nevent:/gm)].map(
		(match) => match[1],
	);
	expect(ids).toEqual(["7", "8", "9"]);
	// The opening block carries the cursor after the replay.
	expect(text).toContain("retry: 3000\nid: 8\n\n");
	expect(text).toContain("event: message\ndata: {");
	expect(text).not.toContain("event: device");
	const last = text
		.split("\n\n")
		.find((block) => block.startsWith("id: 9\n"))
		?.split("data: ")[1];
	expect(JSON.parse(last ?? "{}")).toMatchObject({
		seq: 9,
		body: "Hello again",
		number: { e164: NUMBER },
	});
});

test("SSE with both streams uses '<deviceEventId>:<seq>' ids and resumes both without gaps", async () => {
	const lastDevice = Number(
		db
			.query<{ id: number }, []>("SELECT max(id) AS id FROM device_events")
			.get()?.id,
	);
	const idsOf = (text: string) =>
		[...text.matchAll(/^id: (.+)\nevent:/gm)].map((match) => match[1]);
	const open = async (path: string, headers: Record<string, string> = {}) => {
		const controller = new AbortController();
		const response = await app.handle(
			new Request(`http://localhost${path}`, {
				headers,
				signal: controller.signal,
			}),
		);
		return { response, close: () => controller.abort() };
	};

	const first = await open(
		`/api/v1/events?types=device,message&after=${lastDevice - 1}:7`,
	);
	const text = await readEvents(first.response, (t) =>
		t.includes(`id: ${lastDevice}:8`),
	);
	first.close();
	expect(idsOf(text)).toEqual([`${lastDevice}:7`, `${lastDevice}:8`]);

	// Reconnecting with the last id: only what came after, of both kinds.
	const resumed = await open("/api/v1/events?types=device,message", {
		"last-event-id": `${lastDevice}:8`,
	});
	const reading = readEvents(resumed.response, (t) =>
		t.includes(`id: ${lastDevice + 1}:9`),
	);
	await Bun.sleep(20);
	const [event] = insertEvents(
		db,
		[{ phoneId: null, kind: "adb.reachable", detail: {} }],
		"2026-10-22T09:00:00.000Z",
	);
	if (event) events.publish(event);
	await arrive(12, "After the reconnect");
	const resumedText = await reading;
	resumed.close();
	expect(idsOf(resumedText)).toEqual([
		`${lastDevice + 1}:8`,
		`${lastDevice + 1}:9`,
	]);

	// A bare number resumes device events; texts start with the next new one.
	const bare = await open("/api/v1/events?types=device,message", {
		"last-event-id": String(lastDevice - 1),
	});
	const oldText = await readEvents(bare.response, (t) =>
		t.includes(": keep-alive"),
	);
	bare.close();
	expect(idsOf(oldText)).toEqual([`${lastDevice}:9`, `${lastDevice + 1}:9`]);

	// Without types the stream is device events only.
	const plain = await open("/api/v1/events", {
		"last-event-id": String(lastDevice),
	});
	const plainText = await readEvents(plain.response, (t) =>
		t.includes(": keep-alive"),
	);
	plain.close();
	expect(idsOf(plainText)).toEqual([String(lastDevice + 1)]);
});

test("OpenAPI documents the message routes with examples", async () => {
	const spec = (await (await get("/api/docs/json")).json()) as {
		paths: Record<string, Record<string, unknown>>;
	};
	for (const path of [
		"/api/v1/numbers/{key}/messages",
		"/api/v1/messages",
		"/api/v1/attachments/{id}",
		"/api/v1/events",
	])
		expect(Object.keys(spec.paths)).toContain(path);
	expect(JSON.stringify(spec.paths["/api/v1/messages"])).toContain(
		"Is the couch still available?",
	);
});
