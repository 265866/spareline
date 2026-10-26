import { describe, expect, test } from "bun:test";
import { join } from "node:path";
import {
	AndroidKeyCode,
	AndroidKeyEventAction,
	AndroidMotionEventAction,
	AndroidScreenPowerMode,
	ScrcpyControlMessageSerializer,
	ScrcpyOptions3_3_3,
	ScrcpyPointerId,
} from "@yume-chan/scrcpy";
import {
	CODEC_H264,
	classifyControl,
	MAX_PACKET_BYTES,
	MAX_TEXT_BYTES,
	parseVideoMeta,
	parseWakefulness,
	RESET_VIDEO,
	StreamReader,
	scidPattern,
	serverArgs,
	setDisplayPower,
	type VideoPacket,
} from "./protocol";

// test/fixtures/screen/grid-home.bin is every byte of a real scrcpy-server
// 3.3.3 video socket on the B30's home screen (scripts/record-screen-fixture.ts),
// with a RESET_VIDEO sent halfway.
const FIXTURE = join(import.meta.dir, "../../test/fixtures/screen/grid-home");
const recorded = new Uint8Array(await Bun.file(`${FIXTURE}.bin`).arrayBuffer());
const summary = (await Bun.file(`${FIXTURE}.json`).json()) as {
	packets: number;
	configs: number;
	keyframes: number;
};

function chunked(bytes: Uint8Array, size: (i: number) => number) {
	return new ReadableStream<Uint8Array>({
		start(controller) {
			for (let offset = 0, i = 0; offset < bytes.length; i++) {
				const n = size(i);
				controller.enqueue(bytes.slice(offset, offset + n));
				offset += n;
			}
			controller.close();
		},
	});
}

async function parse(stream: ReadableStream<Uint8Array>) {
	const reader = new StreamReader(stream);
	const dummy = await reader.read(1);
	const meta = parseVideoMeta(await reader.read(76));
	const packets: VideoPacket[] = [];
	for (
		let packet = await reader.readPacket();
		packet;
		packet = await reader.readPacket()
	)
		packets.push(packet);
	return { dummy: dummy[0], meta, packets };
}

// A fixed-seed generator, so a failing chunking can be reproduced.
function lcg(seed: number) {
	let state = seed;
	return () => {
		state = (state * 1_103_515_245 + 12_345) % 2 ** 31;
		return state;
	};
}

describe("video stream framing on recorded bytes", () => {
	const splits = {
		"as one chunk": () => recorded.length,
		"one byte at a time": () => 1,
		"in random chunks": (() => {
			const next = lcg(7);
			return () => 1 + (next() % 4_000);
		})(),
	};
	for (const [name, size] of Object.entries(splits))
		test(name, async () => {
			const { dummy, meta, packets } = await parse(chunked(recorded, size));
			expect(dummy).toBe(0);
			expect(meta).toEqual({
				deviceName: "S6702X",
				codec: CODEC_H264,
				width: 216,
				height: 480,
			});
			expect(packets).toHaveLength(summary.packets);
			expect(packets.filter((p) => p.config)).toHaveLength(summary.configs);
			expect(packets.filter((p) => p.keyframe)).toHaveLength(summary.keyframes);
			// The stream opens with a config packet, and a keyframe follows each
			// config packet (also the one RESET_VIDEO produced).
			expect(packets[0]?.config).toBe(true);
			packets.forEach((packet, i) => {
				if (packet.config) expect(packets[i + 1]?.keyframe).toBe(true);
			});
			// The relay forwards packets byte for byte.
			const relayed = Buffer.concat(packets.map((p) => p.bytes));
			expect(relayed.equals(Buffer.from(recorded.subarray(77)))).toBe(true);
		});

	test("a stream cut mid-packet is an error, not a short packet", async () => {
		const cut = recorded.subarray(0, recorded.length - 10);
		await expect(parse(chunked(cut, () => 1_000))).rejects.toThrow(
			"stream ended after",
		);
	});
});

test("video meta must be 76 bytes", () => {
	expect(() => parseVideoMeta(new Uint8Array(75))).toThrow("76 bytes");
});

test("server arguments match the ones measured in spike S1", () => {
	expect(serverArgs("0badc0de")).toEqual([
		"scid=0badc0de",
		"log_level=info",
		"video=true",
		"audio=false",
		"control=true",
		"tunnel_forward=true",
		"max_size=0",
		"max_fps=30",
		"video_bit_rate=4000000",
		"video_codec=h264",
		"stay_awake=true",
		"clipboard_autosync=false",
		"send_device_meta=true",
		"send_dummy_byte=true",
	]);
});

test("the pkill pattern matches the server but not its own shell", () => {
	const pattern = new RegExp(scidPattern("0badc0de"));
	expect(
		pattern.test(
			"app_process / com.genymobile.scrcpy.Server 3.3.3 scid=0badc0de log_level=info",
		),
	).toBe(true);
	expect(pattern.test(`sh -c pkill -f '${scidPattern("0badc0de")}'`)).toBe(
		false,
	);
	expect(pattern.test("scid=0badc0df")).toBe(false);
});

