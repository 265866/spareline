import {
	type AndroidKeyCode,
	AndroidKeyEventAction,
	AndroidKeyEventMeta,
	AndroidMotionEventAction,
	ScrcpyControlMessageSerializer,
	type ScrcpyMediaStreamPacket,
	ScrcpyOptions3_3_3,
	ScrcpyPointerId,
	ScrcpyVideoCodecId,
} from "@yume-chan/scrcpy";
import {
	BitmapVideoFrameRenderer,
	type VideoFrameRenderer,
	WebCodecsVideoDecoder,
	WebGLVideoFrameRenderer,
} from "@yume-chan/scrcpy-decoder-webcodecs";
import { ReadableStream, type WritableStream } from "@yume-chan/stream-extra";
import type { ScreenMessage } from "../src/screens/manager";
import { SIGNED_OUT_EVENT } from "./api";

export type StreamPhase =
	| "connecting"
	| "starting"
	| "live"
	| "restarting"
	| "offline"
	| "failed"
	| "unsupported";

export type StreamState = {
	phase: StreamPhase;
	width: number;
	height: number;
	fps: number;
	kbps: number;
	message: string | null;
	// Stopped after repeated failures; retry() starts again.
	stopped: boolean;
};

// What the stream needs from a decoder; WebCodecsVideoDecoder fits.
export type FrameDecoder = {
	readonly writable: WritableStream<ScrcpyMediaStreamPacket>;
	sizeChanged(
		listener: (size: { width: number; height: number }) => void,
	): unknown;
	dispose(): void;
};

// The browser pieces a ScreenStream uses, replaceable in tests.
export type ScreenStreamDeps = {
	supported: boolean;
	createRenderer(canvas: HTMLCanvasElement): VideoFrameRenderer;
	createDecoder(renderer: VideoFrameRenderer): FrameDecoder;
	openSocket(path: string): WebSocket;
	schedule(run: () => void, ms: number): ReturnType<typeof setTimeout>;
	// Calls hide when the page is left (also into the back/forward cache,
	// where it would otherwise keep its socket open) and show when it returns.
	watchPage(hide: () => void, show: () => void): () => void;
	// Whether the page is a secure context, for the unsupported-browser
	// message.
	page(): { isSecureContext: boolean };
	// The server closed the screen because the session ended.
	signedOut(): void;
};

// Only frame parsing and control serialization depend on these; they match
// the arguments the relay starts scrcpy-server 3.3.3 with.
const options = new ScrcpyOptions3_3_3({
	audio: false,
	tunnelForward: true,
	control: true,
	clipboardAutosync: false,
});
const serializer = new ScrcpyControlMessageSerializer(options);
const encoder = new TextEncoder();

// scrcpy-server's limit for one INJECT_TEXT message; the relay's allow-list
// (src/screens/protocol.ts) enforces the same value, which a test checks.
export const MAX_TEXT_BYTES = 300;

const webCodecsSupported = WebCodecsVideoDecoder.isSupported;

// Asked once per page: the check itself creates a WebGL context, and a page
// gets only about 16 before Chrome starts dropping them.
let webGLSupported: boolean | undefined;

const browserDeps: ScreenStreamDeps = {
	supported: webCodecsSupported,
	// WebGL is fastest; ImageBitmap works where WebGL does not.
	createRenderer: (canvas) => {
		webGLSupported ??= WebGLVideoFrameRenderer.isSupported;
		return webGLSupported
			? new WebGLVideoFrameRenderer(canvas)
			: new BitmapVideoFrameRenderer(canvas);
	},
	createDecoder: (renderer) =>
		new WebCodecsVideoDecoder({ codec: ScrcpyVideoCodecId.H264, renderer }),
	openSocket: (path) =>
		new WebSocket(
			`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${path}`,
		),
	schedule: (run, ms) => setTimeout(run, ms),
	page: () => window,
	signedOut: () => window.dispatchEvent(new Event(SIGNED_OUT_EVENT)),
	watchPage(hide, show) {
		const onShow = (event: PageTransitionEvent) => {
			if (event.persisted) show();
		};
		window.addEventListener("pagehide", hide);
		window.addEventListener("pageshow", onShow);
		return () => {
			window.removeEventListener("pagehide", hide);
			window.removeEventListener("pageshow", onShow);
		};
	},
};

// WebCodecs needs a current browser and a secure context (HTTPS, or
// localhost). No address is built from the current host: the server itself
// speaks plain HTTP, and only the reverse proxy in front of it adds HTTPS,
// at an address the page cannot know.
export function unsupportedMessage(page: { isSecureContext: boolean }): string {
	return page.isSecureContext
		? "This browser can't play the video. Open this site in a current Chrome, Edge, or Safari."
		: "This browser can't play the video over plain HTTP. Open this site through its HTTPS address (the reverse proxy in front of the server) in Chrome, Edge, or Safari.";
}

