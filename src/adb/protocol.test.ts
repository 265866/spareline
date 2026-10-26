import { expect, test } from "bun:test";
import { loadTranscript } from "../../test/adb-replay";
import {
	encodeRequest,
	encodeShellPacket,
	encodeSync,
	parseHexLength,
	shellV2Stdout,
} from "./protocol";

const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes);

test("requests carry a 4-digit hex length", () => {
	expect(text(encodeRequest("host:version"))).toBe("000chost:version");
	expect(text(encodeRequest(""))).toBe("0000");
	// Length counts UTF-8 bytes, not characters.
	expect(text(encodeRequest("shell:echo é")).slice(0, 4)).toBe("000d");
});

test("requests reject NUL bytes and oversize payloads", () => {
	expect(() => encodeRequest("shell:echo a\0b")).toThrow("NUL");
	expect(() => encodeRequest("x".repeat(0x10000))).toThrow("65535");
	expect(encodeRequest("x".repeat(0xffff)).length).toBe(0xffff + 4);
});

test("hex lengths", () => {
	expect(parseHexLength(new TextEncoder().encode("0061"))).toBe(0x61);
	expect(parseHexLength(new TextEncoder().encode("FFFF"))).toBe(0xffff);
	expect(() => parseHexLength(new TextEncoder().encode("OKAY"))).toThrow();
});

test("sync and shell packets are little-endian", () => {
	expect([...encodeSync("DONE", 0x01020304)]).toEqual([
		0x44, 0x4f, 0x4e, 0x45, 0x04, 0x03, 0x02, 0x01,
	]);
	expect([...encodeSync("DATA", new Uint8Array([9, 8]))]).toEqual([
		0x44, 0x41, 0x54, 0x41, 2, 0, 0, 0, 9, 8,
	]);
	expect([...encodeShellPacket(0, new Uint8Array([7]))]).toEqual([
		0, 1, 0, 0, 0, 7,
	]);
});

test("shellV2Stdout unframes a recorded shell,v2 reply, however it is split", async () => {
	const transcript = await loadTranscript("shell-v2");
	const server = Buffer.concat(
		(transcript.connections[0] ?? [])
			.filter((event) => event.from === "server")
			.map((event) => Buffer.from(event.hex, "hex")),
	);
	// Two OKAYs (transport, then service), then the shell packets.
	const packets = new Uint8Array(server.subarray(8));
	for (const size of [packets.length, 1, 3]) {
		const chunks: Uint8Array[] = [];
		for (let at = 0; at < packets.length; at += size)
			chunks.push(packets.subarray(at, at + size));
		const out = await new Response(
			new ReadableStream<Uint8Array>({
				start(controller) {
					for (const chunk of chunks) controller.enqueue(chunk);
					controller.close();
				},
			}).pipeThrough(shellV2Stdout()),
		).text();
		expect(out).toBe("S6702X\n");
	}
});