describe("control allow-list", () => {
	// Messages as the browser builds them, with the same serializer.
	const serializer = new ScrcpyControlMessageSerializer(
		new ScrcpyOptions3_3_3({
			audio: false,
			tunnelForward: true,
			control: true,
			clipboardAutosync: false,
		}),
	);
	const touch = serializer.injectTouch({
		action: AndroidMotionEventAction.Down,
		pointerId: ScrcpyPointerId.Finger,
		pointerX: 100,
		pointerY: 200,
		videoWidth: 216,
		videoHeight: 480,
		pressure: 1,
		actionButton: 0,
		buttons: 0,
	});
	const scroll = serializer.injectScroll({
		pointerX: 100,
		pointerY: 200,
		videoWidth: 216,
		videoHeight: 480,
		scrollX: 0,
		scrollY: -1,
		buttons: 0,
	});
	const key = serializer.injectKeyCode({
		action: AndroidKeyEventAction.Down,
		keyCode: AndroidKeyCode.AndroidHome,
		repeat: 0,
		metaState: 0,
	});
	const back = serializer.backOrScreenOn(AndroidKeyEventAction.Down);

	test("allows key, text, touch, scroll, and back-or-screen-on", () => {
		expect(classifyControl(key)).toBe("input");
		expect(classifyControl(serializer.injectText("héllo"))).toBe("text");
		expect(classifyControl(serializer.injectText(""))).toBe("text");
		expect(classifyControl(touch)).toBe("input");
		expect(scroll && classifyControl(scroll)).toBe("input");
		expect(back && classifyControl(back)).toBe("input");
	});

	test("refuses every other type, display power included", () => {
		expect(
			classifyControl(serializer.setDisplayPower(AndroidScreenPowerMode.Off)),
		).toBeNull();
		expect(classifyControl(setDisplayPower(true))).toBeNull();
		expect(classifyControl(serializer.expandNotificationPanel())).toBeNull();
		expect(classifyControl(serializer.expandSettingPanel())).toBeNull();
		expect(classifyControl(serializer.collapseNotificationPanel())).toBeNull();
		expect(classifyControl(serializer.rotateDevice())).toBeNull();
		expect(
			classifyControl(serializer.startApp("com.android.settings")),
		).toBeNull();
		expect(classifyControl(serializer.uHidDestroy(1))).toBeNull();
		expect(classifyControl(serializer.resetVideo())).toBeNull();
		expect(classifyControl(RESET_VIDEO)).toBeNull();
		// SET_CLIPBOARD: type 9, u64 sequence, paste flag, u32 length, "x".
		expect(
			classifyControl(
				new Uint8Array([9, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0x78]),
			),
		).toBeNull();
		expect(classifyControl(new Uint8Array([8, 0]))).toBeNull(); // GET_CLIPBOARD
		expect(classifyControl(new Uint8Array())).toBeNull();
	});

	test("refuses allowed types with the wrong length", () => {
		expect(classifyControl(touch.subarray(0, 31))).toBeNull();
		expect(classifyControl(new Uint8Array([...touch, 0]))).toBeNull();
		expect(classifyControl(key.subarray(0, 13))).toBeNull();
		// Two messages in one frame would be parsed as two by the server.
		expect(classifyControl(new Uint8Array([...key, ...key]))).toBeNull();
		expect(classifyControl(new Uint8Array([10]))).toBeNull();
		const text = serializer.injectText("hello");
		expect(classifyControl(text.subarray(0, text.length - 1))).toBeNull();
		expect(classifyControl(new Uint8Array([...text, 0x41]))).toBeNull();
	});

	test("refuses text over 300 bytes", () => {
		expect(classifyControl(serializer.injectText("a".repeat(300)))).toBe(
			"text",
		);
		expect(classifyControl(serializer.injectText("a".repeat(301)))).toBeNull();
	});
});

test("a packet header claiming more than 8 MiB is a corrupt stream", async () => {
	const header = new Uint8Array(12);
	new DataView(header.buffer).setUint32(8, MAX_PACKET_BYTES + 1);
	const reader = new StreamReader(chunked(header, () => 12));
	await expect(reader.readPacket()).rejects.toThrow("exceeds");
});

test("the wakefulness read parses what the B30 printed, awake and dozing", async () => {
	// Recorded through adb exec: the line, then the error dumpsys prints when
	// grep -m1 closes the pipe.
	const read = (name: string) =>
		Bun.file(
			join(import.meta.dir, `../../test/fixtures/device/nuu-s6702x/${name}`),
		).text();
	expect(parseWakefulness(await read("wakefulness-awake.txt"))).toBe("Awake");
	expect(parseWakefulness(await read("wakefulness-dozing.txt"))).toBe("Dozing");
	expect(parseWakefulness("")).toBeNull();
});
