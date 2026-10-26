import { afterEach, beforeEach, expect, test } from "bun:test";
import { join } from "node:path";
import { WritableStream } from "@yume-chan/stream-extra";
import {
	type FrameDecoder,
	ScreenStream,
	type ScreenStreamDeps,
	type StreamState,
} from "./screen-stream";

// ScreenStream against a WebSocket server that answers like the relay (a
// hello, then the scrcpy stream recorded from the B30), with a stand-in
// decoder: Bun has no WebCodecs, and these tests need a decoder that fails
// on demand.

const recorded = new Uint8Array(
	await Bun.file(
		join(import.meta.dir, "../test/fixtures/screen/grid-home.bin"),
	).arrayBuffer(),
);

let server: ReturnType<typeof Bun.serve>;
let sockets: { close(code?: number): void }[];
let closed: number;
let streams: ScreenStream[];

beforeEach(() => {
	sockets = [];
	closed = 0;
	streams = [];
	server = Bun.serve({
		hostname: "127.0.0.1",
		port: 0,
		fetch: (request, srv) =>
			srv.upgrade(request, { data: undefined })
				? undefined
				: new Response("", { status: 400 }),
		websocket: {
			open(ws) {
				sockets.push(ws);
				ws.send(
					JSON.stringify({
						t: "hello",
						width: 216,
						height: 480,
					}),
				);
				// The packets after the dummy byte and video header.
				ws.send(recorded.subarray(77));
			},
			message() {},
			close() {
				closed++;
			},
		},
	});
});

afterEach(() => {
	for (const stream of streams) stream.dispose();
	server.stop(true);
});

// A decoder that fails on its first packet, or draws a frame for each data
// packet through the renderer it was given.
function decoder(fails: boolean) {
	return (renderer: Parameters<ScreenStreamDeps["createDecoder"]>[0]) =>
		({
			writable: new WritableStream({
				async write(packet) {
					if (fails) throw new Error("decoder not configured");
					if (packet.type === "data") await renderer.draw({} as VideoFrame);
				},
			}),
			sizeChanged: () => {},
			dispose: () => {},
		}) satisfies FrameDecoder;
}

function start(plan: boolean[], overrides: Partial<ScreenStreamDeps> = {}) {
	const delays: number[] = [];
	const page = { hide: () => {}, show: () => {} };
	let connection = 0;
	let renderers = 0;
	const stream = new ScreenStream(
		{} as HTMLCanvasElement,
		"SPARELINE00001",
		() => {},
		{
			supported: true,
			createRenderer: () => {
				renderers++;
				return { setSize() {}, draw() {} };
			},
			// Each connection takes the next entry of the plan: fail or work.
			createDecoder: (renderer) =>
				decoder(plan[Math.min(connection++, plan.length - 1)] ?? true)(
					renderer,
				),
			openSocket: (path) =>
				new WebSocket(`ws://127.0.0.1:${server.port}${path}`),
			schedule: (run, ms) => {
				delays.push(ms);
				return setTimeout(run, 0);
			},
			watchPage: (hide, show) => {
				page.hide = hide;
				page.show = show;
				return () => {};
			},
			...overrides,
		},
	);
	streams.push(stream);
	return {
		stream,
		delays,
		page,
		connections: () => connection,
		renderers: () => renderers,
	};
}

async function until(check: () => boolean, ms = 3_000): Promise<void> {
	const deadline = Date.now() + ms;
	while (!check()) {
		if (Date.now() > deadline) throw new Error("timed out waiting");
		await Bun.sleep(5);
	}
}

test("a decoder that always fails backs off and stops after three failures", async () => {
	const { stream, delays, connections, renderers } = start([true]);
	await until(() => stream.state.stopped);
	expect(delays).toEqual([1_000, 2_000]);
	expect(connections()).toBe(3);
	// One renderer (one canvas context) for all of them.
	expect(renderers()).toBe(1);
	expect(stream.state.phase).toBe("failed");
	await Bun.sleep(50);
	expect(connections()).toBe(3);

	// Retry starts over, with the same limit.
	stream.retry();
	await until(() => connections() === 4);
	expect(stream.state.stopped).toBe(false);
	await until(() => stream.state.stopped);
	expect(connections()).toBe(6);
	expect(delays).toEqual([1_000, 2_000, 1_000, 2_000]);
});

test("a drawn frame resets the backoff and the failure count", async () => {
	const { stream, delays, connections } = start([
		true,
		true,
		false,
		true,
		true,
		false,
	]);
	await until(() => connections() === 3 && stream.state.phase === "live");
	await Bun.sleep(50);
	// The relay drops the working connection.
	sockets[2]?.close(1011);
	await until(() => connections() === 6);
	await Bun.sleep(50);
	expect(delays).toEqual([1_000, 2_000, 1_000, 2_000, 4_000]);
	expect(stream.state.stopped).toBe(false);
	expect(stream.state.phase).toBe("live");
});

test("backoff is capped at 30 s", async () => {
	const { stream, delays } = start([false]);
	await until(() => stream.state.phase === "live");
	// Hello alone does not reset the backoff; only a drawn frame does. Here
	// the stream never connects successfully after the first time.
	server.stop(true);
	await until(() => delays.length >= 7, 5_000);
	expect(delays.slice(0, 7)).toEqual([
		1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000,
	]);
});

test("leaving the page closes the socket, and coming back reconnects", async () => {
	const { stream, page, connections } = start([false]);
	await until(() => stream.state.phase === "live");
	page.hide();
	await until(() => closed === 1);
	await Bun.sleep(50);
	expect(connections()).toBe(1);
	page.show();
	await until(() => connections() === 2 && stream.state.phase === "live");
});

for (const part of ["createRenderer", "createDecoder"] as const)
	test(`when ${part} throws, no socket opens and the stream waits for retry`, async () => {
		let broken = true;
		const fail = () => {
			if (broken) throw new Error("WebGL context lost");
		};
		const overrides: Partial<ScreenStreamDeps> =
			part === "createRenderer"
				? {
						createRenderer: () => {
							fail();
							return { setSize() {}, draw() {} };
						},
					}
				: {
						createDecoder: (renderer) => {
							fail();
							return decoder(false)(renderer);
						},
					};
		const { stream } = start([false], overrides);
		await Bun.sleep(100);
		expect(sockets).toHaveLength(0);
		expect(stream.state).toMatchObject({ phase: "failed", stopped: true });

		broken = false;
		stream.retry();
		await until(() => stream.state.phase === "live");
		expect(sockets).toHaveLength(1);
	});

test("a browser without WebCodecs is told why, and no socket opens", async () => {
	for (const [page, message] of [
		[
			{ isSecureContext: true },
			"This browser can't play the video. Open this site in a current Chrome, Edge, or Safari.",
		],
		[
			{ isSecureContext: false },
			"This browser can't play the video over plain HTTP. Open this site through its HTTPS tailnet address in Chrome, Edge, or Safari.",
		],
	] as const) {
		const states: StreamState[] = [];
		const stream = new ScreenStream(
			{} as HTMLCanvasElement,
			"SPARELINE00001",
			(state) => states.push(state),
			{
				supported: false,
				page: () => page,
				openSocket: (path) =>
					new WebSocket(`ws://127.0.0.1:${server.port}${path}`),
			},
		);
		streams.push(stream);
		expect(states.at(-1)).toMatchObject({ phase: "unsupported", message });
	}
	await Bun.sleep(100);
	expect(sockets).toHaveLength(0);
});
