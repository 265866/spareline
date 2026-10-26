import type { Database } from "bun:sqlite";
import { beforeEach, expect, test } from "bun:test";
import { openDb } from "../db/db";
import {
	Auth,
	AuthError,
	generatePassword,
	SESSION_DAYS,
	sha256,
} from "./auth";
import { LoginLimiter } from "./limiter";

let db: Database;
let clock: { now: Date };
let auth: Auth;
beforeEach(() => {
	db = openDb(":memory:");
	clock = { now: new Date("2026-10-26T00:00:00.000Z") };
	auth = new Auth(db, () => clock.now);
});

test("a new user gets a generated password that signs in; a wrong one does not", async () => {
	expect(auth.hasUsers()).toBe(false);
	const { user, password } = await auth.addUser(" alice ");
	expect(user.name).toBe("alice");
	expect(password).toMatch(/^[A-HJ-NP-Za-km-z2-9]{20}$/);
	expect(auth.hasUsers()).toBe(true);
	expect((await auth.verify("alice", password))?.id).toBe(user.id);
	// Names are case-insensitive.
	expect((await auth.verify("ALICE", password))?.id).toBe(user.id);
	expect(await auth.verify("alice", `${password}x`)).toBeNull();
	expect(await auth.verify("bob", password)).toBeNull();
	// Only the argon2id hash is stored.
	const stored = db
		.query<{ password_hash: string }, []>("SELECT password_hash FROM users")
		.get();
	expect(stored?.password_hash).toStartWith("$argon2id$");
	expect(stored?.password_hash).not.toContain(password);
});

test("user names are checked and unique", async () => {
	await auth.addUser("alice");
	await expect(auth.addUser("Alice")).rejects.toThrow("already exists");
	await expect(auth.addUser("")).rejects.toBeInstanceOf(AuthError);
	await expect(auth.addUser("a b")).rejects.toBeInstanceOf(AuthError);
	await expect(auth.addUser("x".repeat(65))).rejects.toBeInstanceOf(AuthError);
	expect(() => auth.removeUser("nobody")).toThrow("no user nobody");
});

test("a session holds only the token's hash, slides with use, and expires", async () => {
	const { user } = await auth.addUser("alice");
	const token = auth.createSession(user.id);
	expect(token).toMatch(/^[\w-]{43}$/);
	const row = db
		.query<{ token_sha256: string }, []>("SELECT token_sha256 FROM sessions")
		.get();
	expect(row?.token_sha256).toBe(sha256(token));
	expect(auth.session(token)?.user.name).toBe("alice");
	expect(auth.session(`${token}x`)).toBeNull();

	// Used every 20 days, it never expires.
	for (let i = 0; i < 3; i++) {
		clock.now = new Date(clock.now.getTime() + 20 * 86_400_000);
		expect(auth.session(token)?.user.name).toBe("alice");
	}
	// Unused for longer than SESSION_DAYS, it is gone.
	clock.now = new Date(clock.now.getTime() + (SESSION_DAYS + 1) * 86_400_000);
	expect(auth.session(token)).toBeNull();
	expect(db.query("SELECT count(*) AS n FROM sessions").get()).toEqual({
		n: 0,
	});
});

test("a new password ends every other session; removing a user ends all", async () => {
	const { user } = await auth.addUser("alice");
	const here = auth.createSession(user.id);
	const there = auth.createSession(user.id);
	const keep = auth.session(here)?.sessionId ?? null;
	await expect(auth.setPassword(user.id, "short", keep)).rejects.toThrow(
		"at least 12",
	);
	await auth.setPassword(user.id, "correct horse battery", keep);
	expect(auth.session(here)?.user.name).toBe("alice");
	expect(auth.session(there)).toBeNull();
	expect(await auth.verify("alice", "correct horse battery")).not.toBeNull();

	const reset = await auth.resetPassword("alice");
	expect(auth.session(here)).toBeNull();
	expect(await auth.verify("alice", reset)).not.toBeNull();

	const again = auth.createSession(user.id);
	auth.removeUser("alice");
	expect(auth.session(again)).toBeNull();
});

test("expired sessions are pruned", async () => {
	const { user } = await auth.addUser("alice");
	auth.createSession(user.id);
	clock.now = new Date(clock.now.getTime() + (SESSION_DAYS + 1) * 86_400_000);
	auth.createSession(user.id);
	auth.pruneSessions();
	expect(db.query("SELECT count(*) AS n FROM sessions").get()).toEqual({
		n: 1,
	});
});

test("generated passwords use the whole alphabet evenly", () => {
	const counts = new Map<string, number>();
	for (let i = 0; i < 2_000; i++)
		for (const char of generatePassword())
			counts.set(char, (counts.get(char) ?? 0) + 1);
	expect(counts.size).toBe(56);
	// 40,000 characters over 56 letters: about 714 each.
	for (const count of counts.values()) expect(count).toBeWithin(550, 900);
});

test("the limiter refuses a key after 10 failures in 15 minutes, until the oldest ages out", () => {
	let now = 0;
	const limiter = new LoginLimiter(10, 15 * 60_000, () => now);
	const keys = ["name:alice", "ip:192.0.2.1"];
	for (let i = 0; i < 9; i++) {
		limiter.fail(keys);
		now += 1_000;
	}
	expect(limiter.retryAfter(keys)).toBeNull();
	limiter.fail(keys);
	// The oldest failure was at 0 and expires at 900 s.
	expect(limiter.retryAfter(keys)).toBe(900 - 9);
	expect(limiter.retryAfter(["name:bob", "ip:192.0.2.1"])).toBe(891);
	// A success clears the name but not the address.
	limiter.succeed(keys);
	expect(limiter.retryAfter(["name:alice"])).toBeNull();
	expect(limiter.retryAfter(["ip:192.0.2.1"])).toBe(891);
	now = 15 * 60_000 + 1;
	expect(limiter.retryAfter(keys)).toBeNull();
});
