// Records adb wire transcripts for the protocol tests in src/adb/.
//
//   ADB_SERVER=127.0.0.1:5037 bun scripts/record-adb-fixtures.ts SERIAL test/fixtures/adb
//
// Each scenario runs AdbClient through a recording TCP proxy and saves every
// connection's bytes, both directions, with the server's chunk boundaries.
// The scenarios carry no personal data (no IMEI, ICCID, or number reads),
// because transcripts are committed byte for byte, except that the phone's
// serial is replaced with a fake one of the same length, which keeps every
// length prefix valid. The track-devices scenario
// kicks the phone's USB transport (`adb reconnect`), which the phone survives
// like a brief unplug.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Socket } from "bun";
import { AdbClient, parseEndpoint } from "../src/adb/client";
import { encodeRequest, readHexString, readStatus } from "../src/adb/protocol";
import { AdbSocket } from "../src/adb/socket";

type Event = { from: "client" | "server"; hex: string };

const [serial, outDir] = Bun.argv.slice(2);
if (!serial || !outDir) {
	console.error("usage: bun scripts/record-adb-fixtures.ts SERIAL OUT_DIR");
	process.exit(2);
}
const upstream = parseEndpoint(process.env.ADB_SERVER ?? "127.0.0.1:5037");
const direct = new AdbClient(upstream);
const hex = (bytes: Uint8Array) => Buffer.from(bytes).toString("hex");
const fakeSerial = `${"SPARELINE".padEnd(serial.length - 1, "0").slice(0, serial.length - 1)}1`;
const serialHex = hex(new TextEncoder().encode(serial));
const fakeSerialHex = hex(new TextEncoder().encode(fakeSerial));

async function record(
	name: string,
	description: string,
	scenario: (client: AdbClient) => Promise<unknown>,
): Promise<void> {
	const connections: Event[][] = [];
	type State = {
		events: Event[];
		upstream?: Socket<undefined>;
		pending: Uint8Array[];
	};
	const server = Bun.listen<State>({
		hostname: "127.0.0.1",
		port: 0,
		socket: {
			async open(client) {
				const state: State = { events: [], pending: [] };
				client.data = state;
				connections.push(state.events);
				state.upstream = await Bun.connect({
					hostname: upstream.host,
					port: upstream.port,
					socket: {
						data(_socket, chunk) {
							state.events.push({ from: "server", hex: hex(chunk) });
							client.write(chunk);
						},
						close: () => {
							client.end();
						},
					},
				});
				for (const chunk of state.pending.splice(0))
					state.upstream.write(chunk);
			},
			data(client, chunk) {
				client.data.events.push({ from: "client", hex: hex(chunk) });
				if (client.data.upstream) client.data.upstream.write(chunk);
				else client.data.pending.push(chunk.slice());
			},
			close: (client) => {
				client.data.upstream?.end();
			},
		},
	});
	const client = new AdbClient({ host: "127.0.0.1", port: server.port });
	let outcome: string;
	try {
		outcome = `ok: ${JSON.stringify(await scenario(client))}`;
	} catch (error) {
		outcome = `error: ${String(error)}`;
	}
	await Bun.sleep(200);
	server.stop(true);
	for (const events of connections)
		for (const event of events)
			event.hex = event.hex.replaceAll(serialHex, fakeSerialHex);
	// A serial split across two chunks would survive the replacement above.
	for (const events of connections)
		for (const from of ["client", "server"])
			if (
				events
					.filter((event) => event.from === from)
					.map((event) => event.hex)
					.join("")
					.includes(serialHex)
			)
				throw new Error(`${name} still contains the serial; not writing`);
	outcome = outcome.replaceAll(serial as string, fakeSerial);
	writeFileSync(
		join(outDir as string, `${name}.json`),
		`${JSON.stringify({ description, recordedAt: new Date().toISOString(), outcome, connections }, null, "\t")}\n`,
	);
	console.log(`${name}: ${outcome.slice(0, 120)}`);
}

mkdirSync(outDir, { recursive: true });
const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes);

await record("version", "host:version", (client) => client.version());
await record("devices-l", "host:devices-l with one phone attached", (client) =>
	client.listDevices(),
);
await record(
	"track-devices-reconnect",
	"host:track-devices-l while the phone's transport is kicked with `adb reconnect`",
	async (client) => {
		const controller = new AbortController();
		const seen: string[] = [];
		const stop = setTimeout(() => controller.abort(), 12_000);
		const kick = setTimeout(async () => {
			const socket = await AdbSocket.connect(upstream);
			await socket.write(encodeRequest(`host-serial:${serial}:reconnect`));
			await readStatus(socket, "reconnect");
			seen.push(`kicked: ${await readHexString(socket)}`);
			socket.close();
		}, 1_000);
		try {
			for await (const list of client.trackDevices(controller.signal)) {
				seen.push(
					list.map((entry) => `${entry.serial}:${entry.state}`).join(",") ||
						"(none)",
				);
				if (seen.length >= 3 && list.some((entry) => entry.state === "device"))
					break;
			}
		} catch (error) {
			seen.push(String(error));
		} finally {
			clearTimeout(stop);
			clearTimeout(kick);
		}
		return seen;
	},
);
await record(
	"shell-v2",
	"shell,v2 with stdout, stderr, and exit code 3",
	async (client) => {
		const result = await client.shell(
			serial,
			"getprop ro.product.model; echo oops >&2; exit 3",
		);
		return {
			stdout: text(result.stdout),
			stderr: text(result.stderr),
			exitCode: result.exitCode,
		};
	},
);
await record(
	"exec-binary",
	"exec: returning 64 bytes of an ELF header",
	async (client) =>
		(await client.exec(serial, "head -c 64 /system/bin/toybox")).length,
);
await record(
	"push",
	"sync: SEND of 70000 bytes (two DATA chunks)",
	async (client) => {
		const data = new Uint8Array(70_000).map((_, i) => i % 251);
		await client.push(serial, data, "/data/local/tmp/sl-fixture.bin", {
			mtime: new Date("2026-10-21T00:00:00Z"),
		});
		const size = text(
			await direct.exec(
				serial,
				"stat -c %s /data/local/tmp/sl-fixture.bin; rm /data/local/tmp/sl-fixture.bin",
			),
		).trim();
		return { size };
	},
);
await record("push-fail", "sync: SEND to a read-only path", (client) =>
	client.push(serial, new Uint8Array([1, 2, 3]), "/system/sl-denied.bin"),
);
await record(
	"fail-unknown-serial",
	"host:transport to a serial that is not attached",
	(client) => client.shell("NOSUCHSERIAL", "true"),
);
await record(
	"fail-localabstract",
	"localabstract: to a socket that does not exist",
	(client) => client.openLocal(serial, "sl-no-such-socket"),
);
await record(
	"stream-echo",
	"tcp: duplex stream to a loopback-only echo server on the phone",
	async (client) => {
		await direct.shell(
			serial,
			"(toybox nc -s 127.0.0.1 -p 27183 -L cat </dev/null >/dev/null 2>&1 &); sleep 0.3",
		);
		try {
			const stream = await client.openStream(serial, "tcp:27183");
			await stream.write(new TextEncoder().encode("hello spareline"));
			const reader = stream.readable.getReader();
			const { value } = await reader.read();
			stream.close();
			return text(value ?? new Uint8Array());
		} finally {
			await direct.shell(serial, "pkill -f 'nc -s 127.0.0.1 -p 27183'");
		}
	},
);
