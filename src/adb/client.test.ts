import { afterEach, describe, expect, setDefaultTimeout, test } from "bun:test";
import { loadTranscript, type Replay, replay } from "../../test/adb-replay";
import { AdbClient, parseEndpoint } from "./client";
import { AdbError } from "./socket";

// These tests replay byte transcripts recorded from the real adb server and the
// B30 (scripts/record-adb-fixtures.ts), both as recorded and written one byte at
// a time (the client may still read two at once), and check the bytes the client
// sends against the recording.

// Above the client's own 30 s call deadline, so a stalled request/response call
// fails with the client's timeout error instead of the test's (streams have no
// client deadline). Under heavy CPU load the slowest
// test here took about 3 s.
setDefaultTimeout(35_000);

const SERIAL = "SPARELINE00001";
let active: Replay | undefined;
afterEach(() => active?.stop());

async function clientFor(name: string, rechunk?: number) {
	active = replay(await loadTranscript(name), { rechunk });
	return new AdbClient({ host: "127.0.0.1", port: active.port });
}

async function rejection(promise: Promise<unknown>): Promise<AdbError> {
	try {
		await promise;
	} catch (error) {
		if (error instanceof AdbError) return error;
		throw error;
	}
	throw new Error("expected the call to reject");
}

for (const rechunk of [undefined, 1]) {
	describe(
		rechunk ? "replayed one byte at a time" : "replayed as recorded",
		() => {
			test("host:version", async () => {
				const client = await clientFor("version", rechunk);
				expect(await client.version()).toBe(41);
				expect(active?.mismatches).toEqual([]);
			});

			test("host:devices-l", async () => {
				const client = await clientFor("devices-l", rechunk);
				expect(await client.listDevices()).toEqual([
					{
						serial: SERIAL,
						state: "device",
						rawState: "device",
						usb: "1-1",
						product: "S6702X1",
						model: "S6702X",
						device: "S6702XA",
						transportId: 1,
					},
				]);
				expect(active?.mismatches).toEqual([]);
			});

			test("track-devices through a transport reconnect", async () => {
				const client = await clientFor("track-devices-reconnect", rechunk);
				const seen: string[] = [];
				for await (const list of client.trackDevices())
					seen.push(
						list.map((d) => `${d.state}#${d.transportId}`).join(",") || "none",
					);
				// The server's stream ends when the replay does.
				expect(seen).toEqual([
					"device#1",
					"offline#1",
					"none",
					"offline#2",
					"authorizing#2",
					"offline#2",
					"device#2",
				]);
				expect(active?.mismatches).toEqual([]);
			});

			test("shell,v2 separates stdout, stderr, and the exit code", async () => {
				const client = await clientFor("shell-v2", rechunk);
				const result = await client.shell(
					SERIAL,
					"getprop ro.product.model; echo oops >&2; exit 3",
				);
				expect(new TextDecoder().decode(result.stdout)).toBe("S6702X\n");
				expect(new TextDecoder().decode(result.stderr)).toBe("oops\n");
				expect(result.exitCode).toBe(3);
				expect(active?.mismatches).toEqual([]);
			});

			test("exec: returns raw binary stdout", async () => {
				const client = await clientFor("exec-binary", rechunk);
				const bytes = await client.exec(
					SERIAL,
					"head -c 64 /system/bin/toybox",
				);
				expect(bytes.length).toBe(64);
				expect([...bytes.subarray(0, 4)]).toEqual([0x7f, 0x45, 0x4c, 0x46]);
				expect(active?.mismatches).toEqual([]);
			});

			test("stream services are duplex", async () => {
				const client = await clientFor("stream-echo", rechunk);
				const stream = await client.openStream(SERIAL, "tcp:27183");
				await stream.write(new TextEncoder().encode("hello spareline"));
				const chunks: string[] = [];
				for await (const chunk of stream.readable)
					chunks.push(new TextDecoder().decode(chunk));
				expect(chunks.join("")).toBe("hello spareline");
				expect(active?.mismatches).toEqual([]);
			});
		},
	);
}

test("sync push sends SEND, DATA in 64 KiB chunks, DONE, and QUIT byte for byte", async () => {
	const client = await clientFor("push");
	const data = new Uint8Array(70_000).map((_, i) => i % 251);
	await client.push(SERIAL, data, "/data/local/tmp/sl-fixture.bin", {
		mtime: new Date("2026-10-21T00:00:00Z"),
	});
	expect(active?.mismatches).toEqual([]);
});

