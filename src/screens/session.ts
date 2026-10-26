import type { AdbStream } from "../adb/client";
import type { Devices } from "../devices/manager";
import type { Log } from "../log";
import {
	ANY_SCRCPY_PATTERN,
	ANY_SERVER_PATTERN,
	CODEC_H264,
	CODEC_HEADER_LENGTH,
	DEVICE_NAME_LENGTH,
	newScid,
	parseVideoMeta,
	RESET_VIDEO,
	SERVER_PATH,
	StreamReader,
	scidPattern,
	serverCommand,
	type VideoMeta,
	type VideoPacket,
} from "./protocol";

export type ScreenDevices = Pick<Devices, "exec" | "openLocal" | "push">;

export type SessionHandlers = {
	packet(packet: VideoPacket): void;
	// Called once. `error` is null when stop() ended the session.
	ended(error: Error | null): void;
};

export type SessionOptions = {
	// How long the server may take to accept the video socket.
	connectTimeoutMs?: number;
	// Minimum spacing of RESET_VIDEO requests.
	resetIntervalMs?: number;
};

// The longest timer Bun accepts; the server's exec call lives as long as the
// session.
const FOREVER_MS = 2_147_483_647;
// How long a start waits for earlier scrcpy processes to exit.
const SETTLE_TIMEOUT_MS = 3_000;
const decoder = new TextDecoder();

// One scrcpy-server run on one phone, from jar push to teardown. It reads the
// video socket and hands each packet to `handlers.packet` unchanged.
export class ScrcpySession {
	readonly scid = newScid();
	meta: VideoMeta | null = null;
	startedAt = 0;
	private readonly abort = new AbortController();
	private video: AdbStream | undefined;
	private reader: StreamReader | undefined;
	private control: AdbStream | undefined;
	private ended = false;
	private teardown: Promise<void> = Promise.resolve();
	private lastResetAt = 0;
	private resetTimer: Timer | undefined;
	private readonly connectTimeoutMs: number;
	private readonly resetIntervalMs: number;

	constructor(
		private readonly devices: ScreenDevices,
		readonly serial: string,
		private readonly jar: Uint8Array,
		private readonly handlers: SessionHandlers,
		private readonly log: Log,
		options: SessionOptions = {},
	) {
		this.connectTimeoutMs = options.connectTimeoutMs ?? 10_000;
		this.resetIntervalMs = options.resetIntervalMs ?? 500;
	}

	get running(): boolean {
		return !this.ended && this.meta !== null;
	}

	// Starts the server and resolves once the video stream's header has been
	// read. Rejects (and ends the session) if any step fails.
	async start(): Promise<VideoMeta> {
		const begin = performance.now();
		const signal = this.abort.signal;
		try {
			const settleMs = await this.settle();
			// The server unlinks its jar at startup, so every session pushes it.
			await this.devices.push(this.serial, this.jar, SERVER_PATH, {
				mode: 0o644,
			});
			this.devices
				.exec(this.serial, serverCommand(this.scid), {
					signal,
					timeoutMs: FOREVER_MS,
				})
				.then(
					(output) =>
						this.finish(
							new Error(
								`scrcpy-server exited: ${tail(decoder.decode(output))}`,
							),
						),
					(error: unknown) => this.finish(asError(error)),
				);
			const reader = await this.connectVideo();
			this.control = await this.devices.openLocal(
				this.serial,
				`scrcpy_${this.scid}`,
				{ signal, timeoutMs: 5_000 },
			);
			void this.drainControl(this.control);
			const meta = parseVideoMeta(
				await reader.read(DEVICE_NAME_LENGTH + CODEC_HEADER_LENGTH),
			);
			if (meta.codec !== CODEC_H264)
				throw new Error(`expected h264 video, got codec ${meta.codec}`);
			if (this.ended) throw new Error("the session ended while starting");
			this.meta = meta;
			this.startedAt = Date.now();
			this.log("screen-session-started", {
				serial: this.serial,
				scid: this.scid,
				width: meta.width,
				height: meta.height,
				ms: Math.round(performance.now() - begin),
				settleMs,
			});
			void this.pump(reader);
			return meta;
		} catch (error) {
			this.finish(asError(error));
			throw error;
		}
	}

	// Writes one control message to the server. Callers validate it first.
	async write(message: Uint8Array): Promise<void> {
		if (!this.running || !this.control) return;
		await this.control.write(message).catch(() => {});
	}

