import { existsSync, statSync } from "node:fs";
import { join, normalize, sep } from "node:path";
import { openapi } from "@elysiajs/openapi";
import { Elysia } from "elysia";
import { z } from "zod";
import type { Auth } from "../auth/auth";
import { authRoutes, guard, loginOf, revocation } from "../auth/routes";
import type { Bus } from "../bus";
import { toE164 } from "../devices/identifiers";
import type { Devices } from "../devices/manager";
import {
	ConflictError,
	type Inventory,
	NotFoundError,
} from "../inventory/store";
import type { DeviceEventRow } from "../inventory/sync";
import type { Monitor } from "../monitor/monitor";
import type { ScreenManager } from "../screens/manager";
import { MAX_VIEWER_MESSAGE_BYTES } from "../screens/protocol";
import { screenRoutes } from "../screens/routes";
import { BadRequestError } from "./errors";
import { type MessageOptions, messageRoutes } from "./messages";
import { monitorRoutes } from "./monitor";
import {
	ApiError,
	Health,
	NumberPatch,
	Phone,
	PhoneNumber,
	PhonePatch,
	Sim,
	SimPatch,
	Status,
} from "./schema";
import {
	eventStream,
	parseCursor,
	parseKinds,
	streamDescription,
} from "./stream";
import { batteryView, numberView, phoneView } from "./views";

export type AppOptions = {
	// Users and sessions; every API route but sign-in and the docs needs one.
	auth: Auth;
	inventory: Inventory;
	devices: Devices;
	events: Bus<DeviceEventRow>;
	messages: MessageOptions;
	// Phone policy, drift, and alerts; null serves none of them.
	monitor?: Monitor | null;
	// Built SPA (bun run build); null serves no UI.
	webDir: string | null;
	version: string;
	// Interval of SSE keep-alive comments, so proxies keep idle streams open.
	heartbeatMs?: number;
	// Screen streaming; null serves no screen routes.
	screens?: ScreenManager | null;
};

