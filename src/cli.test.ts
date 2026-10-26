import { expect, test } from "bun:test";
import { Auth } from "./auth/auth";
import { run } from "./cli";
import { openDb } from "./db/db";

test("users add prints a password that signs in; list, reset, and remove work", async () => {
	const auth = new Auth(openDb(":memory:"));
	const lines: string[] = [];
	const cli = (...args: string[]) =>
		run(args, auth, (line) => lines.push(line));

	expect(await cli("users", "list")).toBe(0);
	expect(lines.pop()).toBe("No users yet.");

	expect(await cli("users", "add", "alice")).toBe(0);
	const added = /Password \(shown once\): (\S+)$/.exec(lines.pop() ?? "")?.[1];
	expect(await auth.verify("alice", added ?? "")).not.toBeNull();
	expect(await cli("users", "add", "alice")).toBe(1);
	expect(lines.pop()).toBe("user alice already exists");

	expect(await cli("users", "list")).toBe(0);
	expect(lines.pop()).toStartWith("alice\tsince ");

	expect(await cli("users", "reset-password", "alice")).toBe(0);
	const reset = /: (\S+)$/.exec(lines.pop() ?? "")?.[1];
	expect(await auth.verify("alice", added ?? "")).toBeNull();
	expect(await auth.verify("alice", reset ?? "")).not.toBeNull();

	expect(await cli("users", "remove", "alice")).toBe(0);
	expect(auth.hasUsers()).toBe(false);
	expect(await cli("users", "remove", "alice")).toBe(1);

	expect(await cli("users", "add", "bob")).toBe(0);
	expect(await cli("keys", "add", "bob", "script", "read")).toBe(0);
	const key = /Key \(shown once\): (\S+)$/.exec(lines.pop() ?? "")?.[1] ?? "";
	expect(auth.keyPrincipal(key)?.user.name).toBe("bob");
	expect(await cli("keys", "list")).toBe(0);
	expect(lines.pop()).toMatch(/^1\tsl_\S{6}\tbob\tscript\tread\tlast used /);
	expect(await cli("keys", "add", "nobody", "x", "full")).toBe(1);
	expect(await cli("keys", "add", "bob", "x", "admin")).toBe(2);
	expect(await cli("keys", "revoke", "1")).toBe(0);
	expect(auth.keyPrincipal(key)).toBeNull();
	expect(await cli("keys", "revoke", "9")).toBe(1);

	expect(await cli("users")).toBe(2);
	expect(await cli("groups", "list")).toBe(2);
	expect(lines.pop()).toStartWith("usage:");
});
