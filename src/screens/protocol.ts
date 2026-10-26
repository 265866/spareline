// scrcpy-server 3.3.3 wire format, as the relay uses it: server arguments,
// the video socket's handshake and packet framing, and the control messages
// a browser may send.

export const SCRCPY_VERSION = "3.3.3";
// CleanUp (the server's settings-restoring child) runs with this exact
// CLASSPATH, so the jar must live here and nowhere else.
export const SERVER_PATH = "/data/local/tmp/scrcpy-server.jar";

// Native resolution (max_size=0) at up to 30 fps, measured in spike S1
// (architecture.md D3).
export const VIDEO_BIT_RATE = 4_000_000;

// A random 31-bit session id as 8 hex digits; the server names its abstract
// socket scrcpy_<scid>, so concurrent sessions on different phones never meet.
export function newScid(): string {
	const value = crypto.getRandomValues(new Uint32Array(1))[0] ?? 0;
	return (value & 0x7fffffff).toString(16).padStart(8, "0");
}

export function serverArgs(scid: string): string[] {
	return [
		`scid=${scid}`,
		"log_level=info",
		"video=true",
		"audio=false",
		"control=true",
		"tunnel_forward=true",
		"max_size=0",
		"max_fps=30",
		`video_bit_rate=${VIDEO_BIT_RATE}`,
		"video_codec=h264",
		"stay_awake=true",
		"clipboard_autosync=false",
		"send_device_meta=true",
		"send_dummy_byte=true",
	];
}

export function serverCommand(scid: string): string {
	return `CLASSPATH=${SERVER_PATH} app_process / com.genymobile.scrcpy.Server ${SCRCPY_VERSION} ${serverArgs(scid).join(" ")} 2>&1`;
}

// A pattern for `pgrep -f`/`pkill -f` that matches the server's command line
// but not the `sh -c` running the pgrep itself: "scid=1234abc[d]" matches
// "scid=1234abcd", while the shell's own argv holds the bracketed text.
export function scidPattern(scid: string): string {
	return `scid=${scid.slice(0, -1)}[${scid.slice(-1)}]`;
}
// Any scrcpy process: a server, or CleanUp restoring settings after one.
export const ANY_SCRCPY_PATTERN = "com.genymobile.scrcp[y]";
// Any scrcpy server, whatever its scid; CleanUp does not match.
export const ANY_SERVER_PATTERN = "com.genymobile.scrcpy.Serve[r]";

export const DEVICE_NAME_LENGTH = 64;
export const CODEC_HEADER_LENGTH = 12;
export const PACKET_HEADER_LENGTH = 12;
// Far above any real frame (a native 1080x2400 keyframe is a few hundred KB);
// a larger size means a corrupt stream, not a frame to buffer.
export const MAX_PACKET_BYTES = 8 * 1024 * 1024;
export const CODEC_H264 = 0x68_32_36_34; // "h264"

export type VideoMeta = {
	deviceName: string;
	codec: number;
	width: number;
	height: number;
};

// The 64-byte NUL-padded device name, then codec id, width, and height as
// big-endian u32s.
export function parseVideoMeta(bytes: Uint8Array): VideoMeta {
	if (bytes.length !== DEVICE_NAME_LENGTH + CODEC_HEADER_LENGTH)
		throw new Error(`video meta must be 76 bytes, got ${bytes.length}`);
	const name = bytes.subarray(0, DEVICE_NAME_LENGTH);
	const end = name.indexOf(0);
	const view = new DataView(
		bytes.buffer,
		bytes.byteOffset + DEVICE_NAME_LENGTH,
		CODEC_HEADER_LENGTH,
	);
	return {
		deviceName: new TextDecoder().decode(
			end < 0 ? name : name.subarray(0, end),
		),
		codec: view.getUint32(0),
		width: view.getUint32(4),
		height: view.getUint32(8),
	};
}

// One video packet exactly as the server sent it (12-byte header plus
// payload), which is also what a browser viewer receives.
export type VideoPacket = {
	bytes: Uint8Array;
	config: boolean;
	keyframe: boolean;
};

// Header: u64 BE whose bit 63 marks a config packet (SPS/PPS), bit 62 a
// keyframe, and the rest the PTS; then the payload size as a u32 BE.
export function parsePacketHeader(header: Uint8Array): {
	config: boolean;
	keyframe: boolean;
	size: number;
} {
	const view = new DataView(
		header.buffer,
		header.byteOffset,
		PACKET_HEADER_LENGTH,
	);
	const high = view.getUint32(0);
	return {
		config: (high & 0x8000_0000) !== 0,
		keyframe: (high & 0x4000_0000) !== 0,
		size: view.getUint32(8),
	};
}