// The relay's close code for a viewer whose session ended
// (src/screens/routes.ts); retrying would only be refused.
const CLOSE_SIGNED_OUT = 4003;

// Reconnect delays double from 1 s up to this.
const MAX_RETRY_MS = 30_000;
// Consecutive decode failures, with no frame drawn in between, after which
// the stream stops and waits for retry().
const MAX_DECODE_FAILURES = 3;

// One phone's live screen on one canvas: the relay's WebSocket, the scrcpy
// packet parser, and a WebCodecs decoder. It reconnects with backoff until
// disposed, and stops after repeated decode failures.
export class ScreenStream {
	private ws: WebSocket | null = null;
	private decoder: FrameDecoder | null = null;
	// One per stream: each renderer holds a canvas context.
	private renderer: VideoFrameRenderer | null = null;
	private retryTimer: ReturnType<typeof setTimeout> | undefined;
	// Reconnects since a frame was last drawn.
	private attempts = 0;
	private decodeFailures = 0;
	private decodeFailed = false;
	private disposed = false;
	private readonly deps: ScreenStreamDeps;
	private unwatchPage = () => {};
	private hidden = false;
	state: StreamState = {
		phase: "connecting",
		width: 0,
		height: 0,
		fps: 0,
		kbps: 0,
		message: null,
		stopped: false,
	};

	constructor(
		private readonly canvas: HTMLCanvasElement,
		private readonly serial: string,
		private readonly onState: (state: StreamState) => void,
		deps: Partial<ScreenStreamDeps> = {},
	) {
		this.deps = { ...browserDeps, ...deps };
		if (!this.deps.supported) {
			this.update({
				phase: "unsupported",
				message: unsupportedMessage(this.deps.page()),
			});
			return;
		}
		this.unwatchPage = this.deps.watchPage(
			() => this.hide(),
			() => this.show(),
		);
		this.connect();
	}

	// Starts again after the stream stopped on repeated failures.
	retry(): void {
		if (this.disposed || this.ws) return;
		clearTimeout(this.retryTimer);
		this.attempts = 0;
		this.decodeFailures = 0;
		this.connect();
	}

	dispose(): void {
		this.disposed = true;
		this.unwatchPage();
		this.disconnect();
	}

	private disconnect(): void {
		clearTimeout(this.retryTimer);
		this.ws?.close(1000);
		this.ws = null;
		this.decoder?.dispose();
		this.decoder = null;
	}

	private hide(): void {
		this.hidden = true;
		this.disconnect();
	}

	private show(): void {
		if (!this.hidden || this.disposed) return;
		this.hidden = false;
		if (this.state.stopped) return;
		this.attempts = 0;
		this.connect();
	}

	// Touch at video coordinates. Mouse input uses the generic finger id;
	// touchscreens pass their pointer ids so multi-touch works.
	touch(
		action: AndroidMotionEventAction,
		x: number,
		y: number,
		pointerId: bigint = ScrcpyPointerId.Finger,
	): void {
		const { width, height } = this.state;
		if (!width || !height) return;
		this.send(
			serializer.injectTouch({
				action,
				pointerId,
				pointerX: clamp(x, 0, width - 1),
				pointerY: clamp(y, 0, height - 1),
				videoWidth: width,
				videoHeight: height,
				pressure: action === AndroidMotionEventAction.Up ? 0 : 1,
				actionButton: 0,
				buttons: 0,
			}),
		);
	}

	scroll(x: number, y: number, deltaX: number, deltaY: number): void {
		const { width, height } = this.state;
		if (!width || !height) return;
		this.send(
			serializer.injectScroll({
				pointerX: clamp(x, 0, width - 1),
				pointerY: clamp(y, 0, height - 1),
				videoWidth: width,
				videoHeight: height,
				scrollX: clamp(-deltaX / 100, -16, 16),
				scrollY: clamp(-deltaY / 100, -16, 16),
				buttons: 0,
			}),
		);
	}

	// An Android key code, pressed and released.
	key(keyCode: number): void {
		for (const action of [AndroidKeyEventAction.Down, AndroidKeyEventAction.Up])
			this.send(
				serializer.injectKeyCode({
					action,
					keyCode: keyCode as AndroidKeyCode,
					repeat: 0,
					metaState: AndroidKeyEventMeta.None,
				}),
			);
	}

