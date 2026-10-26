import type { Database } from "bun:sqlite";
import { Auth, SESSION_COOKIE } from "../src/auth/auth";

// A signed-in user for API tests. Nothing here checks the password, so the
// row skips the deliberately slow argon2 hash.
export function testSession(db: Database, name = "owner@example.com") {
	const at = new Date().toISOString();
	db.run(
		"INSERT INTO users (name, password_hash, created_at, password_changed_at) VALUES (?, 'unused', ?, ?)",
		[name, at, at],
	);
	const auth = new Auth(db);
	const user = auth.findUser(name);
	if (!user) throw new Error("test user missing");
	return {
		auth,
		name,
		cookie: `${SESSION_COOKIE}=${auth.createSession(user.id)}`,
	};
}

// Sends the session cookie with every request the app handles, unless the
// request carries a cookie header of its own.
export function signedIn<
	T extends { handle: (request: Request) => Promise<Response> },
>(app: T, cookie: string): T {
	const handle = app.handle.bind(app);
	app.handle = (request: Request) => {
		if (request.headers.has("cookie")) return handle(request);
		const headers = new Headers(request.headers);
		headers.set("cookie", cookie);
		return handle(new Request(request, { headers }));
	};
	return app;
}