export function createApp(options: AppOptions) {
	const { inventory, devices } = options;
	const monitor = options.monitor ?? null;

	const api = new Elysia({ prefix: "/api/v1" })
		.get(
			"/health",
			() => {
				const adb = devices.adb();
				const known = inventory.listPhones();
				const online = known.filter(
					(phone) => devices.get(phone.serial)?.status === "online",
				).length;
				return {
					ok: adb.reachable,
					adbReachable: adb.reachable,
					phonesOnline: online,
					phonesOffline: known.length - online,
					alerts: monitor?.alerts().length ?? 0,
					version: options.version,
				};
			},
			{
				response: { 200: Health },
				detail: { summary: "Liveness and phone summary", tags: ["health"] },
			},
		)
		.get(
			"/phones",
			() => ({
				phones: inventory
					.listPhones()
					.map((row) => phoneView(row, devices.get(row.serial))),
			}),
			{
				response: { 200: z.object({ phones: z.array(Phone) }) },
				detail: { summary: "List phones with live state", tags: ["phones"] },
			},
		)
		.get(
			"/phones/:serial",
			({ params }) => {
				const row = inventory.getPhone(params.serial);
				if (!row) throw new NotFoundError(`no phone ${params.serial}`);
				return phoneView(row, devices.get(row.serial));
			},
			{
				response: { 200: Phone, 404: ApiError },
				detail: { summary: "One phone", tags: ["phones"] },
			},
		)
		.patch(
			"/phones/:serial",
			({ params, body, request }) => {
				const row = inventory.patchPhone(params.serial, body, loginOf(request));
				return phoneView(row, devices.get(row.serial));
			},
			{
				body: PhonePatch,
				response: { 200: Phone, 404: ApiError },
				detail: {
					summary: "Edit a phone's location, hub port, cost, and purchase date",
					tags: ["phones"],
				},
			},
		)
		.get("/sims", () => ({ sims: inventory.listSims() }), {
			response: { 200: z.object({ sims: z.array(Sim) }) },
			detail: { summary: "List SIMs", tags: ["sims"] },
		})
		.get(
			"/sims/:key",
			({ params }) => {
				const sim = inventory.findSim(params.key);
				if (!sim) throw new NotFoundError(`no SIM ${params.key}`);
				return sim;
			},
			{
				response: { 200: Sim, 404: ApiError },
				detail: { summary: "One SIM, by id or ICCID", tags: ["sims"] },
			},
		)
		.patch(
			"/sims/:key",
			({ params, body, request }) => {
				const sim = inventory.findSim(params.key);
				if (!sim) throw new NotFoundError(`no SIM ${params.key}`);
				return inventory.patchSim(sim.id, body, loginOf(request));
			},
			{
				body: SimPatch,
				response: { 200: Sim, 404: ApiError },
				detail: { summary: "Edit a SIM's status", tags: ["sims"] },
			},
		)
		.get(
			"/numbers",
			() => ({
				numbers: inventory.listNumbers().map((row) => numberView(row, devices)),
			}),
			{
				response: { 200: z.object({ numbers: z.array(PhoneNumber) }) },
				detail: { summary: "List numbers", tags: ["numbers"] },
			},
		)
		.get(
			"/numbers/:key",
			({ params }) => {
				const row = inventory.findNumber(params.key);
				if (!row) throw new NotFoundError(`no number ${params.key}`);
				return numberView(row, devices);
			},
			{
				response: { 200: PhoneNumber, 404: ApiError },
				detail: {
					summary: "One number, by id or E.164 (+15125550101)",
					tags: ["numbers"],
				},
			},
		)
		.patch(
			"/numbers/:key",
			({ params, body, request }) => {
				const row = inventory.findNumber(params.key);
				if (!row) throw new NotFoundError(`no number ${params.key}`);
				let e164: string | null | undefined = body.e164;
				if (typeof body.e164 === "string") {
					// Numbers typed without +CC are read as US numbers.
					e164 = toE164(body.e164, "us");
					if (e164 === null)
						throw new BadRequestError(
							`"${body.e164}" is not a phone number in E.164 or 10-digit US form`,
						);
				}
				return numberView(
					inventory.patchNumber(row.id, { ...body, e164 }, loginOf(request)),
					devices,
				);
			},
			{
				body: NumberPatch,
				response: {
					200: PhoneNumber,
					400: ApiError,
					404: ApiError,
					409: ApiError,
				},
				detail: {
					summary: "Edit a number's note, or set the number by hand",
					tags: ["numbers"],
				},
			},
		)
		.get(
			"/status",
			() => ({
				adb: devices.adb(),
				phones: devices.list().map((phone) => ({
					serial: phone.serial,
					status: phone.status,
					adbState: phone.adbState,
					since: phone.since,
					usb: phone.usb,
					battery: batteryView(phone.health?.battery ?? null),
					radios: phone.health?.radios ?? [],
					simStates: phone.health?.simStates ?? [],
					healthAt: phone.healthAt,
					identityAt: phone.identityAt,
					probeError: phone.probeError,
					policy: monitor?.phone(phone.serial) ?? null,
				})),
				alerts: monitor?.alerts() ?? [],
				dataThresholdBytes: monitor?.dataThresholdBytes ?? null,
				profile: monitor?.policy
					? {
							name: monitor.policy.name,
							setUp: monitor.policy.rules
								.filter((rule) => rule.enforce)
								.map((rule) => rule.describe),
							removesApps: monitor.policy.rules.some(
								(rule) => rule.id === "debloat",
							),
						}
					: null,
			}),
			{
				response: { 200: Status },
				detail: {
					summary: "Live adb and phone state, policy results, and alerts",
					tags: ["status"],
				},
			},
		)
		.use(monitorRoutes(monitor))
		.get(
			"/events",
			({ query, headers, request, server }) => {
				// The heartbeat keeps the stream busy; without this Bun would
				// still close it after its idle timeout.
				server?.timeout(request, 0);
				const kinds = parseKinds(query.types);
				if (kinds === null)
					throw new BadRequestError(
						`types must be device, message, or both, not "${query.types}"`,
					);
				return eventStream(
					{
						inventory,
						events: options.events,
						store: options.messages.store,
						messages: options.messages.bus,
						heartbeatMs: options.heartbeatMs ?? 15_000,
					},
					kinds,
					parseCursor(headers["last-event-id"] ?? query.after, kinds),
					query.latest ?? 0,
					// Ends when the client goes away or its session ends.
					revocation(request).signal,
				);
			},
			{
				query: z.object({
					after: z
						.string()
						.optional()
						.describe("A cursor in the event id format; Last-Event-ID wins"),
					latest: z.coerce
						.number()
						.int()
						.min(0)
						.max(500)
						.optional()
						.describe("Without a cursor: replay the newest N device events"),
					// Elysia turns a comma-separated value into an array.
					types: z
						.union([z.string(), z.array(z.string())])
						.optional()
						.describe("Comma-separated: device, message. Default device."),
				}),
				detail: {
					summary: "Device events and texts as Server-Sent Events",
					description: streamDescription,
					tags: ["events", "messages"],
				},
			},
		);

	if (options.screens) api.use(screenRoutes(options.screens));

	// Bun applies WebSocket limits server-wide, so they are set on the root.
	const app = new Elysia({
		websocket: { maxPayloadLength: MAX_VIEWER_MESSAGE_BYTES },
	})
		.onRequest(guard(options.auth))
		.use(
			openapi({
				path: "/api/docs",
				specPath: "/api/docs/json",
				documentation: {
					info: {
						title: "spareline",
						version: options.version,
						description:
							"Phones on USB, their numbers, and every text they receive. Every route except sign-in needs a session: sign in with POST /api/v1/auth/login, which sets the session cookie.",
					},
					components: {
						securitySchemes: {
							session: {
								type: "apiKey",
								in: "cookie",
								name: "spareline_session",
							},
						},
					},
					security: [{ session: [] }],
				},
				mapJsonSchema: { zod: z.toJSONSchema },
				exclude: { paths: ["/", "/assets/*", "/*"] },
			}),
		)
		.onError(({ error, code, set }) => {
			if (error instanceof NotFoundError) {
				set.status = 404;
				return { error: error.message };
			}
			if (error instanceof ConflictError) {
				set.status = 409;
				return { error: error.message };
			}
			if (error instanceof BadRequestError) {
				set.status = 400;
				return { error: error.message };
			}
			if (code === "VALIDATION") {
				set.status = 422;
				return { error: error.message };
			}
			if (code === "NOT_FOUND") {
				set.status = 404;
				return { error: "not found" };
			}
		})
		// Public, for container and proxy health checks: says only whether the
		// adb server is reachable.
		.get("/healthz", () => ({ ok: devices.adb().reachable }), {
			response: { 200: z.object({ ok: z.boolean() }) },
			detail: {
				summary: "Liveness (no sign-in needed)",
				description: "ok is true when the adb server is reachable.",
				security: [],
				tags: ["health"],
			},
		})
		.use(authRoutes(options.auth))
		.use(api)
		.use(messageRoutes(inventory, options.messages));

	return options.webDir ? app.use(webRoutes(options.webDir)) : app;
}

// Serves the built SPA: hashed assets with long caching, and index.html for
// every other path that is not an API route, so client-side routes reload.
function webRoutes(webDir: string) {
	const root = normalize(webDir);
	const index = join(root, "index.html");
	return new Elysia()
		.get("/assets/*", ({ params, set }) => {
			const path = normalize(join(root, "assets", params["*"]));
			if (
				!path.startsWith(root + sep) ||
				!statSync(path, { throwIfNoEntry: false })?.isFile()
			) {
				set.status = 404;
				return "not found";
			}
			set.headers["cache-control"] = "public, max-age=31536000, immutable";
			return Bun.file(path);
		})
		.get("/*", ({ params, set }) => {
			if (params["*"].startsWith("api/")) {
				set.status = 404;
				return { error: "not found" };
			}
			if (!existsSync(index)) {
				set.status = 503;
				return "The web UI is not built. Run `bun run build`.";
			}
			set.headers["cache-control"] = "no-cache";
			return Bun.file(index);
		});
}

export type App = ReturnType<typeof createApp>;
