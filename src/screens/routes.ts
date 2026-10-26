import type { ServerWebSocket } from "bun";
import { Elysia } from "elysia";
import { z } from "zod";
import { loginOf } from "../auth/routes";
import { AttachError, type ScreenManager, type ViewerHandle } from "./manager";
import type { ViewerSink } from "./viewer";

export const ScreenSession = z.object({
	serial: z.string(),
	state: z.enum(["starting", "running", "idle"]),
	width: z.number().int().nullable(),
	height: z.number().int().nullable(),
	viewers: z.number().int(),
	fps: z.number(),
	kbps: z.number(),
	startedAt: z.string().nullable(),
});

// Any page may open a WebSocket to any host, so without this check a page
// from another site, open in a signed-in user's browser, could watch and
// control a phone. Browsers always send Origin on a WebSocket, and reverse
// proxies such as Tailscale Serve pass it and the original Host through
// unchanged. Clients without an Origin (scripts) are allowed.
export function sameOrigin(
	origin: string | null,
	host: string | null,
): boolean {
	if (origin === null) return true;
	try {
		return host !== null && new URL(origin).host === host.toLowerCase();
	} catch {
		return false;
	}
}

export function screenRoutes(screens: ScreenManager) {
	const handles = new WeakMap<object, ViewerHandle>();
	return new Elysia()
		.get("/screens", () => ({ sessions: screens.list() }), {
			response: { 200: z.object({ sessions: z.array(ScreenSession) }) },
			detail: {
				summary: "Active screen sessions",
				description:
					"One entry per phone with a scrcpy session or viewers. A session stops 30 s after its last viewer leaves.",
				tags: ["screens"],
			},
		})
		.ws("/phones/:serial/screen", {
			beforeHandle({ request, status }) {
				if (
					!sameOrigin(
						request.headers.get("origin"),
						request.headers.get("host"),
					)
				)
					return status(403, "cross-origin WebSocket refused");
			},
			detail: {
				summary: "Live screen of one phone (WebSocket)",
				description:
					"Text frames are JSON: hello {width, height} at every session start, then status and stats. Binary frames are scrcpy 3.3.3 video packets, unchanged. The client may send scrcpy control messages of types 0-4 (key, text, touch, scroll, back-or-screen-on), at most 4 KiB each; anything else is dropped and counted. A browser's Origin must match the Host. Close codes: 4001 phone offline, 4002 session failed, 1001 server shutting down.",
				tags: ["screens"],
			},
			open(ws) {
				const raw = ws.raw;
				// Elysia's type for the socket omits getBufferedAmount; at runtime
				// it is Bun's ServerWebSocket.
				const bun = raw as unknown as ServerWebSocket<unknown>;
				const sink: ViewerSink = {
					send: (data) => raw.send(data),
					bufferedAmount: () => bun.getBufferedAmount(),
					close: (code, reason) => raw.close(code, reason),
				};
				try {
					handles.set(
						raw,
						screens.attach(
							ws.data.params.serial,
							sink,
							loginOf(ws.data.request),
						),
					);
				} catch (error) {
					if (!(error instanceof AttachError)) throw error;
					raw.close(error.code, error.message.slice(0, 120));
				}
			},
			message(ws, message) {
				if (message instanceof Uint8Array)
					handles.get(ws.raw)?.control(message);
			},
			close(ws) {
				handles.get(ws.raw)?.detach();
				handles.delete(ws.raw);
			},
		});
}
