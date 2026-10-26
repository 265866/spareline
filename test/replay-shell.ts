import { join } from "node:path";
import type { ShellResult } from "../src/adb/client";
import type { DeviceShell } from "../src/devices/probe";

export const FIXTURES = join(import.meta.dir, "fixtures/device");

export function fixture(phone: string, name: string): Promise<string> {
	return Bun.file(join(FIXTURES, phone, name)).text();
}

// A DeviceShell stand-in that answers each probe script with output recorded
// from a real phone (scripts/capture-fixtures.ts). Inbound phone states such
// as a fresh SIM or a phone coming online cannot be produced on demand on real
// hardware, so tests drive DeviceManager and the API with recordings instead.
export class ReplayShell implements DeviceShell {
	readonly calls: string[] = [];
	private readonly answers = new Map<string, string>();
	// Commands that start with these prefixes get the mapped output.
	private readonly prefixes: [string, string][] = [];
	failing: Error | undefined;

	answer(command: string, stdout: string): this {
		this.answers.set(command, stdout);
		return this;
	}

	answerPrefix(prefix: string, stdout: string): this {
		this.prefixes.push([prefix, stdout]);
		return this;
	}

	async shell(serial: string, command: string): Promise<ShellResult> {
		this.calls.push(`${serial}: ${command}`);
		if (this.failing) throw this.failing;
		const stdout =
			this.answers.get(command) ??
			this.prefixes.find(([prefix]) => command.startsWith(prefix))?.[1];
		if (stdout === undefined)
			throw new Error(`no recorded output for: ${command}`);
		return {
			stdout: new TextEncoder().encode(stdout),
			stderr: new Uint8Array(),
			exitCode: 0,
		};
	}
}
