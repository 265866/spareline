import { AdbError, type AdbSocket, concat } from "./socket";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

// The adb server's smart-socket request: 4 lowercase hex digits of the payload
// length, then the payload. The server limits a request to 0xffff bytes.
export function encodeRequest(service: string): Uint8Array {
	// adbd ends the service name at a NUL, which would silently truncate a
	// shell command.
	if (service.includes("\0"))
		throw new AdbError("protocol", "adb requests cannot contain NUL bytes");
	const body = encoder.encode(service);
	if (body.length > 0xffff)
		throw new AdbError(
			"protocol",
			`adb request is ${body.length} bytes, over the 65535 limit`,
		);
	const header = body.length.toString(16).padStart(4, "0");
	const out = new Uint8Array(4 + body.length);
	out.set(encoder.encode(header), 0);
	out.set(body, 4);
	return out;
}

export function parseHexLength(bytes: Uint8Array): number {
	const text = decoder.decode(bytes);
	if (!/^[0-9a-fA-F]{4}$/.test(text))
		throw new AdbError(
			"protocol",
			`expected a 4-digit hex length, got ${JSON.stringify(text)}`,
		);
	return Number.parseInt(text, 16);
}

// One length-prefixed string, as the server sends after FAIL, for
// host:version, and for each track-devices update.
export async function readHexString(socket: AdbSocket): Promise<string> {
	const length = parseHexLength(await socket.read(4));
	return decoder.decode(await socket.read(length));
}

// Reads OKAY, or throws the server's FAIL message.
export async function readStatus(
	socket: AdbSocket,
	what: string,
): Promise<void> {
	const status = decoder.decode(await socket.read(4));
	if (status === "OKAY") return;
	if (status === "FAIL") {
		const message = await readHexString(socket).catch(() => "(no message)");
		throw new AdbError("fail", `${what}: ${message}`);
	}
	throw new AdbError(
		"protocol",
		`${what}: expected OKAY or FAIL, got ${JSON.stringify(status)}`,
	);
}

export async function request(
	socket: AdbSocket,
	service: string,
): Promise<void> {
	await socket.write(encodeRequest(service));
	await readStatus(socket, service);
}

export type ShellResult = {
	stdout: Uint8Array;
	stderr: Uint8Array;
	exitCode: number;
};

// shell,v2 packets: 1 byte id, 4 byte little-endian length, then the data.
const ShellPacket = {
	stdin: 0,
	stdout: 1,
	stderr: 2,
	exit: 3,
	closeStdin: 4,
	windowSize: 5,
} as const;

export function encodeShellPacket(id: number, data: Uint8Array): Uint8Array {
	const out = new Uint8Array(5 + data.length);
	const view = new DataView(out.buffer);
	view.setUint8(0, id);
	view.setUint32(1, data.length, true);
	out.set(data, 5);
	return out;
}

export async function readShellV2(socket: AdbSocket): Promise<ShellResult> {
	const stdout: Uint8Array[] = [];
	const stderr: Uint8Array[] = [];
	for (;;) {
		const header = await socket.read(5).catch((error: unknown) => {
			if (error instanceof AdbError && error.code === "closed")
				throw new AdbError(
					"closed",
					"the shell ended without reporting an exit code (device disconnected?)",
					{ cause: error },
				);
			throw error;
		});
		const view = new DataView(header.buffer, header.byteOffset, 5);
		const id = view.getUint8(0);
		const length = view.getUint32(1, true);
		const data = await socket.read(length);
		switch (id) {
			case ShellPacket.stdout:
				stdout.push(data);
				break;
			case ShellPacket.stderr:
				stderr.push(data);
				break;
			case ShellPacket.exit: {
				const exitCode = data[0];
				if (exitCode === undefined)
					throw new AdbError("protocol", "shell exit packet without a code");
				socket.close();
				return {
					stdout: concat(stdout),
					stderr: concat(stderr),
					exitCode,
				};
			}
			default:
				// adbd sends no other packet types to the host; skip any it adds.
				break;
		}
	}
}

// The stdout of a streaming `shell,v2` service, unframed. A long-running
// command such as logcat runs under the shell protocol so that adbd ends it
// when the stream closes; this strips the packet headers as bytes arrive.
export function shellV2Stdout(): TransformStream<Uint8Array, Uint8Array> {
	let pending: Uint8Array = new Uint8Array(0);
	return new TransformStream({
		transform(chunk, controller) {
			pending = concat([pending, chunk]);
			while (pending.length >= 5) {
				const view = new DataView(pending.buffer, pending.byteOffset, 5);
				const length = view.getUint32(1, true);
				if (pending.length < 5 + length) break;
				if (view.getUint8(0) === ShellPacket.stdout)
					controller.enqueue(pending.slice(5, 5 + length));
				pending = pending.slice(5 + length);
			}
		},
	});
}

// sync: requests and responses carry a 4-byte ASCII id and a little-endian
// 32-bit length or value.
export const SYNC_DATA_MAX = 64 * 1024;

export function encodeSync(id: string, value: number | Uint8Array): Uint8Array {
	const payload = typeof value === "number" ? undefined : value;
	const out = new Uint8Array(8 + (payload?.length ?? 0));
	out.set(encoder.encode(id), 0);
	new DataView(out.buffer).setUint32(
		4,
		payload ? payload.length : (value as number),
		true,
	);
	if (payload) out.set(payload, 8);
	return out;
}

export async function readSyncStatus(
	socket: AdbSocket,
	what: string,
): Promise<void> {
	const header = await socket.read(8);
	const id = decoder.decode(header.subarray(0, 4));
	const value = new DataView(header.buffer, header.byteOffset, 8).getUint32(
		4,
		true,
	);
	if (id === "OKAY") return;
	if (id === "FAIL") {
		const message = decoder.decode(await socket.read(value));
		throw new AdbError("fail", `${what}: ${message}`);
	}
	throw new AdbError(
		"protocol",
		`${what}: expected OKAY or FAIL, got ${JSON.stringify(id)}`,
	);
}
