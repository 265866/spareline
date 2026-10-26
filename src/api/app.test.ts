import type { Database } from "bun:sqlite";
import { afterEach, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FakeAdb, recordedTrackLists } from "../../test/fake-adb";
import { signedIn, testSession } from "../../test/session";
import { Bus } from "../bus";
import { openDb } from "../db/db";
import { DeviceManager } from "../devices/manager";
import { Inventory } from "../inventory/store";
import { type DeviceEventRow, insertEvents } from "../inventory/sync";
import { silent } from "../log";
import { MessageStore } from "../sms/store";
import { type App, createApp } from "./app";
import type { NumberView, PhoneView } from "./schema";

// Real inbound device states (a phone being plugged in, a SIM appearing)
// cannot be produced on demand, so DeviceManager runs against FakeAdb, which
// replays the adb server's recorded device lists and the B30's recorded
// probe output. Everything else, from the database to HTTP, is real.

const B30 = "SPARELINE00001";
let db: Database;
let adb: FakeAdb;
let manager: DeviceManager;
let events: Bus<DeviceEventRow>;
let app: App;
let webDir: string;
let session: ReturnType<typeof testSession>;

beforeEach(async () => {
	db = openDb(":memory:");
	adb = await FakeAdb.b30();
	events = new Bus();
	manager = new DeviceManager({
		adb,
		db,
		site: "home",
		events,
		log: silent,
		identityRetryMs: [],
	});
	webDir = mkdtempSync(join(tmpdir(), "spareline-web-"));
	mkdirSync(join(webDir, "assets"));
	writeFileSync(
		join(webDir, "index.html"),
		"<!doctype html><div id=root></div>",
	);
	writeFileSync(join(webDir, "assets", "main-abc123.js"), "console.log(1)");
	session = testSession(db);
	app = signedIn(
		createApp({
			auth: session.auth,
			inventory: new Inventory(db),
			devices: manager,
			events,
			messages: {
				store: new MessageStore(db),
				bus: new Bus(),
				attachmentsDir: join(webDir, "attachments"),
			},
			webDir,
			version: "test",
			heartbeatMs: 50,
		}),
		session.cookie,
	);
	manager.start();
	adb.send((await recordedTrackLists())[0] ?? []);
	for (let i = 0; i < 200 && !manager.get(B30)?.healthAt; i++)
		await Bun.sleep(5);
	await manager.settled();
});
afterEach(async () => {
	await manager.stop();
	rmSync(webDir, { recursive: true, force: true });
});

const call = (
	method: string,
	path: string,
	body?: unknown,
	headers: Record<string, string> = {},
) =>
	app.handle(
		new Request(`http://localhost${path}`, {
			method,
			headers:
				body === undefined
					? headers
					: { "content-type": "application/json", ...headers },
			body: body === undefined ? undefined : JSON.stringify(body),
		}),
	);
const json = async <T>(response: Response | Promise<Response>): Promise<T> =>
	(await response).json() as Promise<T>;

test("healthz says only whether adb is reachable, without a session", async () => {
	const response = await call("GET", "/healthz", undefined, { cookie: "" });
	expect(response.status).toBe(200);
	expect(await response.json()).toEqual({ ok: true });
});

test("health summarizes adb and phones for a signed-in user", async () => {
	expect(
		(await call("GET", "/api/v1/health", undefined, { cookie: "" })).status,
	).toBe(401);
	const response = await call("GET", "/api/v1/health");
	expect(response.status).toBe(200);
	expect(await response.json()).toEqual({
		ok: true,
		adbReachable: true,
		phonesOnline: 1,
		phonesOffline: 0,
		alerts: 0,
		version: "test",
	});
});

test("phones show inventory joined with live state", async () => {
	const { phones } = await json<{ phones: PhoneView[] }>(
		call("GET", "/api/v1/phones"),
	);
	expect(phones).toHaveLength(1);
	const phone = phones[0] as PhoneView;
	expect(phone).toMatchObject({
		serial: B30,
		model: "S6702X",
		androidVersion: "14",
		imeis: ["350000000000006", "350000000000014"],
		status: "online",
		adbState: "device",
		battery: {
			level: 85,
			temperatureC: 22.1,
			status: "not_charging",
			plugged: ["usb"],
		},
	});
	expect(phone.slots).toEqual([
		{
			slot: 0,
			simState: "LOADED",
			radio: expect.objectContaining({
				voice: "out_of_service",
				signalLevel: 1,
				signalDbm: -122,
			}),
			sim: {
				id: 1,
				iccid: "8901555000000000001",
				carrier: "T-Mobile",
				subId: 1,
			},
			number: {
				id: 1,
				e164: "+15125550101",
				source: "detected",
				note: "",
			},
		},
		{
			slot: 1,
			simState: "ABSENT",
			radio: expect.objectContaining({ voice: "power_off" }),
			sim: null,
			number: null,
		},
	]);
	expect((await call("GET", "/api/v1/phones/NOPE")).status).toBe(404);
});

