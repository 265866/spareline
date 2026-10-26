import type { AdbStream, CallOptions, DeviceEntry } from "../src/adb/client";
import { parseDeviceList } from "../src/adb/device-list";
import { encodeShellPacket } from "../src/adb/protocol";
import { AdbError } from "../src/adb/socket";
import type { AdbPort } from "../src/devices/manager";
import { HEALTH_SCRIPT, IDENTITY_SCRIPT } from "../src/devices/probe";
import { loadTranscript } from "./adb-replay";
import { fixture, ReplayShell } from "./replay-shell";

// The device lists the adb server sent while the B30's transport was kicked
// (test/fixtures/adb/track-devices-reconnect.json), decoded in order.
export async function recordedTrackLists(): Promise<DeviceEntry[][]> {
	const transcript = await loadTranscript("track-devices-reconnect");
	const events = transcript.connections[0] ?? [];
	let stream = Buffer.concat(
		events
			.filter((e) => e.from === "server")
			.map((e) => Buffer.from(e.hex, "hex")),
	).toString("latin1");
	stream = stream.slice("OKAY".length);
	const lists: DeviceEntry[][] = [];
	while (stream.length >= 4) {
		const length = Number.parseInt(stream.slice(0, 4), 16);
		lists.push(parseDeviceList(stream.slice(4, 4 + length)));
		stream = stream.slice(4 + length);
	}
	return lists;
}

export type StreamHandle = {
	serial: string;
	service: string;
	closed: boolean;
	push(text: string): void;
};

// An adb server stand-in for DeviceManager tests: the test feeds device lists
// and the shell answers with output recorded from the B30.
export class FakeAdb extends ReplayShell implements AdbPort {
	reachable = true;
	versionCalls = 0;
	private queue: (DeviceEntry[] | "end")[] = [];
	private wake: (() => void) | undefined;

	static async b30(): Promise<FakeAdb> {
		return new FakeAdb().recordB30();
	}

	// Answers the device manager's probes with the B30's recorded output.
	async recordB30(): Promise<this> {
		this.answer(IDENTITY_SCRIPT, await fixture("nuu-s6702x", "identity.txt"))
			.answerPrefix(
				"printf '\\n@@sl:imei:0\\n'",
				await fixture("nuu-s6702x", "subscriber.txt"),
			)
			.answer(HEALTH_SCRIPT, await fixture("nuu-s6702x", "health.txt"));
		return this;
	}

	// While set, every shell call waits for it, like a phone that is slow to
	// answer. The fake ignores abort signals, as a stuck adb call would.
	hold: Promise<void> | null = null;

	override async shell(serial: string, command: string) {
		if (this.hold) await this.hold;
		return super.shell(serial, command);
	}

	send(list: DeviceEntry[] | "end"): void {
		this.queue.push(list);
		this.wake?.();
	}

	async version(): Promise<number> {
		this.versionCalls++;
		if (!this.reachable)
			throw new AdbError("connect", "cannot reach the adb server (fake)");
		return 41;
	}

	async *trackDevices(signal?: AbortSignal): AsyncGenerator<DeviceEntry[]> {
		for (;;) {
			if (signal?.aborted) return;
			const next = this.queue.shift();
			if (next === "end") return;
			if (next) {
				yield next;
				continue;
			}
			await new Promise<void>((resolve) => {
				this.wake = resolve;
				signal?.addEventListener("abort", () => resolve(), { once: true });
			});
		}
	}

	async exec(
		_serial: string,
		_command: string,
		_options?: CallOptions,
	): Promise<Uint8Array> {
		return new TextEncoder().encode("exec-ok");
	}

	async openLocal(): Promise<AdbStream> {
		throw new AdbError("fail", "localabstract: not recorded");
	}

	// Streams opened with openStream (logcat), which the test feeds. The radio
	// log lines of a SIM swap cannot be produced on demand on a real phone.
	readonly streams: StreamHandle[] = [];
	async openStream(serial: string, service: string): Promise<AdbStream> {
		let controller!: ReadableStreamDefaultController<Uint8Array>;
		const readable = new ReadableStream<Uint8Array>({
			start(c) {
				controller = c;
			},
		});
		const handle: StreamHandle = {
			serial,
			service,
			closed: false,
			// As stdout packets when the service speaks the shell protocol.
			push: (text) => {
				const bytes = new TextEncoder().encode(text);
				controller.enqueue(
					service.startsWith("shell,v2,") ? encodeShellPacket(1, bytes) : bytes,
				);
			},
		};
		this.streams.push(handle);
		return {
			readable,
			write: async () => {},
			close: () => {
				if (handle.closed) return;
				handle.closed = true;
				controller.close();
			},
		};
	}

	readonly pushes: { serial: string; remote: string; size: number }[] = [];
	async push(serial: string, data: Uint8Array, remote: string): Promise<void> {
		this.pushes.push({ serial, remote, size: data.length });
	}
}
