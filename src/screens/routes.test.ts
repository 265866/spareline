import type { Database } from "bun:sqlite";
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FakePhone } from "../../test/fake-scrcpy";
import { createApp } from "../api/app";
import { Bus } from "../bus";
import { openDb } from "../db/db";
import type { Devices } from "../devices/manager";
import { Inventory } from "../inventory/store";
import { silent } from "../log";
import { MessageStore } from "../sms/store";
import { ScreenManager } from "./manager";
import { MAX_VIEWER_MESSAGE_BYTES } from "./protocol";
import { sameOrigin } from "./routes";

// The screen routes inside the real app, over a real WebSocket. The phone
// side is the stand-in from test/fake-scrcpy.ts (see there for why).

const SERIAL = "SPARELINE00001";
let db: Database;
let phone: FakePhone;
let screens: ScreenManager;
let server: ReturnType<ReturnType<typeof createApp>["listen"]>;
let base: string;
let webDir: string;

beforeEach(() => {
	db = openDb(":memory:");
	db.run(
		"INSERT INTO phones (serial, site, last_state, first_seen_at) VALUES (?, 'test', 'device', '2026-10-22T00:00:00Z')",
		[SERIAL],
	);
	phone = new FakePhone();
	screens = new ScreenManager({
		devices: phone,
		db,
		jar: new Uint8Array([1]),
		log: silent,
		statsIntervalMs: 60_000,
		session: { resetIntervalMs: 0 },
	});
	// A built UI, so the SPA's catch-all route is present as in production.
	webDir = mkdtempSync(join(tmpdir(), "spareline-web-"));
	writeFileSync(join(webDir, "index.html"), "<!doctype html>");
	server = createApp({
		inventory: new Inventory(db),
		devices: phone as unknown as Devices,
		events: new Bus(),
		messages: {
			store: new MessageStore(db),
			bus: new Bus(),
			attachmentsDir: join(webDir, "attachments"),
		},
		webDir,
		version: "test",
		screens,
	}).listen({ hostname: "127.0.0.1", port: 0 });
	base = `127.0.0.1:${server.server?.port}`;
});

afterEach(async () => {
	await screens.stop();
	await server.stop();
	db.close();
	rmSync(webDir, { recursive: true, force: true });
});

type Received = { text: Record<string, unknown>[]; binary: Uint8Array[] };

function connect(path: string, headers: Record<string, string> = {}) {
	const received: Received = { text: [], binary: [] };
	// Bun's WebSocket client accepts headers, which browsers do not.
	const ws = new WebSocket(`ws://${base}${path}`, {
		headers,
	} as unknown as string[]);
	ws.binaryType = "arraybuffer";
	ws.onmessage = (event) => {
		if (typeof event.data === "string")
			received.text.push(JSON.parse(event.data));
		else received.binary.push(new Uint8Array(event.data as ArrayBuffer));
	};
	const closed = new Promise<CloseEvent>((resolve) => {
		ws.onclose = resolve;
	});
	return { ws, received, closed };
}

async function until(check: () => boolean, ms = 2_000): Promise<void> {
	const deadline = Date.now() + ms;
	while (!check()) {
		if (Date.now() > deadline) throw new Error("timed out waiting");
		await Bun.sleep(5);
	}
}