test("editing a phone records who signed in", async () => {
	const response = await call("PATCH", `/api/v1/phones/${B30}`, {
		location: "A3",
	});
	expect(response.status).toBe(200);
	const phone = (await response.json()) as PhoneView;
	expect(phone.location).toBe("A3");
	expect(db.query("SELECT login, entity FROM inventory_edits").all()).toEqual([
		{ login: "owner@example.com", entity: "phone" },
	]);
	// Unknown fields and wrong types are rejected.
	for (const body of [{ nickname: "x" }, { costCents: -5 }])
		expect((await call("PATCH", `/api/v1/phones/${B30}`, body)).status).toBe(
			422,
		);
});

test("numbers: detected, overridden by hand, conflicts, and cleared", async () => {
	const { numbers } = await json<{ numbers: NumberView[] }>(
		call("GET", "/api/v1/numbers"),
	);
	expect(numbers).toEqual([
		expect.objectContaining({
			id: 1,
			e164: "+15125550101",
			source: "detected",
			phone: { serial: B30, model: "S6702X", slot: 0, removedAt: null },
			phoneStatus: "online",
			radio: expect.objectContaining({ slot: 0, voice: "out_of_service" }),
		}),
	]);
	expect((await call("GET", "/api/v1/numbers/+15125550101")).status).toBe(200);

	const set = await call("PATCH", "/api/v1/numbers/1", {
		e164: "(512) 555-0199",
		note: "Craigslist",
	});
	expect(set.status).toBe(200);
	expect(await set.json()).toMatchObject({
		e164: "+15125550199",
		source: "manual",
		detectedE164: "+15125550101",
		note: "Craigslist",
	});
	expect(
		(await call("PATCH", "/api/v1/numbers/1", { e164: "call me" })).status,
	).toBe(400);
	// A country code cannot start with 0; it would otherwise reach the
	// database CHECK constraint and fail with a 500.
	expect(
		(await call("PATCH", "/api/v1/numbers/1", { e164: "+0123456789" })).status,
	).toBe(400);
	expect(
		(await json<NumberView>(call("PATCH", "/api/v1/numbers/1", { e164: null })))
			.e164,
	).toBe("+15125550101");
});

test("a number's note is edited alone and shows wherever the number does", async () => {
	const response = await call("PATCH", "/api/v1/numbers/+15125550101", {
		note: "Facebook ad, blue couch",
	});
	expect(response.status).toBe(200);
	expect(await response.json()).toMatchObject({
		e164: "+15125550101",
		source: "detected",
		note: "Facebook ad, blue couch",
	});
	expect((await json<NumberView>(call("GET", "/api/v1/numbers/1"))).note).toBe(
		"Facebook ad, blue couch",
	);
	const { phones } = await json<{ phones: PhoneView[] }>(
		call("GET", "/api/v1/phones"),
	);
	expect(phones[0]?.slots[0]?.number?.note).toBe("Facebook ad, blue couch");
	expect(
		db.query("SELECT login, entity, changes_json FROM inventory_edits").all(),
	).toEqual([
		{
			login: "owner@example.com",
			entity: "number",
			changes_json: '{"note":"Facebook ad, blue couch"}',
		},
	]);
	// Clearing works; oversized notes are refused.
	expect(
		(await json<NumberView>(call("PATCH", "/api/v1/numbers/1", { note: "" })))
			.note,
	).toBe("");
	expect(
		(await call("PATCH", "/api/v1/numbers/1", { note: "x".repeat(2001) }))
			.status,
	).toBe(422);
});

test("sims by id or ICCID", async () => {
	const sim = await json<{ id: number; placement: unknown }>(
		call("GET", "/api/v1/sims/8901555000000000001"),
	);
	expect(sim).toMatchObject({
		id: 1,
		placement: { serial: B30, slot: 0, subId: 1 },
	});
	expect(
		await json(call("PATCH", "/api/v1/sims/1", { status: "spare" })),
	).toMatchObject({ status: "spare" });
	for (const body of [{ status: "lost" }, { note: "prepaid" }])
		expect((await call("PATCH", "/api/v1/sims/1", body)).status).toBe(422);
});

test("status shows live adb state", async () => {
	expect(await json(call("GET", "/api/v1/status"))).toMatchObject({
		adb: { reachable: true, version: 41 },
		phones: [
			{
				serial: B30,
				status: "online",
				adbState: "device",
				usb: "1-1",
				simStates: ["LOADED", "ABSENT"],
			},
		],
	});
});