// Exact-length reads over a byte stream that arrives in arbitrary chunks.
export class StreamReader {
	private readonly reader: ReadableStreamDefaultReader<Uint8Array>;
	private chunks: Uint8Array[] = [];
	private buffered = 0;

	constructor(stream: ReadableStream<Uint8Array>) {
		this.reader = stream.getReader();
	}

	// Exactly `length` bytes; throws if the stream ends first.
	async read(length: number): Promise<Uint8Array> {
		while (this.buffered < length) {
			const { value, done } = await this.reader.read();
			if (done)
				throw new Error(
					`stream ended after ${this.buffered} of ${length} expected bytes`,
				);
			this.chunks.push(value);
			this.buffered += value.length;
		}
		const out = new Uint8Array(length);
		let offset = 0;
		while (offset < length) {
			const head = this.chunks[0] as Uint8Array;
			const take = Math.min(head.length, length - offset);
			out.set(head.subarray(0, take), offset);
			offset += take;
			if (take === head.length) this.chunks.shift();
			else this.chunks[0] = head.subarray(take);
		}
		this.buffered -= length;
		return out;
	}

	// The next packet, or null when the stream ends cleanly between packets.
	async readPacket(): Promise<VideoPacket | null> {
		if (this.buffered === 0) {
			const { value, done } = await this.reader.read();
			if (done) return null;
			this.chunks.push(value);
			this.buffered += value.length;
		}
		const header = await this.read(PACKET_HEADER_LENGTH);
		const { config, keyframe, size } = parsePacketHeader(header);
		if (size > MAX_PACKET_BYTES)
			throw new Error(
				`video packet of ${size} bytes exceeds ${MAX_PACKET_BYTES}`,
			);
		const bytes = new Uint8Array(PACKET_HEADER_LENGTH + size);
		bytes.set(header, 0);
		bytes.set(await this.read(size), PACKET_HEADER_LENGTH);
		return { bytes, config, keyframe };
	}

	cancel(): void {
		this.reader.cancel().catch(() => {});
	}
}

// Control message type bytes (scrcpy ControlMessage.TYPE_*).
export const ControlType = {
	injectKeycode: 0,
	injectText: 1,
	injectTouch: 2,
	injectScroll: 3,
	backOrScreenOn: 4,
	setDisplayPower: 10,
	resetVideo: 17,
} as const;

// scrcpy's own client limit for one INJECT_TEXT message.
export const MAX_TEXT_BYTES = 300;
// The largest WebSocket message a viewer may send; the longest allowed
// control message is 305 bytes.
export const MAX_VIEWER_MESSAGE_BYTES = 4096;

export type ControlKind = "input" | "text";

// The control messages a browser may send, each with its exact length. All
// others (display power, clipboard, app start, UHID, rotation, panels) are
// refused, and a wrong length would desynchronize the server's parser, so it
// is refused too.
export function classifyControl(message: Uint8Array): ControlKind | null {
	const length = message.length;
	switch (message[0]) {
		case ControlType.injectKeycode:
			return length === 14 ? "input" : null;
		case ControlType.injectText: {
			if (length < 5 || length > 5 + MAX_TEXT_BYTES) return null;
			const declared = new DataView(
				message.buffer,
				message.byteOffset,
				length,
			).getUint32(1);
			return declared === length - 5 ? "text" : null;
		}
		case ControlType.injectTouch:
			return length === 32 ? "input" : null;
		case ControlType.injectScroll:
			return length === 21 ? "input" : null;
		case ControlType.backOrScreenOn:
			return length === 2 ? "input" : null;
		default:
			return null;
	}
}

export const RESET_VIDEO = new Uint8Array([ControlType.resetVideo]);

// A dozing phone shows scrcpy a dark display, and a swipe keyguard covers the
// screen, so nothing would change and no frames would flow. Waking it and
// dismissing the keyguard is harmless and touches no radio. (A keyguard with
// a PIN would stay up, so phones used here should have none.)
export const WAKE_COMMAND =
	"input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard";

// One cheap read of whether the phone is awake (about 56 ms through adb on
// the B30, measured).
export const WAKEFULNESS_COMMAND = "dumpsys power | grep -m1 mWakefulness=";

// "Awake", "Dozing", "Asleep", or "Dreaming"; null when unreadable.
export function parseWakefulness(output: string): string | null {
	return /mWakefulness=(\w+)/.exec(output)?.[1] ?? null;
}

// The relay sends this itself, to keep the panel dark during a session.
export function setDisplayPower(on: boolean): Uint8Array {
	return new Uint8Array([ControlType.setDisplayPower, on ? 1 : 0]);
}
