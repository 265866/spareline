import type { Socket } from "bun";

type AdbErrorCode =
	| "connect" // the adb server is not reachable
	| "timeout"
	| "aborted"
	| "closed" // the peer closed before the expected bytes arrived
	| "protocol" // bytes that do not follow the adb wire format
	| "fail"; // the server or device answered FAIL with a message

export class AdbError extends Error {
	constructor(
		readonly code: AdbErrorCode,
		message: string,
		options?: { cause?: unknown },
	) {
		super(message, options);
		this.name = "AdbError";
	}
}

export type Endpoint = { host: string; port: number };

// Reading pauses the socket once this much is buffered, so a slow consumer of a
// screen stream cannot grow memory without bound on a 2 GB Pi.
const HIGH_WATER = 4 * 1024 * 1024;

type Waiter = {
	resolve: () => void;
	reject: (error: AdbError) => void;
};

// A TCP connection to the adb server with exact-length reads. Every read and
// write rejects with an AdbError once the connection closes, fails, or the
// signal aborts.
export class AdbSocket {
	private chunks: Uint8Array[] = [];
	private buffered = 0;
	private ended = false;
	private failure: AdbError | undefined;
	private waiter: Waiter | undefined;
	private pendingWrites: Uint8Array[] = [];
	private drainWaiters: Waiter[] = [];
	private paused = false;
	private socket!: Socket<undefined>;
	private detach = () => {};

	private constructor() {}

	static async connect(
		endpoint: Endpoint,
		options: { signal?: AbortSignal; timeoutMs?: number } = {},
	): Promise<AdbSocket> {
		const adb = new AdbSocket();
		const timeoutMs = options.timeoutMs ?? 5_000;
		const signal = options.signal;
		if (signal?.aborted) throw abortError(signal);
		let timer: Timer | undefined;
		let onAbort = () => {};
		const connecting = Bun.connect({
			hostname: endpoint.host,
			port: endpoint.port,
			socket: {
				data: (_socket, chunk) => adb.onData(chunk),
				drain: () => adb.onDrain(),
				end: () => adb.onEnd(),
				close: (_socket, error) =>
					error ? adb.fail(new AdbError("closed", error.message)) : adb.onEnd(),
				error: (_socket, error) =>
					adb.fail(new AdbError("closed", error.message, { cause: error })),
			},
		});
		try {
			adb.socket = await Promise.race([
				connecting,
				new Promise<never>((_, reject) => {
					timer = setTimeout(
						() =>
							reject(
								new AdbError(
									"timeout",
									`connecting to the adb server at ${endpoint.host}:${endpoint.port} timed out after ${timeoutMs} ms`,
								),
							),
						timeoutMs,
					);
				}),
				new Promise<never>((_, reject) => {
					onAbort = () => reject(abortError(signal as AbortSignal));
					signal?.addEventListener("abort", onAbort, { once: true });
				}),
			]);
		} catch (error) {
			// A connection that completes after the timeout must not leak.
			void connecting.then((socket) => socket.end()).catch(() => {});
			if (error instanceof AdbError) throw error;
			throw new AdbError(
				"connect",
				`cannot reach the adb server at ${endpoint.host}:${endpoint.port}: ${String(error)}`,
				{ cause: error },
			);
		} finally {
			clearTimeout(timer);
			signal?.removeEventListener("abort", onAbort);
		}
		adb.socket.setNoDelay(true);
		if (signal) {
			const onAbort = () => adb.destroy(abortError(signal));
			signal.addEventListener("abort", onAbort, { once: true });
			adb.detach = () => signal.removeEventListener("abort", onAbort);
		}
		return adb;
	}

	get closed(): boolean {
		return this.ended || this.failure !== undefined;
	}

	// Exactly `length` bytes. Rejects with "closed" if the peer ends first.
	async read(length: number): Promise<Uint8Array> {
		// Reading pauses at HIGH_WATER, so a longer read could never complete.
		// No adb frame comes close; such a length means corrupt framing.
		if (length > HIGH_WATER)
			throw new AdbError(
				"protocol",
				`refusing to read ${length} bytes in one frame (limit ${HIGH_WATER})`,
			);
		while (this.buffered < length) {
			if (this.failure) throw this.failure;
			if (this.ended)
				throw new AdbError(
					"closed",
					`connection closed after ${this.buffered} of ${length} expected bytes`,
				);
			await this.wait();
		}
		return this.take(length);
	}