async function openEvents(path: string, headers: Record<string, string> = {}) {
	const controller = new AbortController();
	const response = await app.handle(
		new Request(`http://localhost${path}`, {
			headers,
			signal: controller.signal,
		}),
	);
	expect(response.headers.get("content-type")).toBe("text/event-stream");
	const reader = (response.body as ReadableStream<Uint8Array>).getReader();
	let text = "";
	return {
		async readUntil(needle: string) {
			while (!text.includes(needle)) {
				const { value, done } = await reader.read();
				if (done) throw new Error(`stream ended before ${needle}`);
				text += new TextDecoder().decode(value);
			}
		},
		ids: () =>
			// Event ids only, not the cursor in the opening retry block.
			[...text.matchAll(/^id: (\d+)\nevent:/gm)].map((match) =>
				Number(match[1]),
			),
		text: () => text,
		async close() {
			controller.abort();
			await reader.cancel();
		},
	};
}

function storeEvents(count: number, kind = "adb.reachable"): DeviceEventRow[] {
	return insertEvents(
		db,
		Array.from({ length: count }, (_, i) => ({
			phoneId: null,
			kind,
			detail: { i },
		})),
		"2026-10-22T00:00:00.000Z",
	);
}

const lastId = (rows: DeviceEventRow[]) => rows.at(-1)?.id ?? 0;

test("events stream replays stored events, then new ones", async () => {
	const stream = await openEvents("/api/v1/events?after=0");
	await stream.readUntil("number.bound");
	expect(stream.text()).toContain('"kind":"phone.added"');
	expect(stream.text()).toContain("id: 1\nevent: device\n");
	adb.send([
		{
			serial: "NEWPHONE",
			state: "unauthorized",
			rawState: "unauthorized",
			transportId: 3,
		},
	]);
	await stream.readUntil("phone.needs_auth");
	await stream.readUntil(": keep-alive");
	await stream.close();
});

test("after= replays every later event in order, not just the newest page", async () => {
	const last = lastId(storeEvents(150));
	const stream = await openEvents("/api/v1/events?after=10");
	await stream.readUntil(`id: ${last}\n`);
	expect(stream.ids()).toEqual(
		Array.from({ length: last - 10 }, (_, i) => 11 + i),
	);
	await stream.close();
});

test("latest= replays the newest events, and Last-Event-ID resumes after its id", async () => {
	const last = lastId(storeEvents(30));
	const latest = await openEvents("/api/v1/events?latest=3");
	await latest.readUntil(`id: ${last}\n`);
	expect(latest.ids()).toEqual([last - 2, last - 1, last]);
	await latest.close();

	const resumed = await openEvents("/api/v1/events?latest=3", {
		"Last-Event-ID": String(last - 5),
	});
	await resumed.readUntil(`id: ${last}\n`);
	expect(resumed.ids()).toEqual([last - 4, last - 3, last - 2, last - 1, last]);
	await resumed.close();

	// With neither, only events published after connecting arrive.
	const live = await openEvents("/api/v1/events");
	const [fresh] = storeEvents(1, "adb.unreachable");
	if (fresh) events.publish(fresh);
	await live.readUntil("adb.unreachable");
	expect(live.ids()).toEqual([last + 1]);
	await live.close();
});

test("an asset path that is a directory is a 404, not a server error", async () => {
	expect((await call("GET", "/assets/")).status).toBe(404);
	mkdirSync(join(webDir, "assets", "nested"));
	expect((await call("GET", "/assets/nested")).status).toBe(404);
});

test("the SPA is served for client routes, assets are immutable, and paths stay inside", async () => {
	const page = await call("GET", "/numbers/1");
	expect(page.status).toBe(200);
	expect(await page.text()).toContain("id=root");
	const asset = await call("GET", "/assets/main-abc123.js");
	expect(asset.headers.get("cache-control")).toContain("immutable");
	expect((await call("GET", "/assets/..%2F..%2Fpackage.json")).status).toBe(
		404,
	);
	expect((await call("GET", "/api/v1/nothing")).status).toBe(404);
	expect((await call("GET", "/api/docs")).status).toBe(200);
});

test("an edit made with an API key records the user and the key", async () => {
	const user = session.auth.findUser(session.name);
	if (!user) throw new Error("no test user");
	const { key } = session.auth.createKey(user.id, "inventory script", "full");
	const response = await call(
		"PATCH",
		`/api/v1/phones/${B30}`,
		{ location: "B1" },
		{ authorization: `Bearer ${key}`, cookie: "" },
	);
	expect(response.status).toBe(200);
	expect(
		db
			.query("SELECT login FROM inventory_edits ORDER BY id DESC LIMIT 1")
			.get(),
	).toEqual({ login: "owner@example.com (key inventory script)" });
});