	// Types text, split into messages of at most 300 UTF-8 bytes.
	text(value: string): void {
		let chunk = "";
		for (const char of value) {
			if (encoder.encode(chunk + char).length > MAX_TEXT_BYTES) {
				this.send(serializer.injectText(chunk));
				chunk = "";
			}
			chunk += char;
		}
		if (chunk) this.send(serializer.injectText(chunk));
	}

	private send(message: Uint8Array | undefined): void {
		if (message && this.ws?.readyState === WebSocket.OPEN)
			this.ws.send(message as Uint8Array<ArrayBuffer>);
	}

	private update(patch: Partial<StreamState>): void {
		this.state = { ...this.state, ...patch };
		this.onState(this.state);
	}

	private connect(): void {
		if (this.disposed || this.hidden) return;

		// A new decoder per connection, so nothing a failed one left behind
		// carries over. Both are set up before the socket opens: if either
		// throws (say the canvas lost its WebGL context), there is no socket
		// left open without handlers, and the stream waits for retry().
		this.decoder?.dispose();
		this.decoder = null;
		let decoder: FrameDecoder;
		try {
			this.renderer ??= this.deps.createRenderer(this.canvas);
			const renderer = this.renderer;
			decoder = this.deps.createDecoder({
				setSize: (width, height) => renderer.setSize(width, height),
				draw: (frame) => {
					// A drawn frame is the proof that this connection works.
					this.attempts = 0;
					this.decodeFailures = 0;
					return renderer.draw(frame);
				},
			});
		} catch (error) {
			console.warn("screen video could not be set up", error);
			this.renderer = null;
			this.update({
				phase: "failed",
				message: "This browser could not set up the video.",
				stopped: true,
			});
			return;
		}
		this.decoder = decoder;
		decoder.sizeChanged(({ width, height }) => this.update({ width, height }));

		const ws = this.deps.openSocket(
			`/api/v1/phones/${encodeURIComponent(this.serial)}/screen`,
		);
		ws.binaryType = "arraybuffer";
		this.ws = ws;
		this.decodeFailed = false;
		this.update({ phase: "connecting", message: null, stopped: false });

		const packets = new ReadableStream<Uint8Array>({
			start: (controller) => {
				ws.onmessage = (event: MessageEvent<ArrayBuffer | string>) => {
					if (typeof event.data === "string")
						this.onMessage(JSON.parse(event.data) as ScreenMessage);
					else {
						controller.enqueue(new Uint8Array(event.data));
					}
				};
				ws.onclose = (event) => {
					try {
						controller.close();
					} catch {
						// Already closed by the decoder.
					}
					this.onClose(ws, event);
				};
			},
		});
		packets
			.pipeThrough(options.createMediaStreamTransformer())
			.pipeTo(decoder.writable)
			.catch((error: unknown) => {
				// A decoder error leaves a frozen picture, so start over with a
				// fresh socket and decoder. After dispose() there is nothing to do.
				if (this.disposed || this.ws !== ws) return;
				console.warn("screen decoding failed", error);
				this.decodeFailed = true;
				ws.close(4900, "decoder error");
			});
	}

	private onMessage(message: ScreenMessage): void {
		switch (message.t) {
			case "hello":
				this.update({
					phase: "live",
					width: message.width,
					height: message.height,
					message: null,
				});
				break;
			case "status":
				this.update({ phase: message.state });
				break;
			case "stats":
				this.update({ fps: message.fps, kbps: message.kbps });
				break;
		}
	}

	private onClose(ws: WebSocket, event: CloseEvent): void {
		if (this.disposed || this.ws !== ws) return;
		this.ws = null;
		if (this.decodeFailed && ++this.decodeFailures >= MAX_DECODE_FAILURES) {
			this.update({
				phase: "failed",
				message: "This screen's video could not be decoded.",
				stopped: true,
			});
			return;
		}
		if (event.code === CLOSE_SIGNED_OUT) {
			this.update({ phase: "failed", message: "Signed out", stopped: true });
			this.deps.signedOut();
			return;
		}
		let delay = Math.min(MAX_RETRY_MS, 1_000 * 2 ** this.attempts++);
		if (event.code === 4001) {
			this.update({ phase: "offline", message: event.reason || null });
			delay = Math.max(delay, 5_000);
		} else if (event.code === 4002) {
			this.update({ phase: "failed", message: event.reason || null });
			delay = Math.max(delay, 10_000);
		} else this.update({ phase: "connecting" });
		this.retryTimer = this.deps.schedule(() => this.connect(), delay);
	}
}

function clamp(value: number, min: number, max: number): number {
	return Math.min(max, Math.max(min, value));
}
