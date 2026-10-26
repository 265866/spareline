import { type DeviceEntry, parseDeviceList } from "./device-list";
import {
	encodeRequest,
	encodeSync,
	parseHexLength,
	readHexString,
	readShellV2,
	readStatus,
	readSyncStatus,
	request,
	type ShellResult,
	SYNC_DATA_MAX,
} from "./protocol";
import { AdbError, AdbSocket, type Endpoint } from "./socket";

export type { DeviceEntry, ShellResult };

export type CallOptions = {
	signal?: AbortSignal;
	// Bounds the whole call, connection included.
	timeoutMs?: number;
};

// A bidirectional byte stream to a socket on the phone, for example the
// abstract socket scrcpy-server listens on.
export type AdbStream = {
	readable: ReadableStream<Uint8Array>;
	write(data: Uint8Array): Promise<void>;
	close(): void;
};

const DEFAULT_TIMEOUT_MS = 30_000;

export function parseEndpoint(address: string): Endpoint {
	const match = /^\[?([^\]]+?)\]?:(\d+)$/.exec(address);
	const port = Number(match?.[2]);
	if (!match?.[1] || !Number.isInteger(port) || port < 1 || port > 65535)
		throw new Error(`expected host:port for the adb server, got "${address}"`);
	return { host: match[1], port };
}

// A client for the adb server's smart-socket protocol (the same server the
// `adb` CLI talks to). Every call opens its own TCP connection, as the CLI
// does, so calls are independent and a failed call cannot poison another.
export class AdbClient {
	constructor(
		readonly endpoint: Endpoint,
		private readonly connectTimeoutMs = 5_000,
	) {}

	async version(options: CallOptions = {}): Promise<number> {
		return this.withHostSocket(options, async (socket) => {
			await request(socket, "host:version");
			return Number.parseInt(await readHexString(socket), 16);
		});
	}

	async listDevices(options: CallOptions = {}): Promise<DeviceEntry[]> {
		return this.withHostSocket(options, async (socket) => {
			await request(socket, "host:devices-l");
			return parseDeviceList(await readHexString(socket));
		});
	}

	// Yields the full device list on connect and after every change. Ends when
	// the server closes the stream; throws AdbError("aborted") when `signal`
	// aborts and other AdbErrors when the connection fails.
	async *trackDevices(signal?: AbortSignal): AsyncGenerator<DeviceEntry[]> {
		const socket = await AdbSocket.connect(this.endpoint, {
			signal,
			timeoutMs: this.connectTimeoutMs,
		});
		try {
			await request(socket, "host:track-devices-l");
			for (;;) {
				const header = await socket.readOrEnd(4);
				if (!header) return;
				const payload = await socket.read(parseHexLength(header));
				yield parseDeviceList(new TextDecoder().decode(payload));
			}
		} finally {
			socket.close();
		}
	}

	// Runs `command` through `sh -c` on the phone without a pty and returns
	// stdout, stderr, and the exit code separately (adb shell protocol v2).
	async shell(
		serial: string,
		command: string,
		options: CallOptions = {},
	): Promise<ShellResult> {
		return this.withDeviceService(
			serial,
			`shell,v2,raw:${command}`,
			options,
			readShellV2,
		);
	}

	// Runs `command` and returns its raw output. Without the shell protocol,
	// adbd merges stderr into stdout and reports no exit status, so an error
	// message is indistinguishable from data; use shell() when that matters.
	// Binary-safe: no pty and no line-ending translation.
	async exec(
		serial: string,
		command: string,
		options: CallOptions = {},
	): Promise<Uint8Array> {
		return this.withDeviceService(
			serial,
			`exec:${command}`,
			options,
			(socket) => socket.readToEnd(),
		);
	}

	// Opens an abstract unix socket on the phone, such as the one scrcpy-server
	// listens on.
	openLocal(
		serial: string,
		name: string,
		options: CallOptions = {},
	): Promise<AdbStream> {
		return this.openStream(serial, `localabstract:${name}`, options);
	}

	// Opens a duplex stream service on the phone (`localabstract:NAME`,
	// `tcp:PORT`). The stream stays open until either side closes it or
	// `signal` aborts; `timeoutMs` bounds only the setup.
	async openStream(
		serial: string,
		service: string,
		options: CallOptions = {},
	): Promise<AdbStream> {
		const setup = AbortSignal.timeout(options.timeoutMs ?? DEFAULT_TIMEOUT_MS);
		const socket = await AdbSocket.connect(this.endpoint, {
			signal: options.signal,
			timeoutMs: this.connectTimeoutMs,
		});
		const onTimeout = () =>
			socket.destroy(
				new AdbError("timeout", `opening ${service} on ${serial} timed out`),
			);
		setup.addEventListener("abort", onTimeout, { once: true });
		try {
			await request(socket, `host:transport:${serial}`);
			await request(socket, service);
		} catch (error) {
			socket.close();
			throw error;
		} finally {
			setup.removeEventListener("abort", onTimeout);
		}
		return {
			readable: new ReadableStream<Uint8Array>({
				async pull(controller) {
					try {
						const chunk = await socket.readChunk();
						if (chunk) controller.enqueue(chunk);
						else controller.close();
					} catch (error) {
						controller.error(error);
					}
				},
				cancel: () => socket.close(),
			}),
			write: (data) => socket.write(data),
			close: () => socket.close(),
		};
	}

	// Writes `data` to `remotePath` on the phone with the sync protocol.
	async push(
		serial: string,
		data: Uint8Array,
		remotePath: string,
		options: CallOptions & { mode?: number; mtime?: Date } = {},
	): Promise<void> {
		const mode = 0o100000 | (options.mode ?? 0o644);
		const mtime = Math.floor((options.mtime ?? new Date()).getTime() / 1000);
		await this.withDeviceService(serial, "sync:", options, async (socket) => {
			const encoder = new TextEncoder();
			await socket.write(
				encodeSync("SEND", encoder.encode(`${remotePath},${mode}`)),
			);
			for (let offset = 0; offset < data.length; offset += SYNC_DATA_MAX)
				await socket.write(
					encodeSync("DATA", data.subarray(offset, offset + SYNC_DATA_MAX)),
				);
			await socket.write(encodeSync("DONE", mtime));
			await readSyncStatus(socket, `push ${remotePath}`);
			await socket.write(encodeSync("QUIT", 0));
		});
	}

	private async withHostSocket<T>(
		options: CallOptions,
		run: (socket: AdbSocket) => Promise<T>,
	): Promise<T> {
		const signal = callSignal(options);
		const socket = await AdbSocket.connect(this.endpoint, {
			signal,
			timeoutMs: this.connectTimeoutMs,
		});
		try {
			return await run(socket);
		} finally {
			socket.close();
		}
	}

	private async withDeviceService<T>(
		serial: string,
		service: string,
		options: CallOptions,
		run: (socket: AdbSocket) => Promise<T>,
	): Promise<T> {
		return this.withHostSocket(options, async (socket) => {
			await request(socket, `host:transport:${serial}`);
			await socket.write(encodeRequest(service));
			await readStatus(socket, `${service.split(":")[0]} on ${serial}`);
			return run(socket);
		});
	}
}

function callSignal(options: CallOptions): AbortSignal {
	const timeout = AbortSignal.timeout(options.timeoutMs ?? DEFAULT_TIMEOUT_MS);
	return options.signal ? AbortSignal.any([options.signal, timeout]) : timeout;
}
