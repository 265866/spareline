// Records a real scrcpy video stream for the tests in src/screens/.
//
//   ADB_SERVER=127.0.0.1:5037 bun scripts/record-screen-fixture.ts SERIAL test/fixtures/screen/NAME
//
// Wakes the phone, dismisses the keyguard, and goes to the home screen, so
// no notification or app content is recorded. Then it runs one
// session through ScrcpySession, asks for a second keyframe halfway (as the
// relay does for a late joiner), and saves every byte of the video socket
// (NAME.bin) with a summary (NAME.json). Check the home screen holds nothing
// personal before committing.
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { AdbStream } from "../src/adb/client";
import { AdbClient, parseEndpoint } from "../src/adb/client";
import { concat } from "../src/adb/socket";
import { log } from "../src/log";
import { loadServerJar } from "../src/screens/jar";
import { SCRCPY_VERSION, serverArgs } from "../src/screens/protocol";
import { ScrcpySession, type ScreenDevices } from "../src/screens/session";

const [serial, out] = Bun.argv.slice(2);
if (!serial || !out) {
	console.error(
		"usage: bun scripts/record-screen-fixture.ts SERIAL OUT_PATH_WITHOUT_EXTENSION",
	);
	process.exit(2);
}
const adb = new AdbClient(
	parseEndpoint(process.env.ADB_SERVER ?? "127.0.0.1:5037"),
);
await adb.shell(
	serial,
	"input keyevent KEYCODE_WAKEUP; wm dismiss-keyguard; input keyevent KEYCODE_HOME",
);
await Bun.sleep(1_500);

// Every stream the session opens, with the bytes it delivered. The video
// socket is the one that starts with the dummy byte and carries the most.
const streams: Uint8Array[][] = [];
const devices: ScreenDevices = {
	exec: (s, command, options) => adb.exec(s, command, options),
	push: (s, data, remote, options) =>
		adb.push(
			s,
			typeof data === "string" ? new Uint8Array() : data,
			remote,
			options,
		),
	async openLocal(s, name, options): Promise<AdbStream> {
		const stream = await adb.openLocal(s, name, options);
		const chunks: Uint8Array[] = [];
		streams.push(chunks);
		return {
			...stream,
			readable: stream.readable.pipeThrough(
				new TransformStream<Uint8Array, Uint8Array>({
					transform(chunk, controller) {
						chunks.push(chunk.slice());
						controller.enqueue(chunk);
					},
				}),
			),
		};
	},
};

let packets = 0;
let configs = 0;
let keyframes = 0;
const session = new ScrcpySession(
	devices,
	serial,
	await loadServerJar(),
	{
		packet(packet) {
			packets++;
			if (packet.config) configs++;
			if (packet.keyframe) keyframes++;
		},
		ended(error) {
			if (error) console.error("session ended:", error.message);
		},
	},
	log,
);
const meta = await session.start();
await Bun.sleep(2_000);
session.requestKeyframe();
await Bun.sleep(2_000);
await session.stop();

const video = streams
	.map((chunks) => concat(chunks))
	.filter((bytes) => bytes[0] === 0)
	.sort((a, b) => b.length - a.length)[0];
if (!video) throw new Error("no video stream was recorded");
mkdirSync(dirname(out), { recursive: true });
writeFileSync(`${out}.bin`, video);
writeFileSync(
	`${out}.json`,
	`${JSON.stringify(
		{
			description:
				"Video socket of scrcpy-server on the home screen, RESET_VIDEO after 2 s",
			recordedAt: new Date().toISOString(),
			server: SCRCPY_VERSION,
			args: serverArgs("<scid>"),
			bytes: video.length,
			meta,
			packets,
			configs,
			keyframes,
		},
		null,
		"\t",
	)}\n`,
);
console.log({ bytes: video.length, meta, packets, configs, keyframes });
