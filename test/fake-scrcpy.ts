import { join } from "node:path";
import type { AdbStream, CallOptions } from "../src/adb/client";
import type { DeviceEvent, LivePhone } from "../src/devices/manager";
import type { ScreenMessage } from "../src/screens/manager";
import type { ViewerSink } from "../src/screens/viewer";

// A stand-in for the phone side of a screen session. A real session needs a
// phone running scrcpy-server, and these tests need to end streams, crash
// servers, hold a start halfway, leave orphan servers, and unplug phones on
// demand. The video bytes it serves are the real recording from the B30
// (test/fixtures/screen/grid-home.bin).
const recorded = new Uint8Array(
	await Bun.file(
		join(import.meta.dir, "fixtures/screen/grid-home.bin"),
	).arrayBuffer(),
);

// The recording's dummy byte and video header, then its packets.
const HEADER_BYTES = 1 + 64 + 12;
function splitPackets(bytes: Uint8Array): Uint8Array[] {
	const packets: Uint8Array[] = [];
	for (let offset = HEADER_BYTES; offset < bytes.length; ) {
		const size = new DataView(
			bytes.buffer,
			bytes.byteOffset + offset + 8,
		).getUint32(0);
		packets.push(bytes.subarray(offset, offset + 12 + size));
		offset += 12 + size;
	}
	return packets;
}
const packets = splitPackets(recorded);
const configIndex = packets.findIndex((p) => ((p[0] ?? 0) & 0x80) !== 0);
// What `dumpsys power | grep -m1 mWakefulness=` printed on the B30, awake
// and dozing (grep closing the pipe early makes dumpsys add a line).
const wakefulnessOutput = {
	Awake: await Bun.file(
		join(import.meta.dir, "fixtures/device/nuu-s6702x/wakefulness-awake.txt"),
	).text(),
	Dozing: await Bun.file(
		join(import.meta.dir, "fixtures/device/nuu-s6702x/wakefulness-dozing.txt"),
	).text(),
};
// What the server sends after RESET_VIDEO: a config packet and a keyframe.
const reset = [packets[configIndex], packets[configIndex + 1]] as Uint8Array[];

export type FakeServer = {
	command: string;
	video: ReadableStreamDefaultController<Uint8Array> | null;
	control: Uint8Array[];
	alive: boolean;
};

// A scrcpy process left on the phone from before (a crashed relay, someone's
// own scrcpy). pkill of the server ends it unless `unkillable`; its CleanUp
// then runs for `cleanupMs`.
export type Orphan = { unkillable?: boolean; cleanupMs: number };

export class FakePhone {
	online = true;
	servers: FakeServer[] = [];
	pushes = 0;
	kills: string[] = [];
	// pkill commands that have finished, after killDelayMs.
	killsDone: string[] = [];
	killDelayMs = 0;
	orphan: Orphan | null = null;
	// Wake commands and control writes, in order: "exec:<command>" and
	// "control:<type>".
	log: string[] = [];
	// What `dumpsys power` reports. The wake command makes it Awake unless
	// the wake does not stick, and throws when wakeFails.
	wakefulness: keyof typeof wakefulnessOutput = "Awake";
	wakeSticks = true;
	wakeFails = false;
	wakefulnessReads = 0;
	// While set, video sockets wait for it, holding a start halfway.
	gate: Promise<void> | null = null;
	// When false, RESET_VIDEO is recorded but not answered until answerReset().
	answerResets = true;
	private orphanGoneAt = 0;
	private listeners = new Map<DeviceEvent, Set<(serial: string) => void>>();

	get(serial: string): LivePhone | undefined {
		return {
			serial,
			status: this.online ? "online" : "offline",
		} as LivePhone;
	}

	on(event: DeviceEvent, listener: (serial: string) => void) {
		const set = this.listeners.get(event) ?? new Set();
		this.listeners.set(event, set);
		set.add(listener);
		return () => set.delete(listener);
	}

	unplug(serial: string): void {
		this.online = false;
		for (const listener of this.listeners.get("disconnected") ?? [])
			listener(serial);
		for (const server of this.servers) this.crash(server);
	}

