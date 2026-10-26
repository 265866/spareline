import type { Database } from "bun:sqlite";
import { beforeEach, expect, test } from "bun:test";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { FakeAdb } from "../../test/fake-adb";
import { FakePhone } from "../../test/fake-scrcpy";
import { type App, createApp } from "../api/app";
import { Bus } from "../bus";
import { openDb } from "../db/db";
import { DeviceManager, type Devices } from "../devices/manager";
import { Inventory } from "../inventory/store";
import { silent } from "../log";
import { Monitor } from "../monitor/monitor";
import { ScreenManager } from "../screens/manager";
import { MessageStore } from "../sms/store";
import { Auth, SESSION_COOKIE } from "./auth";
import { LoginLimiter } from "./limiter";

// The real app with every route mounted (screens and the monitor included),
// over an in-memory database. No phone is attached: these tests are about who
// may call what.

let db: Database;
let auth: Auth;
let app: App;
let password: string;

beforeEach(async () => {
	db = openDb(":memory:");
	auth = new Auth(db);
	({ password } = await auth.addUser("alice"));
	const adb = await FakeAdb.b30();
	const devices = new DeviceManager({
		adb,
		db,
		site: "test",
		events: new Bus(),
		log: silent,
	});
	const webDir = mkdtempSync(join(tmpdir(), "spareline-auth-web-"));
	writeFileSync(join(webDir, "index.html"), "<!doctype html><div id=root>");
	app = createApp({
		auth,
		inventory: new Inventory(db),
		devices,
		events: new Bus(),
		messages: {
			store: new MessageStore(db),
			bus: new Bus(),
			attachmentsDir: join(webDir, "attachments"),
		},
		monitor: new Monitor({
			devices,
			db,
			events: new Bus(),
			log: silent,
			policy: null,
		}),
		screens: new ScreenManager({
			devices: new FakePhone() as unknown as Devices,
			db,
			jar: new Uint8Array([1]),
			log: silent,
		}),
		webDir,
		version: "test",
	});
});

const call = (
	method: string,
	path: string,
	options: { body?: unknown; headers?: Record<string, string> } = {},
) =>
	app.handle(
		new Request(`http://localhost${path}`, {
			method,
			headers:
				options.body === undefined
					? options.headers
					: { "content-type": "application/json", ...options.headers },
			body:
				options.body === undefined ? undefined : JSON.stringify(options.body),
		}),
	);

const signIn = async (name = "alice", secret = password) => {
	const response = await call("POST", "/api/v1/auth/login", {
		body: { name, password: secret },
	});
	const cookie = response.headers.get("set-cookie") ?? "";
	return { response, cookie, session: cookie.split(";")[0] ?? "" };
};

test("every API route but sign-in, who-am-I, and the docs refuses a request without a session", async () => {
	const open = new Set([
		"/api/v1/auth/login",
		"/api/v1/auth/me",
		"/api/docs",
		"/api/docs/json",
	]);
	let checked = 0;
	for (const route of app.routes) {
		if (!route.path.startsWith("/api/") || open.has(route.path)) continue;
		const path = route.path.replace(/:\w+/g, "x").replace("*", "x");
		const method = route.method === "WS" ? "GET" : route.method;
		const response = await call(method, path, {
			headers:
				route.method === "WS"
					? {
							Connection: "Upgrade",
							Upgrade: "websocket",
							"Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
							"Sec-WebSocket-Version": "13",
						}
					: { "content-type": "application/json" },
		});
		expect([route.method, route.path, response.status]).toEqual([
			route.method,
			route.path,
			401,
		]);
		checked++;
	}
	// Phones, SIMs, numbers, texts, events, screens, status, and set-up.
	expect(checked).toBeGreaterThan(20);
	// The public ones answer.
	expect((await call("GET", "/healthz")).status).toBe(200);
	expect((await call("GET", "/api/docs/json")).status).toBe(200);
	expect((await call("GET", "/numbers/1")).status).toBe(200);
});

