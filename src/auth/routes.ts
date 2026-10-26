import { Elysia } from "elysia";
import { z } from "zod";
import { ApiError } from "../api/schema";
import {
	type Auth,
	AuthError,
	type KeyScope,
	MIN_PASSWORD_LENGTH,
	type Principal,
	SESSION_COOKIE,
	SESSION_DAYS,
} from "./auth";
import { LoginLimiter } from "./limiter";

// Paths anyone may request. Everything else under /api/ needs a signed-in
// session or an API key; the web app's pages and assets are public, so the
// sign-in page can load.
const PUBLIC_API = new Set([
	"/api/docs",
	"/api/docs/json",
	"/api/v1/auth/login",
	"/api/v1/auth/me",
]);

const principals = new WeakMap<Request, Principal>();
const auths = new WeakMap<Request, Auth>();
// How often an open stream or socket re-checks that its session is still
// valid, for changes another process (the CLI) made or for expiry.
const RECHECK_MS = 5_000;

// Who the request acts for; set by the guard before any route runs.
export function principalOf(request: Request): Principal | null {
	return principals.get(request) ?? null;
}

// The name edits and screen sessions are recorded under.
export function loginOf(request: Request): string | null {
	const principal = principalOf(request);
	if (!principal) return null;
	return principal.kind === "key"
		? `${principal.user.name} (key ${principal.keyName})`
		: principal.user.name;
}

// Routes only a signed-in browser may use: a leaked key must not be able to
// mint more keys or change a password.
const SESSION_ONLY = ["/api/v1/auth/", "/api/v1/api-keys"];

// What a read-only key may do: requests that change nothing. A screen's
// WebSocket can send input, so it is not one of them.
function readOnly(request: Request): boolean {
	return (
		(request.method === "GET" || request.method === "HEAD") &&
		request.headers.get("upgrade")?.toLowerCase() !== "websocket"
	);
}

// Aborts when the request's session ends (signed out, password changed,
// user removed, or expired) or, with `followRequest`, when the request does.
// A stream or socket that outlives its request uses it to stop serving
// someone who no longer may.
export function revocation(
	request: Request,
	followRequest = true,
): { signal: AbortSignal; release: () => void } {
	const controller = new AbortController();
	const signal = followRequest
		? AbortSignal.any([request.signal, controller.signal])
		: controller.signal;
	const principal = principalOf(request);
	const auth = auths.get(request);
	if (!principal || !auth) return { signal, release: () => {} };
	const check = () => {
		if (!auth.valid(principal)) controller.abort();
	};
	const off = auth.onChange(check);
	const timer = setInterval(check, RECHECK_MS);
	const release = () => {
		off();
		clearInterval(timer);
	};
	signal.addEventListener("abort", release, { once: true });
	return { signal, release };
}

function cookie(request: Request, name: string): string | null {
	for (const part of (request.headers.get("cookie") ?? "").split(";")) {
		const eq = part.indexOf("=");
		if (eq > 0 && part.slice(0, eq).trim() === name)
			return part.slice(eq + 1).trim();
	}
	return null;
}

// Secure cookies need HTTPS; behind a proxy that terminates it, the proxy
// says so in X-Forwarded-Proto.
function secure(request: Request): boolean {
	return (
		new URL(request.url).protocol === "https:" ||
		request.headers.get("x-forwarded-proto") === "https"
	);
}

function sessionCookie(request: Request, token: string, maxAge: number) {
	return [
		`${SESSION_COOKIE}=${token}`,
		"Path=/",
		"HttpOnly",
		"SameSite=Strict",
		`Max-Age=${maxAge}`,
		...(secure(request) ? ["Secure"] : []),
	].join("; ");
}