test("sync push surfaces the device's FAIL message", async () => {
	const client = await clientFor("push-fail");
	const error = await rejection(
		client.push(SERIAL, new Uint8Array([1, 2, 3]), "/system/sl-denied.bin"),
	);
	expect(error.code).toBe("fail");
	expect(error.message).toContain("Read-only file system");
});

test("an unknown serial fails with the server's message", async () => {
	const client = await clientFor("fail-unknown-serial");
	const error = await rejection(client.shell("NOSUCHSERIAL", "true"));
	expect(error.code).toBe("fail");
	expect(error.message).toContain("device 'NOSUCHSERIAL' not found");
});

test("localabstract to a missing socket fails", async () => {
	const client = await clientFor("fail-localabstract");
	const error = await rejection(client.openLocal(SERIAL, "sl-no-such-socket"));
	expect(error.code).toBe("fail");
	expect(error.message).toBe("localabstract:sl-no-such-socket: closed");
});

describe("failures without a transcript", () => {
	test("a refused connection is a connect error", async () => {
		const server = Bun.listen({
			hostname: "127.0.0.1",
			port: 0,
			socket: { data() {} },
		});
		const port = server.port;
		server.stop(true);
		const error = await rejection(
			new AdbClient({ host: "127.0.0.1", port }).version(),
		);
		expect(error.code).toBe("connect");
	});

	test("a silent server times out", async () => {
		const server = Bun.listen({
			hostname: "127.0.0.1",
			port: 0,
			socket: { data() {} },
		});
		try {
			const client = new AdbClient({ host: "127.0.0.1", port: server.port });
			const error = await rejection(client.version({ timeoutMs: 100 }));
			expect(error.code).toBe("timeout");
		} finally {
			server.stop(true);
		}
	});

	test("aborting cancels a waiting call", async () => {
		const server = Bun.listen({
			hostname: "127.0.0.1",
			port: 0,
			socket: { data() {} },
		});
		try {
			const client = new AdbClient({ host: "127.0.0.1", port: server.port });
			const controller = new AbortController();
			const pending = client.shell(SERIAL, "sleep 100", {
				signal: controller.signal,
			});
			setTimeout(() => controller.abort(), 20);
			expect((await rejection(pending)).code).toBe("aborted");
		} finally {
			server.stop(true);
		}
	});

	test("a connection closed mid-frame is reported, not hung", async () => {
		const server = Bun.listen({
			hostname: "127.0.0.1",
			port: 0,
			socket: {
				data(socket) {
					socket.end("OKAY00");
				},
			},
		});
		try {
			const client = new AdbClient({ host: "127.0.0.1", port: server.port });
			const error = await rejection(client.version());
			expect(error.code).toBe("closed");
		} finally {
			server.stop(true);
		}
	});

	test("an impossible frame length is a protocol error, not a hang", async () => {
		const server = Bun.listen({
			hostname: "127.0.0.1",
			port: 0,
			socket: {
				data(socket, chunk) {
					// OKAY to host:transport and to the shell request, then a stdout
					// packet claiming 4 GiB.
					if (chunk.toString().includes("shell"))
						socket.write(
							Buffer.concat([
								Buffer.from("OKAY"),
								Buffer.from([1, 0xff, 0xff, 0xff, 0xff]),
							]),
						);
					else socket.write("OKAY");
				},
			},
		});
		try {
			const client = new AdbClient({ host: "127.0.0.1", port: server.port });
			const error = await rejection(
				client.shell(SERIAL, "true", { timeoutMs: 2_000 }),
			);
			expect(error.code).toBe("protocol");
		} finally {
			server.stop(true);
		}
	});

	test("garbage instead of a status is a protocol error", async () => {
		const server = Bun.listen({
			hostname: "127.0.0.1",
			port: 0,
			socket: {
				data(socket) {
					socket.end("HTTP/1.1 400 Bad Request\r\n\r\n");
				},
			},
		});
		try {
			const client = new AdbClient({ host: "127.0.0.1", port: server.port });
			expect((await rejection(client.version())).code).toBe("protocol");
		} finally {
			server.stop(true);
		}
	});
});

test("parseEndpoint", () => {
	expect(parseEndpoint("127.0.0.1:5037")).toEqual({
		host: "127.0.0.1",
		port: 5037,
	});
	expect(parseEndpoint("[::1]:5037")).toEqual({ host: "::1", port: 5037 });
	expect(() => parseEndpoint("localhost")).toThrow();
	expect(() => parseEndpoint("localhost:99999")).toThrow();
});
