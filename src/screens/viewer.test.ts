import { describe, expect, test } from "bun:test";
import { BACKLOG_LIMIT, type Delivery, decide, Viewer } from "./viewer";

const LIMIT = 100_000;
const frame = { config: false, keyframe: false };
const key = { config: false, keyframe: true };
const config = { config: true, keyframe: false };
const fresh = (): Delivery => ({
	needsConfig: false,
	waitingForKeyframe: false,
	keyframeRequestedAt: null,
});

describe("backpressure policy", () => {
	test("a viewer with room gets every frame", () => {
		const state = fresh();
		expect(decide(state, frame, 0, LIMIT, 0)).toBe("send");
		expect(decide(state, frame, LIMIT, LIMIT, 1)).toBe("send");
		expect(state.waitingForKeyframe).toBe(false);
	});

	test("a full queue drops frames, then resumes at a keyframe it asks for once", () => {
		const state = fresh();
		expect(decide(state, frame, LIMIT + 1, LIMIT, 0)).toBe("drop");
		// Still full: keyframes are dropped too, and nothing is requested yet.
		expect(decide(state, key, LIMIT + 1, LIMIT, 10)).toBe("drop");
		expect(decide(state, frame, LIMIT + 1, LIMIT, 20)).toBe("drop");
		// Drained: frames that depend on dropped ones are still dropped, and
		// the first of them asks the server for a keyframe.
		expect(decide(state, frame, 0, LIMIT, 30)).toBe(
			"drop-and-request-keyframe",
		);
		expect(decide(state, frame, 0, LIMIT, 40)).toBe("drop");
		expect(decide(state, key, 0, LIMIT, 50)).toBe("send");
		expect(decide(state, frame, 0, LIMIT, 60)).toBe("send");
	});

	test("an unanswered keyframe request is repeated after 2 s", () => {
		const state: Delivery = {
			needsConfig: false,
			waitingForKeyframe: true,
			keyframeRequestedAt: 0,
		};
		expect(decide(state, frame, 0, LIMIT, 1_999)).toBe("drop");
		expect(decide(state, frame, 0, LIMIT, 2_000)).toBe(
			"drop-and-request-keyframe",
		);
		expect(decide(state, frame, 0, LIMIT, 2_500)).toBe("drop");
	});

	test("config packets always go out, even to a full queue, and expect a keyframe", () => {
		const state = fresh();
		expect(decide(state, config, LIMIT * 10, LIMIT, 0)).toBe("send");
		expect(state.waitingForKeyframe).toBe(true);
		// The keyframe that follows a config was not requested by this viewer,
		// so no extra request is made for the frames before it.
		expect(decide(state, frame, 0, LIMIT, 1)).toBe("drop");
		expect(decide(state, key, 0, LIMIT, 2)).toBe("send");
	});

	test("a viewer without a config packet gets nothing else, not even a keyframe", () => {
		const state: Delivery = {
			needsConfig: true,
			waitingForKeyframe: true,
			keyframeRequestedAt: 0,
		};
		// A keyframe the encoder sends on its own, before the requested config.
		expect(decide(state, key, 0, LIMIT, 10)).toBe("drop");
		expect(decide(state, frame, 0, LIMIT, 20)).toBe("drop");
		// Still no config after 2 s: ask again.
		expect(decide(state, key, 0, LIMIT, 2_000)).toBe(
			"drop-and-request-keyframe",
		);
		expect(decide(state, config, 0, LIMIT, 2_100)).toBe("send");
		expect(decide(state, key, 0, LIMIT, 2_110)).toBe("send");
		expect(decide(state, frame, 0, LIMIT, 2_120)).toBe("send");
	});

	test("the limit is about a second of video", () => {
		expect(BACKLOG_LIMIT).toBe(500_000);
	});
});

describe("Viewer", () => {
	function sink(buffered = 0, result = 1) {
		const sent: (Uint8Array | string)[] = [];
		return {
			sent,
			buffered,
			send(data: Uint8Array | string) {
				sent.push(data);
				return result;
			},
			bufferedAmount() {
				return this.buffered;
			},
			close() {},
		};
	}
	const packet = (keyframe: boolean) => ({
		bytes: new Uint8Array([keyframe ? 1 : 2]),
		config: false,
		keyframe,
	});
	const configPacket = {
		bytes: new Uint8Array([0]),
		config: true,
		keyframe: false,
	};

	test("a new viewer starts at a config packet, then a keyframe, without asking again", () => {
		const s = sink();
		const viewer = new Viewer(s, null, 0);
		expect(viewer.deliver(packet(true), LIMIT, 5)).toBe(false);
		expect(viewer.deliver(packet(false), LIMIT, 10)).toBe(false);
		expect(viewer.deliver(configPacket, LIMIT, 15)).toBe(false);
		expect(viewer.deliver(packet(false), LIMIT, 18)).toBe(false);
		expect(viewer.deliver(packet(true), LIMIT, 20)).toBe(false);
		expect(viewer.deliver(packet(false), LIMIT, 30)).toBe(false);
		expect(s.sent.map((d) => (d as Uint8Array)[0])).toEqual([0, 1, 2]);
		expect(viewer.counts.dropped).toBe(3);
	});

	test("a config packet the socket dropped is needed again", () => {
		const viewer = new Viewer(sink(0, 0), null, 0);
		viewer.deliver(configPacket, LIMIT, 0);
		expect(viewer.delivery.needsConfig).toBe(true);
	});

	test("a slow viewer stops receiving and asks for a keyframe once drained", () => {
		const s = sink();
		const viewer = new Viewer(s, null, 0);
		viewer.deliver(configPacket, LIMIT, 0);
		viewer.deliver(packet(true), LIMIT, 0);
		s.buffered = LIMIT + 1;
		for (let i = 0; i < 10; i++) viewer.deliver(packet(false), LIMIT, i);
		expect(s.sent).toHaveLength(2);
		s.buffered = 0;
		expect(viewer.deliver(packet(false), LIMIT, 20)).toBe(true);
		expect(viewer.deliver(packet(true), LIMIT, 30)).toBe(false);
		expect(s.sent).toHaveLength(3);
		expect(viewer.counts.dropped).toBe(11);
	});

	test("a frame the socket dropped makes the viewer wait for a keyframe", () => {
		const s = sink(0, 0);
		const viewer = new Viewer(s, null, 0);
		viewer.delivery.needsConfig = false;
		viewer.deliver(packet(true), LIMIT, 0);
		expect(viewer.delivery.waitingForKeyframe).toBe(true);
		expect(viewer.counts.dropped).toBe(1);
	});
});
