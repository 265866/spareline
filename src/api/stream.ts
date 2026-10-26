import type { Bus } from "../bus";
import type { Inventory } from "../inventory/store";
import type { DeviceEventRow } from "../inventory/sync";
import type { Message, MessageStore } from "../sms/store";

export type StreamKind = "device" | "message";

export type StreamSources = {
	inventory: Inventory;
	events: Bus<DeviceEventRow>;
	store: MessageStore;
	messages: Bus<Message>;
	heartbeatMs: number;
};

// Where a client is in each stream; undefined for a stream it has no cursor
// for yet.
export type StreamCursor = { device?: number; message?: number };

const REPLAY_PAGE = 500;

export const streamDescription = `Server-Sent Events for device events (\`event: device\`) and ingested texts (\`event: message\`). \`types=device,message\` picks the streams (default \`device\`).

Each event's \`id\` is the cursor of the streams you asked for: \`<deviceEventId>\` with \`types=device\`, \`<seq>\` with \`types=message\`, and \`<deviceEventId>:<seq>\` with both. With that value as \`after\` (or Last-Event-ID, which EventSource sends when it reconnects), the stream replays everything after it, oldest first, then streams new events. A bare number with both types is a device-event id; texts then start with the next new one. Without a cursor, \`latest=N\` replays the newest N device events, and otherwise only new events are sent. A \`: keep-alive\` comment is sent every 15 s.`;

// null for an unknown type.
export function parseKinds(
	types: string | string[] | undefined,
): StreamKind[] | null {
	if (!types || types.length === 0) return ["device"];
	const kinds = new Set(
		[types]
			.flat()
			.flatMap((type) => type.split(","))
			.map((type) => type.trim()),
	);
	const known = (["device", "message"] as const).filter((kind) =>
		kinds.has(kind),
	);
	return known.length === kinds.size ? known : null;
}

// Reads a cursor in the format the stream writes for the same `kinds`.
export function parseCursor(
	raw: string | undefined,
	kinds: StreamKind[],
): StreamCursor {
	const value = raw?.trim() ?? "";
	const both = kinds.length === 2;
	const pair = /^(\d+):(\d+)$/.exec(value);
	if (both && pair)
		return { device: Number(pair[1]), message: Number(pair[2]) };
	const single = /^\d+$/.test(value) ? Number(value) : undefined;
	if (single === undefined) return {};
	if (both) return { device: single };
	return kinds[0] === "message" ? { message: single } : { device: single };
}

export function eventStream(
	sources: StreamSources,
	kinds: StreamKind[],
	start: StreamCursor,
	latest: number,
	signal: AbortSignal,
): Response {
	const encoder = new TextEncoder();
	const wantDevice = kinds.includes("device");
	const wantMessage = kinds.includes("message");
	let cleanup = () => {};
	const stream = new ReadableStream<Uint8Array>({
		start(controller) {
			const { inventory, store } = sources;
			let device = start.device ?? inventory.latestEvents(1)[0]?.id ?? 0;
			let message = start.message ?? store.maxSeq();
			const id = () =>
				[wantDevice ? device : null, wantMessage ? message : null]
					.filter((part) => part !== null)
					.join(":");
			const send = (event: StreamKind, data: unknown) =>
				controller.enqueue(
					encoder.encode(
						`id: ${id()}\nevent: ${event}\ndata: ${JSON.stringify(data)}\n\n`,
					),
				);
			// Backlog and subscription happen in one synchronous run, and both
			// buses publish after their commits, so nothing falls in between;
			// the id checks drop anything delivered twice.
			const unsubscribes: (() => void)[] = [];
			if (wantDevice) {
				if (start.device !== undefined) {
					for (;;) {
						const page = inventory.eventsAfter(device, REPLAY_PAGE);
						for (const event of page) {
							device = event.id;
							send("device", event);
						}
						if (page.length < REPLAY_PAGE) break;
					}
				} else if (latest > 0) {
					// The newest N, oldest first, ending at the newest id.
					for (const event of inventory.latestEvents(latest)) {
						device = event.id;
						send("device", event);
					}
				}
				unsubscribes.push(
					sources.events.subscribe((event) => {
						if (event.id <= device) return;
						device = event.id;
						send("device", event);
					}),
				);
			}
			if (wantMessage) {
				while (start.message !== undefined) {
					const rows = store.list({
						after: message,
						limit: REPLAY_PAGE,
						order: "asc",
					});
					for (const row of rows) {
						message = row.seq;
						send("message", row);
					}
					if (rows.length < REPLAY_PAGE) break;
				}
				unsubscribes.push(
					sources.messages.subscribe((row) => {
						if (row.seq <= message) return;
						message = row.seq;
						send("message", row);
					}),
				);
			}
			// The current cursor, even when nothing was replayed: a client that
			// reconnects before any event arrives resumes from here instead of
			// from scratch.
			controller.enqueue(encoder.encode(`retry: 3000\nid: ${id()}\n\n`));
			const heartbeat = setInterval(
				() => controller.enqueue(encoder.encode(": keep-alive\n\n")),
				sources.heartbeatMs,
			);
			cleanup = () => {
				clearInterval(heartbeat);
				for (const unsubscribe of unsubscribes) unsubscribe();
			};
			signal.addEventListener("abort", () => {
				cleanup();
				try {
					controller.close();
				} catch {
					// Already closed by the client.
				}
			});
		},
		cancel: () => cleanup(),
	});
	return new Response(stream, {
		headers: {
			"content-type": "text/event-stream",
			"cache-control": "no-cache",
			"x-accel-buffering": "no",
		},
	});
}