// Runs on every request, before routing: resolves the principal (an
// `Authorization: Bearer` API key, else the session cookie) and turns away
// protected requests without one, WebSocket upgrades included, and what a
// key's scope does not allow.
export function guard(auth: Auth) {
	return ({
		request,
		set,
	}: {
		request: Request;
		set: { status?: number | string; headers: Record<string, unknown> };
	}) => {
		const path = new URL(request.url).pathname;
		if (!path.startsWith("/api/")) return;
		const bearer = /^Bearer\s+(\S+)$/i.exec(
			request.headers.get("authorization") ?? "",
		)?.[1];
		const token = cookie(request, SESSION_COOKIE);
		// A used session is extended on the server; the cookie gets the same
		// new lifetime, so an active browser is never signed out.
		const principal =
			bearer !== undefined
				? auth.keyPrincipal(bearer)
				: token
					? auth.session(token, () => {
							set.headers["set-cookie"] = sessionCookie(
								request,
								token,
								SESSION_DAYS * 86_400,
							);
						})
					: null;
		if (!principal) {
			if (PUBLIC_API.has(path)) return;
			set.status = 401;
			return {
				error:
					bearer === undefined ? "sign in first" : "unknown or revoked API key",
			};
		}
		principals.set(request, principal);
		auths.set(request, auth);
		if (principal.kind !== "key") return;
		if (SESSION_ONLY.some((prefix) => path.startsWith(prefix))) {
			set.status = 403;
			return {
				error:
					"API keys cannot manage keys or passwords; sign in with a browser",
			};
		}
		if (principal.scope === "read" && !readOnly(request)) {
			set.status = 403;
			return { error: "this API key can only read" };
		}
	};
}

const Credentials = z.object({
	name: z.string().min(1).max(64),
	password: z.string().min(1).max(1024),
});

const Me = z.object({ user: z.object({ name: z.string() }) });

// Only JSON: a page on another site can post a form, but not JSON without a
// CORS preflight this server never grants.
function jsonOnly({
	request,
	set,
}: {
	request: Request;
	set: { status?: number | string };
}) {
	if (
		!(request.headers.get("content-type") ?? "").startsWith("application/json")
	) {
		set.status = 415;
		return { error: "send Content-Type: application/json" };
	}
}

export function authRoutes(auth: Auth, limiter = new LoginLimiter()) {
	return new Elysia({ prefix: "/api/v1/auth" })
		.post(
			"/login",
			async ({ request, body, set }) => {
				const keys = [`name:${body.name.trim().toLowerCase()}`];
				const wait = limiter.retryAfter(keys);
				if (wait !== null) {
					set.status = 429;
					set.headers["retry-after"] = String(wait);
					return { error: `too many failed sign-ins; try again in ${wait} s` };
				}
				limiter.fail(keys);
				const user = await auth.verify(body.name, body.password);
				if (!user) {
					set.status = 401;
					return { error: "wrong name or password" };
				}
				limiter.succeed(keys);
				set.headers["set-cookie"] = sessionCookie(
					request,
					auth.createSession(user.id),
					SESSION_DAYS * 86_400,
				);
				return { user: { name: user.name } };
			},
			{
				beforeHandle: jsonOnly,
				body: Credentials,
				response: { 200: Me, 401: ApiError, 415: ApiError, 429: ApiError },
				detail: {
					summary: "Sign in",
					description:
						"Sets an HttpOnly session cookie that lasts 30 days from its last use. After 10 failed attempts for one name in 15 minutes, further attempts get 429 until the oldest failure is 15 minutes old.",
					security: [],
					tags: ["auth"],
				},
			},
		)
		.get(
			"/me",
			({ request, set }) => {
				const principal = principalOf(request);
				if (!principal) {
					set.status = 401;
					return {
						error: auth.hasUsers()
							? "sign in first"
							: "no users yet: create one on the server with `bun src/cli.ts users add NAME`",
						noUsers: !auth.hasUsers(),
					};
				}
				return { user: { name: principal.user.name } };
			},
			{
				response: {
					200: Me,
					401: ApiError.extend({ noUsers: z.boolean() }),
				},
				detail: { summary: "Who is signed in", security: [], tags: ["auth"] },
			},
		)
		.post(
			"/logout",
			({ request, set }) => {
				const principal = principalOf(request);
				if (principal?.kind === "session") auth.endSession(principal.sessionId);
				set.headers["set-cookie"] = sessionCookie(request, "", 0);
				return { ok: true };
			},
			{
				beforeHandle: jsonOnly,
				response: { 200: z.object({ ok: z.boolean() }), 415: ApiError },
				detail: { summary: "Sign out this browser", tags: ["auth"] },
			},
		)
		.post(
			"/password",
			async ({ request, body, set }) => {
				const principal = principalOf(request);
				if (principal?.kind !== "session")
					throw new Error("the guard let a non-session request in");
				const keys = [`name:${principal.user.name.toLowerCase()}`];
				const wait = limiter.retryAfter(keys);
				if (wait !== null) {
					set.status = 429;
					set.headers["retry-after"] = String(wait);
					return { error: `too many failed attempts; try again in ${wait} s` };
				}
				limiter.fail(keys);
				if (!(await auth.verify(principal.user.name, body.current))) {
					set.status = 403;
					return { error: "the current password is wrong" };
				}
				limiter.succeed(keys);
				try {
					await auth.setPassword(
						principal.user.id,
						body.next,
						principal.sessionId,
					);
				} catch (error) {
					if (!(error instanceof AuthError)) throw error;
					set.status = 400;
					return { error: error.message };
				}
				return { ok: true };
			},
			{
				beforeHandle: jsonOnly,
				body: z.object({
					current: z.string().min(1).max(1024),
					next: z.string().max(1024),
				}),
				response: {
					200: z.object({ ok: z.boolean() }),
					400: ApiError,
					403: ApiError,
					415: ApiError,
					429: ApiError,
				},
				detail: {
					summary: "Change your password",
					description: `The new password needs at least ${MIN_PASSWORD_LENGTH} characters. Every other session of the user ends.`,
					tags: ["auth"],
				},
			},
		);
}

