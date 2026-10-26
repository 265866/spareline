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

// The real app with every route mounted (screens and the monitor included),
// over an in-memory database. No phone is attached: these tests are about who
// may call what.

let db: Database;
let auth: Auth;
const clock = { now: new Date() };
let app: App;
let password: string;

beforeEach(async () => {
	db = openDb(":memory:");
	clock.now = new Date();
	auth = new Auth(db, () => clock.now);
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
	// The guard only covers /api/, so any other route must be one of these
	// three, all of them public by design.
	expect(
		app.routes
			.filter((route) => !route.path.startsWith("/api/"))
			.map((route) => `${route.method} ${route.path}`)
			.sort(),
	).toEqual(["GET /*", "GET /assets/*", "GET /healthz"]);
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

test("concurrent wrong passwords cannot get past the limit, and one name does not lock out another", async () => {
	const results = await Promise.all(
		Array.from({ length: 30 }, () => signIn("alice", "wrong")),
	);
	const statuses = results.map((result) => result.response.status);
	expect(statuses.filter((status) => status === 401)).toHaveLength(10);
	expect(statuses.filter((status) => status === 429)).toHaveLength(20);
	const { password: bobs } = await auth.addUser("bob");
	expect((await signIn("bob", bobs)).response.status).toBe(200);
});

test("the current-password check is rate limited too", async () => {
	const { session } = await signIn();
	const change = () =>
		call("POST", "/api/v1/auth/password", {
			body: { current: "wrong", next: "a long new password" },
			headers: { cookie: session },
		});
	for (let i = 0; i < 10; i++) expect((await change()).status).toBe(403);
	expect((await change()).status).toBe(429);
});

test("a session used after an hour gets its cookie renewed for 30 more days", async () => {
	const { session } = await signIn();
	const first = await call("GET", "/api/v1/phones", {
		headers: { cookie: session },
	});
	expect(first.headers.get("set-cookie")).toBeNull();
	clock.now = new Date(clock.now.getTime() + 2 * 3_600_000);
	const later = await call("GET", "/api/v1/phones", {
		headers: { cookie: session },
	});
	expect(later.status).toBe(200);
	expect(later.headers.get("set-cookie")).toBe(
		`${session}; Path=/; HttpOnly; SameSite=Strict; Max-Age=${30 * 86_400}`,
	);
});

test("an open event stream ends when its session ends", async () => {
	const { session } = await signIn();
	const server = app.listen({ hostname: "127.0.0.1", port: 0 });
	try {
		const base = `http://127.0.0.1:${server.server?.port}`;
		const stream = await fetch(`${base}/api/v1/events?types=device,message`, {
			headers: { cookie: session },
		});
		expect(stream.status).toBe(200);
		const reader = stream.body?.getReader();
		const ended = (async () => {
			for (;;) if ((await reader?.read())?.done ?? true) return true;
		})();
		const logout = await fetch(`${base}/api/v1/auth/logout`, {
			method: "POST",
			headers: { cookie: session, "content-type": "application/json" },
			body: "{}",
		});
		expect(logout.status).toBe(200);
		expect(
			await Promise.race([ended, Bun.sleep(2_000).then(() => false)]),
		).toBe(true);
	} finally {
		await server.stop(true);
	}
});

test("a session removed by another process ends open streams within seconds", async () => {
	const { session } = await signIn();
	const server = app.listen({ hostname: "127.0.0.1", port: 0 });
	try {
		const base = `http://127.0.0.1:${server.server?.port}`;
		const stream = await fetch(`${base}/api/v1/events`, {
			headers: { cookie: session },
		});
		const reader = stream.body?.getReader();
		const ended = (async () => {
			for (;;) if ((await reader?.read())?.done ?? true) return true;
		})();
		// What the CLI does from its own process: no notification here.
		db.run("DELETE FROM sessions");
		expect(
			await Promise.race([ended, Bun.sleep(7_000).then(() => false)]),
		).toBe(true);
	} finally {
		await server.stop(true);
	}
}, 10_000);

const createKey = async (
	session: string,
	name: string,
	scope: "read" | "full",
) => {
	const response = await call("POST", "/api/v1/api-keys", {
		body: { name, scope },
		headers: { cookie: session },
	});
	expect(response.status).toBe(200);
	return ((await response.json()) as { key: string }).key;
};
const bearer = (key: string) => ({ authorization: `Bearer ${key}` });

test("an API key is shown once, listed only by its prefix, and opens the API", async () => {
	const { session } = await signIn();
	const key = await createKey(session, "texts script", "full");
	expect(key).toMatch(/^sl_[\w-]{43}$/);
	const list = await call("GET", "/api/v1/api-keys", {
		headers: { cookie: session },
	});
	const { keys } = (await list.json()) as {
		keys: { name: string; prefix: string; user: string; scope: string }[];
	};
	expect(keys).toMatchObject([
		{
			name: "texts script",
			user: "alice",
			scope: "full",
			prefix: key.slice(0, 9),
		},
	]);
	expect(JSON.stringify(keys)).not.toContain(key.slice(9));
	expect(
		(await call("GET", "/api/v1/phones", { headers: bearer(key) })).status,
	).toBe(200);
	expect(
		(await call("GET", "/api/v1/messages", { headers: bearer(key) })).status,
	).toBe(200);
	expect(
		db.query("SELECT last_used_at IS NOT NULL AS used FROM api_keys").get(),
	).toEqual({ used: 1 });
});

test("a read key may only read; neither kind may manage keys or passwords", async () => {
	const { session } = await signIn();
	const read = await createKey(session, "reader", "read");
	const full = await createKey(session, "writer", "full");
	const readOnly = { "content-type": "application/json", ...bearer(read) };
	for (const route of app.routes) {
		if (
			!route.path.startsWith("/api/v1/") ||
			route.path.startsWith("/api/v1/auth/")
		)
			continue;
		const path = route.path.replace(/:\w+/g, "x").replace("*", "x");
		const response = await call(
			route.method === "WS" ? "GET" : route.method,
			path,
			{
				headers:
					route.method === "WS"
						? {
								...bearer(read),
								Connection: "Upgrade",
								Upgrade: "websocket",
								"Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
								"Sec-WebSocket-Version": "13",
							}
						: readOnly,
			},
		);
		const refused =
			route.method !== "GET" || route.path.startsWith("/api/v1/api-keys");
		expect([route.method, route.path, response.status === 403]).toEqual([
			route.method,
			route.path,
			refused,
		]);
	}
	for (const key of [read, full]) {
		const mint = await call("POST", "/api/v1/api-keys", {
			body: { name: "more", scope: "full" },
			headers: bearer(key),
		});
		expect(mint.status).toBe(403);
		const password = await call("POST", "/api/v1/auth/password", {
			body: { current: "x", next: "a long new password" },
			headers: bearer(key),
		});
		expect(password.status).toBe(403);
	}
	// A full key may change things (app.test.ts checks the edit records the key).
	const edit = await call("PATCH", "/api/v1/numbers/1", {
		body: { note: "x" },
		headers: bearer(full),
	});
	expect(edit.status).toBe(404);
});

test("a revoked or unknown key is refused, even next to a valid session cookie", async () => {
	const { session } = await signIn();
	const key = await createKey(session, "old", "full");
	const { keys } = (await (
		await call("GET", "/api/v1/api-keys", { headers: { cookie: session } })
	).json()) as { keys: { id: number }[] };
	const revoke = await call("DELETE", `/api/v1/api-keys/${keys[0]?.id}`, {
		headers: { cookie: session },
	});
	expect(revoke.status).toBe(200);
	expect(await revoke.json()).toMatchObject({ name: "old" });
	const refused = await call("GET", "/api/v1/phones", {
		headers: { ...bearer(key), cookie: session },
	});
	expect(refused.status).toBe(401);
	expect(await refused.json()).toEqual({ error: "unknown or revoked API key" });
	expect(
		(await call("GET", "/api/v1/phones", { headers: bearer("sl_nope") }))
			.status,
	).toBe(401);
	expect(
		(
			await call("DELETE", "/api/v1/api-keys/999", {
				headers: { cookie: session },
			})
		).status,
	).toBe(404);
});

test("revoking a key ends its open event stream at once", async () => {
	const { session } = await signIn();
	const key = await createKey(session, "stream", "read");
	const server = app.listen({ hostname: "127.0.0.1", port: 0 });
	try {
		const base = `http://127.0.0.1:${server.server?.port}`;
		const stream = await fetch(`${base}/api/v1/events?types=message`, {
			headers: bearer(key),
		});
		expect(stream.status).toBe(200);
		const reader = stream.body?.getReader();
		const ended = (async () => {
			for (;;) if ((await reader?.read())?.done ?? true) return true;
		})();
		const id = auth.listKeys().find((apiKey) => apiKey.name === "stream")?.id;
		const revoke = await fetch(`${base}/api/v1/api-keys/${id}`, {
			method: "DELETE",
			headers: { cookie: session },
		});
		expect(revoke.status).toBe(200);
		expect(
			await Promise.race([ended, Bun.sleep(2_000).then(() => false)]),
		).toBe(true);
	} finally {
		await server.stop(true);
	}
});