	// Asks the server for a new config packet and keyframe, at most once per
	// resetIntervalMs; requests in between are coalesced into one.
	requestKeyframe(): void {
		if (!this.running || this.resetTimer) return;
		const wait = this.lastResetAt + this.resetIntervalMs - Date.now();
		const send = () => {
			this.resetTimer = undefined;
			this.lastResetAt = Date.now();
			void this.write(RESET_VIDEO);
		};
		if (wait <= 0) send();
		else this.resetTimer = setTimeout(send, wait);
	}

	// Stops the server and resolves once it has been killed.
	async stop(): Promise<void> {
		this.finish(null);
		await this.teardown;
	}

	// A server's CleanUp child restores stay_on_while_plugged_in after the
	// server dies. A server started while another runs, or before that
	// CleanUp finishes, would record the modified value as the one to
	// restore. So a start kills any server left on the phone (an orphan from a
	// crashed relay, or someone's own scrcpy), waits until it and its CleanUp
	// are gone, and fails if they are not. Returns how long it waited.
	private async settle(): Promise<number> {
		const begin = Date.now();
		const deadline = begin + SETTLE_TIMEOUT_MS;
		await this.devices.exec(
			this.serial,
			`pkill -f '${ANY_SERVER_PATTERN}' || true`,
			{ signal: this.abort.signal, timeoutMs: 5_000 },
		);
		for (;;) {
			const pids = decoder
				.decode(
					await this.devices.exec(
						this.serial,
						`pgrep -f '${ANY_SCRCPY_PATTERN}' || true`,
						{ signal: this.abort.signal, timeoutMs: 5_000 },
					),
				)
				.trim();
			if (!pids) return Date.now() - begin;
			if (Date.now() >= deadline)
				throw new Error(
					`scrcpy processes ${pids.split(/\s+/).join(", ")} still running after ${SETTLE_TIMEOUT_MS} ms`,
				);
			await Bun.sleep(50);
		}
	}

	// The tunnel accepts a connection before the server listens; the server's
	// dummy byte (0x00) proves the far end is live.
	private async connectVideo(): Promise<StreamReader> {
		const deadline = performance.now() + this.connectTimeoutMs;
		let last: unknown;
		while (performance.now() < deadline) {
			if (this.ended) break;
			let stream: AdbStream | undefined;
			try {
				stream = await this.devices.openLocal(
					this.serial,
					`scrcpy_${this.scid}`,
					{ signal: this.abort.signal, timeoutMs: 5_000 },
				);
				const reader = new StreamReader(stream.readable);
				const dummy = await reader.read(1);
				if (dummy[0] !== 0)
					throw new Error(`expected dummy byte 0, got ${dummy[0]}`);
				this.video = stream;
				this.reader = reader;
				return reader;
			} catch (error) {
				last = error;
				stream?.close();
				await Bun.sleep(100);
			}
		}
		throw new Error(
			`scrcpy-server did not accept the video socket: ${asError(last).message}`,
		);
	}

	private async pump(reader: StreamReader): Promise<void> {
		try {
			for (;;) {
				const packet = await reader.readPacket();
				if (!packet) throw new Error("the video stream ended");
				this.handlers.packet(packet);
			}
		} catch (error) {
			this.finish(asError(error));
		}
	}

	// The server only sends clipboard and UHID messages, which the relay does
	// not use; reading keeps its socket from filling.
	private async drainControl(control: AdbStream): Promise<void> {
		try {
			for await (const _ of control.readable);
			this.finish(new Error("the control socket closed"));
		} catch (error) {
			this.finish(asError(error));
		}
	}

	private async kill(): Promise<void> {
		try {
			await this.devices.exec(
				this.serial,
				`pkill -f '${scidPattern(this.scid)}' || true`,
				{ timeoutMs: 5_000 },
			);
		} catch {
			// The phone is gone, and the server with it.
		}
	}

	private finish(error: Error | null): void {
		if (this.ended) return;
		this.ended = true;
		clearTimeout(this.resetTimer);
		// Closing the exec stream makes adbd kill app_process; closing the
		// sockets also ends it. pkill catches a server that outlived both.
		this.abort.abort();
		this.reader?.cancel();
		this.video?.close();
		this.control?.close();
		this.teardown = this.kill();
		this.log("screen-session-ended", {
			serial: this.serial,
			scid: this.scid,
			error: error?.message ?? null,
		});
		this.handlers.ended(error);
	}
}

function asError(error: unknown): Error {
	return error instanceof Error ? error : new Error(String(error));
}

function tail(text: string): string {
	const trimmed = text.trim();
	return trimmed.length > 500
		? `…${trimmed.slice(-500)}`
		: trimmed || "(no output)";
}
