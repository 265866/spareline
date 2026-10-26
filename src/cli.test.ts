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

	expect(await cli("users")).toBe(2);
	expect(await cli("groups", "list")).toBe(2);
	expect(lines.pop()).toStartWith("usage:");
});