const ApiKeyView = z.object({
	id: z.number().int(),
	user: z.string(),
	name: z.string(),
	scope: z.enum(["read", "full"]),
	prefix: z.string().describe("The key's first characters"),
	createdAt: z.string(),
	lastUsedAt: z.string().nullable(),
	revokedAt: z.string().nullable(),
});

// Listing, creating, and revoking API keys; signed-in browsers only.
export function keyRoutes(auth: Auth) {
	const owner = (request: Request) => {
		const principal = principalOf(request);
		if (principal?.kind !== "session")
			throw new Error("the guard let a non-session request in");
		return principal.user;
	};
	return new Elysia({ prefix: "/api/v1/api-keys" })
		.get("/", () => ({ keys: auth.listKeys() }), {
			response: { 200: z.object({ keys: z.array(ApiKeyView) }) },
			detail: {
				summary: "List API keys",
				description:
					"Every user's keys, revoked ones included. Never the keys themselves.",
				tags: ["auth"],
			},
		})
		.post(
			"/",
			({ request, body, set }) => {
				try {
					const { key, apiKey } = auth.createKey(
						owner(request).id,
						body.name,
						body.scope as KeyScope,
					);
					return { key, apiKey };
				} catch (error) {
					if (!(error instanceof AuthError)) throw error;
					set.status = 400;
					return { error: error.message };
				}
			},
			{
				beforeHandle: jsonOnly,
				body: z.object({
					name: z.string().max(64),
					scope: z.enum(["read", "full"]),
				}),
				response: {
					200: z.object({
						key: z.string().describe("Shown once: store it now"),
						apiKey: ApiKeyView,
					}),
					400: ApiError,
					415: ApiError,
				},
				detail: {
					summary: "Create an API key",
					description:
						"Send the key as `Authorization: Bearer <key>`. A read key may only make GET requests (texts, events, inventory) and cannot open a screen; a full key may do everything a signed-in user can, except manage keys and passwords.",
					tags: ["auth"],
				},
			},
		)
		.delete(
			"/:id",
			({ params, set }) => {
				try {
					return auth.revokeKey(params.id);
				} catch (error) {
					if (!(error instanceof AuthError)) throw error;
					set.status = 404;
					return { error: error.message };
				}
			},
			{
				params: z.object({ id: z.coerce.number().int() }),
				response: { 200: ApiKeyView, 404: ApiError },
				detail: {
					summary: "Revoke an API key",
					description:
						"Requests with it are refused from now on, and its open event streams and screens close.",
					tags: ["auth"],
				},
			},
		);
}