	// Exactly `length` bytes, or null when the peer closed cleanly before any
	// of them arrived.
	async readOrEnd(length: number): Promise<Uint8Array | null> {
		while (this.buffered === 0) {
			if (this.failure) throw this.failure;
			if (this.ended) return null;
			await this.wait();
		}
		return this.read(length);
	}

	// The next buffered bytes, or null once the peer has closed and the buffer
	// is empty.
	async readChunk(): Promise<Uint8Array | null> {
		while (this.buffered === 0) {
			if (this.failure) throw this.failure;
			if (this.ended) return null;
			await this.wait();
		}
		return this.take(this.buffered);
	}

	async readToEnd(): Promise<Uint8Array> {
		const parts: Uint8Array[] = [];
		for (
			let chunk = await this.readChunk();
			chunk;
			chunk = await this.readChunk()
		)
			parts.push(chunk);
		return concat(parts);
	}

	async write(data: Uint8Array | string): Promise<void> {
		if (this.failure) throw this.failure;
		if (this.ended)
			throw new AdbError("closed", "write after the connection closed");
		const bytes =
			typeof data === "string" ? new TextEncoder().encode(data) : data;
		if (this.pendingWrites.length === 0) {
			const written = this.socket.write(bytes);
			if (written < 0)
				throw new AdbError("closed", "write after the connection closed");
			if (written === bytes.length) return;
			this.pendingWrites.push(bytes.subarray(written));
		} else {
			this.pendingWrites.push(bytes);
		}
		await new Promise<void>((resolve, reject) =>
			this.drainWaiters.push({ resolve, reject }),
		);
	}

	close(): void {
		this.detach();
		if (!this.closed) this.ended = true;
		this.socket?.end();
		this.wake();
	}

	destroy(error: AdbError): void {
		this.fail(error);
		this.socket?.terminate();
	}

	private take(length: number): Uint8Array {
		const out = new Uint8Array(length);
		let offset = 0;
		while (offset < length) {
			const head = this.chunks[0];
			if (!head) break;
			const needed = length - offset;
			if (head.length <= needed) {
				out.set(head, offset);
				offset += head.length;
				this.chunks.shift();
			} else {
				out.set(head.subarray(0, needed), offset);
				this.chunks[0] = head.subarray(needed);
				offset += needed;
			}
		}
		this.buffered -= length;
		if (this.paused && this.buffered < HIGH_WATER / 2) {
			this.paused = false;
			this.socket.resume();
		}
		return out;
	}

	private wait(): Promise<void> {
		return new Promise((resolve, reject) => {
			this.waiter = { resolve, reject };
		});
	}

	private wake(): void {
		const waiter = this.waiter;
		this.waiter = undefined;
		if (!waiter) return;
		if (this.failure) waiter.reject(this.failure);
		else waiter.resolve();
	}

	private onData(chunk: Uint8Array): void {
		// Bun reuses the chunk's memory after the callback returns.
		this.chunks.push(chunk.slice());
		this.buffered += chunk.length;
		if (!this.paused && this.buffered >= HIGH_WATER) {
			this.paused = true;
			this.socket.pause();
		}
		this.wake();
	}

	private onDrain(): void {
		while (this.pendingWrites.length > 0) {
			const head = this.pendingWrites[0];
			if (!head) break;
			const written = this.socket.write(head);
			if (written < 0) {
				this.fail(new AdbError("closed", "connection closed while writing"));
				return;
			}
			if (written < head.length) {
				this.pendingWrites[0] = head.subarray(written);
				return;
			}
			this.pendingWrites.shift();
		}
		for (const waiter of this.drainWaiters.splice(0)) waiter.resolve();
	}

	private onEnd(): void {
		this.ended = true;
		for (const waiter of this.drainWaiters.splice(0))
			waiter.reject(new AdbError("closed", "connection closed while writing"));
		this.wake();
	}

	private fail(error: AdbError): void {
		if (this.failure) return;
		this.detach();
		this.failure = error;
		for (const waiter of this.drainWaiters.splice(0)) waiter.reject(error);
		this.wake();
	}
}

function abortError(signal: AbortSignal): AdbError {
	const reason = signal.reason;
	if (reason instanceof DOMException && reason.name === "TimeoutError")
		return new AdbError("timeout", "the adb operation timed out");
	return new AdbError("aborted", "the adb operation was cancelled", {
		cause: reason,
	});
}

export function concat(parts: Uint8Array[]): Uint8Array {
	if (parts.length === 1 && parts[0]) return parts[0];
	const total = parts.reduce((sum, part) => sum + part.length, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.length;
	}
	return out;
}
