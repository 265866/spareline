import { VIDEO_BIT_RATE, type VideoPacket } from "./protocol";

// What the relay needs from a viewer's WebSocket. Bun's ServerWebSocket.send
// returns -1 when the message was queued under backpressure, 0 when it was
// dropped, and the byte count otherwise.
export type ViewerSink = {
	send(data: Uint8Array | string): number;
	bufferedAmount(): number;
	close(code: number, reason: string): void;
};

// Bytes a viewer may have queued before it skips to the next keyframe:
// about one second of video.
export const BACKLOG_LIMIT = VIDEO_BIT_RATE / 8;

// How long a keyframe request may go unanswered before it is repeated.
const KEYFRAME_RETRY_MS = 2_000;

export type Delivery = {
	// Dropping everything but config packets: the viewer's decoder is new (a
	// new viewer, or a new session) and cannot decode even a keyframe yet.
	needsConfig: boolean;
	// Dropping until a keyframe, because frames after a gap cannot decode.
	waitingForKeyframe: boolean;
	// When this viewer last asked the server for a keyframe (RESET_VIDEO).
	keyframeRequestedAt: number | null;
};

type Decision = "send" | "drop" | "drop-and-request-keyframe";

// The backpressure policy. A slow viewer never grows an unbounded queue: once
// its socket holds more than `limit` bytes it drops frames, and once the
// queue has drained it asks for a keyframe and resumes from there. Config
// packets are tiny and always sent, since a decoder cannot start without one;
// a viewer that has not had one gets nothing else, not even a keyframe the
// encoder sends on its own.
export function decide(
	state: Delivery,
	packet: Pick<VideoPacket, "config" | "keyframe">,
	buffered: number,
	limit: number,
	now: number,
): Decision {
	if (packet.config) {
		// A keyframe follows every config packet.
		state.needsConfig = false;
		state.waitingForKeyframe = true;
		state.keyframeRequestedAt = now;
		return "send";
	}
	if (buffered > limit) {
		state.waitingForKeyframe = true;
		state.keyframeRequestedAt = null;
		return "drop";
	}
	if (state.needsConfig || (state.waitingForKeyframe && !packet.keyframe)) {
		const requested = state.keyframeRequestedAt;
		if (requested !== null && now - requested < KEYFRAME_RETRY_MS)
			return "drop";
		state.keyframeRequestedAt = now;
		return "drop-and-request-keyframe";
	}
	state.waitingForKeyframe = false;
	return "send";
}

type ViewerCounts = {
	dropped: number;
	inputs: number;
	texts: number;
	rejected: number;
};

// One WebSocket watching one phone.
export class Viewer {
	readonly delivery: Delivery;
	readonly counts: ViewerCounts = {
		dropped: 0,
		inputs: 0,
		texts: 0,
		rejected: 0,
	};
	// This viewer's control_sessions row.
	auditId: number | null = null;
	// The session whose hello this viewer got; it receives only that
	// session's packets.
	greetedScid: string | null = null;

	constructor(
		readonly sink: ViewerSink,
		readonly login: string | null,
		now: number,
	) {
		// A new viewer starts mid-stream; the relay asks for a config packet
		// and keyframe when it joins.
		this.delivery = {
			needsConfig: true,
			waitingForKeyframe: true,
			keyframeRequestedAt: now,
		};
	}

	// Returns true when the viewer needs a keyframe from the server.
	deliver(packet: VideoPacket, limit: number, now: number): boolean {
		const decision = decide(
			this.delivery,
			packet,
			this.sink.bufferedAmount(),
			limit,
			now,
		);
		if (decision !== "send") {
			this.counts.dropped++;
			return decision === "drop-and-request-keyframe";
		}
		if (this.sink.send(packet.bytes) === 0) {
			this.counts.dropped++;
			if (packet.config) this.delivery.needsConfig = true;
			this.delivery.waitingForKeyframe = true;
			this.delivery.keyframeRequestedAt = null;
		}
		return false;
	}

	sendJson(message: object): void {
		this.sink.send(JSON.stringify(message));
	}
}
