import { join } from "node:path";

export type Transcript = {
	description: string;
	outcome: string;
	connections: { from: "client" | "server"; hex: string }[][];
};

export async function loadTranscript(name: string): Promise<Transcript> {
	return Bun.file(join(import.meta.dir, "fixtures/adb", `${name}.json`)).json();
}

export type Replay = {
	port: number;
	// Mismatches between what the client sent and what the recording expects.
	mismatches: string[];
	stop(): void;
};

// A fake adb server that plays back a recorded transcript: for the Nth
// connection it waits for the recorded client bytes, checks them, and answers
// with the recorded server bytes. `rechunk: 1` splits every server chunk into
// single bytes to exercise the client's framing.
export function replay(
	transcript: Transcript,
	options: { rechunk?: number } = {},
): Replay {
	const mismatches: string[] = [];
	let next = 0;
	type State = { received: number[]; closed: boolean; wake?: () => void };
	const server = Bun.listen<State>({
		hostname: "127.0.0.1",
		port: 0,
		socket: {
			open(socket) {
				socket.data = { received: [], closed: false };
				const events = transcript.connections[next++];
				if (!events) {
					mismatches.push(`unexpected connection ${next}`);
					socket.end();
					return;
				}
				void (async () => {
					let offset = 0;
					for (const event of events) {
						const bytes = Buffer.from(event.hex, "hex");
						if (event.from === "client") {
							// No idle timer: the client either sends the bytes or closes, and
							// a client that does neither fails on the test's own timeout.
							while (
								socket.data.received.length < offset + bytes.length &&
								!socket.data.closed
							)
								await new Promise<void>((resolve) => {
									socket.data.wake = resolve;
								});
							if (socket.data.received.length < offset + bytes.length) {
								mismatches.push(
									`client closed after ${socket.data.received.length - offset} of ${bytes.length} expected bytes`,
								);
								return;
							}
							const got = Buffer.from(
								socket.data.received.slice(offset, offset + bytes.length),
							);
							if (!got.equals(bytes)) {
								const at = got.findIndex((byte, i) => byte !== bytes[i]);
								const show = (b: Buffer) =>
									JSON.stringify(b.subarray(at, at + 24).toString("latin1"));
								mismatches.push(
									`client bytes differ at offset ${offset + at}: sent ${show(got)}, recorded ${show(bytes)}`,
								);
							}
							offset += bytes.length;
						} else {
							const size = options.rechunk ?? bytes.length;
							for (let i = 0; i < bytes.length; i += size) {
								socket.write(bytes.subarray(i, i + size));
								// setImmediate, not Bun.sleep(0): on Windows a timer yield costs
								// about 1.5 ms per byte even on an idle machine.
								if (options.rechunk)
									await new Promise((resolve) => setImmediate(resolve));
							}
						}
					}
					socket.end();
				})();
			},
			data(socket, chunk) {
				socket.data.received.push(...chunk);
				socket.data.wake?.();
			},
			end(socket) {
				socket.data.closed = true;
				socket.data.wake?.();
			},
			close(socket) {
				socket.data.closed = true;
				socket.data.wake?.();
			},
		},
	});
	return { port: server.port, mismatches, stop: () => server.stop(true) };
}