	async push(): Promise<void> {
		this.pushes++;
	}

	async exec(
		_serial: string,
		command: string,
		options: CallOptions = {},
	): Promise<Uint8Array> {
		if (!this.online) throw new Error("phone is not online");
		if (command.startsWith("pgrep")) {
			const running = this.orphan !== null || Date.now() < this.orphanGoneAt;
			return new TextEncoder().encode(running ? "4242\n" : "");
		}
		if (command.startsWith("input ")) {
			this.log.push(`exec:${command}`);
			if (this.wakeFails) throw new Error("input: device busy (stand-in)");
			if (this.wakeSticks) this.wakefulness = "Awake";
			return new Uint8Array();
		}
		if (command.startsWith("dumpsys power")) {
			this.wakefulnessReads++;
			return new TextEncoder().encode(wakefulnessOutput[this.wakefulness]);
		}
		if (command.startsWith("pkill")) {
			this.kills.push(command);
			if (this.killDelayMs) await Bun.sleep(this.killDelayMs);
			this.killsDone.push(command);
			if (
				command.includes("scrcpy.Serve[r]") &&
				this.orphan &&
				!this.orphan.unkillable
			) {
				this.orphanGoneAt = Date.now() + this.orphan.cleanupMs;
				this.orphan = null;
			}
			return new Uint8Array();
		}
		const server: FakeServer = {
			command,
			video: null,
			control: [],
			alive: true,
		};
		this.servers.push(server);
		return new Promise((_, reject) =>
			options.signal?.addEventListener("abort", () => {
				server.alive = false;
				reject(new Error("aborted"));
			}),
		);
	}

	// The first connection to a live server is its video socket (recorded
	// bytes, then open until closed), the second its control socket, which
	// answers RESET_VIDEO with a config packet and a keyframe.
	async openLocal(): Promise<AdbStream> {
		const server = this.servers.at(-1);
		if (!this.online || !server?.alive)
			throw new Error("localabstract: closed");
		if (!server.video) {
			await this.gate;
			if (!server.alive) throw new Error("localabstract: closed");
			const stream = new ReadableStream<Uint8Array>({
				start(controller) {
					server.video = controller;
					controller.enqueue(recorded);
				},
			});
			return { readable: stream, write: async () => {}, close: () => {} };
		}
		return {
			readable: new ReadableStream({
				start: (controller) => {
					const timer = setInterval(() => {
						if (!server.alive) {
							clearInterval(timer);
							controller.close();
						}
					}, 5);
				},
			}),
			write: async (data) => {
				server.control.push(data.slice());
				this.log.push(`control:${data[0]}`);
				if (data.length === 1 && data[0] === 17 && this.answerResets)
					this.answerReset(server);
			},
			close: () => {},
		};
	}

	answerReset(server: FakeServer): void {
		if (server.alive) for (const packet of reset) server.video?.enqueue(packet);
	}

	// A keyframe the encoder sends on its own, with no config packet before it.
	sendKeyframe(server: FakeServer): void {
		if (server.alive) server.video?.enqueue(reset[1] as Uint8Array);
	}

	crash(server: FakeServer): void {
		server.alive = false;
		try {
			server.video?.close();
		} catch {
			// Already closed.
		}
	}
}

export class FakeSocket implements ViewerSink {
	text: ScreenMessage[] = [];
	binary: Uint8Array[] = [];
	// Every frame in order: a message type, or "binary".
	order: string[] = [];
	closed: { code: number; reason: string } | null = null;
	send(data: Uint8Array | string): number {
		if (typeof data === "string") {
			const message = JSON.parse(data) as ScreenMessage;
			this.text.push(message);
			this.order.push(message.t);
		} else {
			this.binary.push(data);
			this.order.push("binary");
		}
		return 1;
	}
	bufferedAmount(): number {
		return 0;
	}
	close(code: number, reason: string): void {
		this.closed = { code, reason };
	}
	hellos() {
		return this.text.filter((m) => m.t === "hello");
	}
}
