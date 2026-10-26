import type { ShellResult } from "../src/adb/client";
import { ISMS_TRIGGER_SCRIPT } from "../src/sms/parse";
import { commands } from "../src/sms/provider";
import { FakeAdb } from "./fake-adb";
import { fixture } from "./replay-shell";

export const B30 = "SPARELINE00001";
// Provider _ids in the B30's recorded inbox (test/fixtures/device/nuu-s6702x).
export const RECORDED_IDS = [4, 5, 6, 7, 8, 9, 10, 11];

export const NO_RESULT = "No result found.\n";

// What `content read` of an MMS part gives: bytes, provider errors on
// stderr (with exit 0, as Content.java does), or no exit packet at all.
export type PartRead =
	| { stdout: Uint8Array; stderr?: string; exitCode?: number }
	| "no exit packet";

const PART = /^content read --uri content:\/\/mms\/part\/(\d+)$/;

// FakeAdb answering the SMS ingest's commands with the B30's recorded
// provider and isms output. Commands that ask for the same rows in another
// form (by id instead of after a cursor) get the same recording. New texts
// and MMS parts cannot be produced on demand (the phone is out of service
// and its MMS inbox is empty), so tests add them as clearly marked
// stand-ins: provider rows with `answer`, part bytes in `parts`.
export async function smsFakeAdb(): Promise<
	FakeAdb & { parts: Map<number, PartRead> }
> {
	const adb = Object.assign(await FakeAdb.b30(), {
		parts: new Map<number, PartRead>(),
	});
	const inbox = await fixture("nuu-s6702x", "sms-provider.txt");
	adb
		.answer(ISMS_TRIGGER_SCRIPT, await fixture("nuu-s6702x", "isms.txt"))
		.answer(commands.smsKeys(), await fixture("nuu-s6702x", "sms-keys.txt"))
		.answer(commands.smsAfter(0), inbox)
		.answer(commands.smsByIds(RECORDED_IDS), inbox)
		.answer(commands.smsAfter(11), NO_RESULT)
		.answer(commands.mmsKeys(), await fixture("nuu-s6702x", "mms-keys.txt"))
		.answer(
			commands.mmsAfter(0),
			await fixture("nuu-s6702x", "mms-provider.txt"),
		);
	const part = (command: string) => {
		const id = Number(PART.exec(command)?.[1]);
		return Number.isNaN(id)
			? undefined
			: (adb.parts.get(id) ?? { stdout: new Uint8Array() });
	};
	const shell = adb.shell.bind(adb);
	adb.shell = async (serial: string, command: string): Promise<ShellResult> => {
		const read = part(command);
		if (read === undefined) return shell(serial, command);
		adb.calls.push(`${serial}: ${command}`);
		if (read === "no exit packet")
			throw new Error("the shell ended without reporting an exit code");
		return {
			stdout: read.stdout,
			stderr: new TextEncoder().encode(read.stderr ?? ""),
			exitCode: read.exitCode ?? 0,
		};
	};
	// exec: has no shell protocol, so adbd merges stderr into stdout.
	adb.exec = async (_serial: string, command: string) => {
		const read = part(command);
		if (read === undefined || read === "no exit packet")
			return new Uint8Array();
		return Buffer.concat([read.stdout, Buffer.from(read.stderr ?? "")]);
	};
	return adb;
}

export function hex(text: string): string {
	return Buffer.from(text, "utf8").toString("hex").toUpperCase();
}