test("signing in sets an HttpOnly, SameSite=Strict cookie that opens the API", async () => {
	const { response, cookie, session } = await signIn();
	expect(response.status).toBe(200);
	expect(await response.json()).toEqual({ user: { name: "alice" } });
	expect(cookie).toContain(`${SESSION_COOKIE}=`);
	expect(cookie).toContain("HttpOnly");
	expect(cookie).toContain("SameSite=Strict");
	expect(cookie).toContain("Path=/");
	expect(cookie).toContain(`Max-Age=${30 * 86_400}`);
	// Plain HTTP (localhost, or a proxy that did not say otherwise) gets no
	// Secure flag, or the browser would drop the cookie.
	expect(cookie).not.toContain("Secure");
	const phones = await call("GET", "/api/v1/phones", {
		headers: { cookie: session },
	});
	expect(phones.status).toBe(200);
	const me = await call("GET", "/api/v1/auth/me", {
		headers: { cookie: session },
	});
	expect(await me.json()).toEqual({ user: { name: "alice" } });
});

test("behind an HTTPS proxy the cookie is Secure", async () => {
	const response = await call("POST", "/api/v1/auth/login", {
		body: { name: "alice", password },
		headers: { "x-forwarded-proto": "https" },
	});
	expect(response.headers.get("set-cookie")).toContain("; Secure");
});

test("a wrong password, an unknown name, and a form post are refused alike", async () => {
	for (const [name, secret] of [
		["alice", "wrong password"],
		["mallory", password],
	] as const) {
		const { response, cookie } = await signIn(name, secret);
		expect(response.status).toBe(401);
		expect(await response.json()).toEqual({ error: "wrong name or password" });
		expect(cookie).toBe("");
	}
	const form = await app.handle(
		new Request("http://localhost/api/v1/auth/login", {
			method: "POST",
			headers: { "content-type": "application/x-www-form-urlencoded" },
			body: `name=alice&password=${encodeURIComponent(password)}`,
		}),
	);
	expect(form.status).toBe(415);
	expect(form.headers.get("set-cookie")).toBeNull();
});

test("ten failed sign-ins lock the name out for a while, even with the right password", async () => {
	for (let i = 0; i < 10; i++)
		expect((await signIn("alice", "wrong")).response.status).toBe(401);
	const locked = await signIn();
	expect(locked.response.status).toBe(429);
	expect(Number(locked.response.headers.get("retry-after"))).toBeGreaterThan(0);
	expect(locked.cookie).toBe("");
});

test("who-am-I says when nobody can sign in yet", async () => {
	const me = await call("GET", "/api/v1/auth/me");
	expect(me.status).toBe(401);
	expect(await me.json()).toEqual({ error: "sign in first", noUsers: false });
	auth.removeUser("alice");
	const none = await call("GET", "/api/v1/auth/me");
	expect(await none.json()).toMatchObject({ noUsers: true });
});

test("signing out ends the session and clears the cookie", async () => {
	const { session } = await signIn();
	const out = await call("POST", "/api/v1/auth/logout", {
		body: {},
		headers: { cookie: session },
	});
	expect(out.status).toBe(200);
	expect(out.headers.get("set-cookie")).toContain("Max-Age=0");
	const after = await call("GET", "/api/v1/phones", {
		headers: { cookie: session },
	});
	expect(after.status).toBe(401);
});

test("changing the password needs the current one and keeps only this session", async () => {
	const here = (await signIn()).session;
	const there = (await signIn()).session;
	const change = (body: unknown) =>
		call("POST", "/api/v1/auth/password", { body, headers: { cookie: here } });
	expect(
		(await change({ current: "wrong", next: "a long new password" })).status,
	).toBe(403);
	const short = await change({ current: password, next: "short" });
	expect(short.status).toBe(400);
	expect(await short.json()).toEqual({
		error: "a password needs at least 12 characters",
	});
	expect(
		(await change({ current: password, next: "a long new password" })).status,
	).toBe(200);
	expect(
		(await call("GET", "/api/v1/phones", { headers: { cookie: here } })).status,
	).toBe(200);
	expect(
		(await call("GET", "/api/v1/phones", { headers: { cookie: there } }))
			.status,
	).toBe(401);
	expect((await signIn("alice", "a long new password")).response.status).toBe(
		200,
	);
});

test("the limiter is per name and per address", async () => {
	const limiter = new LoginLimiter(2, 60_000);
	limiter.fail(["name:a", "ip:1"]);
	limiter.fail(["name:b", "ip:1"]);
	expect(limiter.retryAfter(["name:c", "ip:1"])).not.toBeNull();
	expect(limiter.retryAfter(["name:c", "ip:2"])).toBeNull();
});