test("a viewer gets hello, then scrcpy packets as binary frames", async () => {
	const { ws, received } = connect(`/api/v1/phones/${SERIAL}/screen`, {
		"Tailscale-User-Login": "someone@example.com",
	});
	await until(() => received.binary.length > 2);
	expect(received.text).toContainEqual(
		expect.objectContaining({ t: "hello", width: 216, height: 480 }),
	);
	// The first binary frame is the config packet, header and all.
	expect(received.binary[0]?.[0]).toBe(0x80);

	ws.send(new Uint8Array([9, 0])); // clipboard: refused
	ws.send(new Uint8Array([10, 1])); // display power on: refused
	ws.send(new Uint8Array([4, 0])); // back or screen on: allowed
	await until(
		() =>
			phone.servers[0]?.control.some((m) => m[0] === 4 && m.length === 2) ??
			false,
	);
	expect(phone.servers[0]?.control).not.toContainEqual(new Uint8Array([9, 0]));
	expect(phone.servers[0]?.control).not.toContainEqual(new Uint8Array([10, 1]));

	const sessions = (await (
		await fetch(`http://${base}/api/v1/screens`)
	).json()) as { sessions: unknown[] };
	expect(sessions.sessions).toEqual([
		expect.objectContaining({
			serial: SERIAL,
			state: "running",
			viewers: 1,
		}),
	]);

	ws.close();
	await until(
		() =>
			(
				db.query("SELECT ended_at FROM control_sessions").get() as {
					ended_at: string | null;
				} | null
			)?.ended_at != null,
	);
	expect(db.query("SELECT login FROM control_sessions").get()).toEqual({
		login: "someone@example.com",
	});
	expect(db.query("SELECT rejected FROM control_sessions").get()).toEqual({
		rejected: 2,
	});
});

describe("Origin", () => {
	const path = `/api/v1/phones/${SERIAL}/screen`;
	const upgrade = (headers: Record<string, string>) =>
		fetch(`http://${base}${path}`, {
			headers: {
				Connection: "Upgrade",
				Upgrade: "websocket",
				"Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
				"Sec-WebSocket-Version": "13",
				...headers,
			},
		});

	test("a page from another site is refused before the upgrade", async () => {
		const response = await upgrade({ Origin: "https://evil.example" });
		expect(response.status).toBe(403);
		// Same host name on another port is another origin.
		const port = await upgrade({ Origin: "http://127.0.0.1:1" });
		expect(port.status).toBe(403);
		expect(phone.servers).toEqual([]);
	});

	test("the app's own pages and clients without Origin are allowed", async () => {
		const own = connect(path, { Origin: `http://${base}` });
		await until(() => own.received.text.some((m) => m.t === "hello"));
		const script = connect(path);
		await until(() => script.received.text.some((m) => m.t === "hello"));
	});

	test("sameOrigin compares the Origin's host and port with Host", () => {
		expect(sameOrigin(null, "a.example")).toBe(true);
		expect(sameOrigin("https://a.example", "a.example")).toBe(true);
		expect(sameOrigin("https://a.example:10000", "a.example:10000")).toBe(true);
		expect(sameOrigin("https://A.example", "a.EXAMPLE")).toBe(true);
		expect(sameOrigin("https://a.example", "a.example:10000")).toBe(false);
		expect(sameOrigin("https://b.example", "a.example")).toBe(false);
		expect(sameOrigin("null", "a.example")).toBe(false);
		expect(sameOrigin("https://a.example", null)).toBe(false);
	});
});

test("a viewer message over 4 KiB closes the socket", async () => {
	const { ws, received, closed } = connect(`/api/v1/phones/${SERIAL}/screen`);
	await until(() => received.text.some((m) => m.t === "hello"));
	ws.send(new Uint8Array(MAX_VIEWER_MESSAGE_BYTES + 1));
	const event = await closed;
	expect(event.code).not.toBe(1000);
});

test("viewers arriving while the server shuts down are closed with 1001", async () => {
	await screens.stop();
	const { closed } = connect(`/api/v1/phones/${SERIAL}/screen`);
	expect((await closed).code).toBe(1001);
});

test("an offline phone's screen closes with 4001 and a reason", async () => {
	phone.online = false;
	const { closed } = connect(`/api/v1/phones/${SERIAL}/screen`);
	const event = await closed;
	expect(event.code).toBe(4001);
	expect(event.reason).toContain("not online");
});

test("a plain GET, without a WebSocket upgrade, starts nothing", async () => {
	const response = await fetch(`http://${base}/api/v1/phones/${SERIAL}/screen`);
	expect(response.ok).toBe(false);
	expect(phone.servers).toEqual([]);
});
