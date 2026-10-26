import { Elysia } from "elysia";
import { z } from "zod";
import { ApiError } from "../api/schema";
import {
	type Auth,
	AuthError,
	MIN_PASSWORD_LENGTH,
	type Principal,
	SESSION_COOKIE,
	SESSION_DAYS,
} from "./auth";
import { LoginLimiter } from "./limiter";

// Paths anyone may request. Everything else under /api/ needs a signed-in
// session; the web app's pages and assets are public, so the sign-in page
// can load.
const PUBLIC_API = new Set([
	"/api/docs",
	"/api/docs/json",
	"/api/v1/auth/login",
	"/api/v1/auth/me",
]);

const principals = new WeakMap<Request, Principal>();

// Who the request acts for; set by the guard before any route runs.
export function principalOf(request: Request): Principal | null {
	return principals.get(request) ?? null;
}

// The name edits and screen sessions are recorded under.
export function loginOf(request: Request): string | null {
	return principalOf(request)?.user.name ?? null;
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

// Runs on every request, before routing: resolves the principal and turns
// away protected requests without one, WebSocket upgrades included.
export function guard(auth: Auth) {
	return ({
		request,
		set,
	}: {
		request: Request;
		set: { status?: number | string };
	}) => {
		const path = new URL(request.url).pathname;
		if (!path.startsWith("/api/")) return;
		const token = cookie(request, SESSION_COOKIE);
		const principal = token ? auth.session(token) : null;
		if (principal) principals.set(request, principal);
		if (principal || PUBLIC_API.has(path)) return;
		set.status = 401;
		return { error: "sign in first" };
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
			async ({ request, body, set, server }) => {
				const address = server?.requestIP(request)?.address ?? "local";
				const keys = [
					`name:${body.name.trim().toLowerCase()}`,
					`ip:${address}`,
				];
				const wait = limiter.retryAfter(keys);
				if (wait !== null) {
					set.status = 429;
					set.headers["retry-after"] = String(wait);
					return { error: `too many failed sign-ins; try again in ${wait} s` };
				}
				const user = await auth.verify(body.name, body.password);
				if (!user) {
					limiter.fail(keys);
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
						"Sets an HttpOnly session cookie that lasts 30 days from its last use. After 10 failed attempts for one name or from one address in 15 minutes, further attempts get 429 until the oldest failure is 15 minutes old.",
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
				detail: { summary: "Who is signed in", tags: ["auth"] },
			},
		)
		.post(
			"/logout",
			({ request, set }) => {
				const principal = principalOf(request);
				if (principal) auth.endSession(principal.sessionId);
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
				if (!principal)
					throw new Error("the guard let an anonymous request in");
				if (!(await auth.verify(principal.user.name, body.current))) {
					set.status = 403;
					return { error: "the current password is wrong" };
				}
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
				},
				detail: {
					summary: "Change your password",
					description: `The new password needs at least ${MIN_PASSWORD_LENGTH} characters. Every other session of the user ends.`,
					tags: ["auth"],
				},
			},
		);
}
