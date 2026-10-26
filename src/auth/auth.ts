import type { Database } from "bun:sqlite";

export type User = { id: number; name: string; createdAt: string };

export type KeyScope = "read" | "full";

export type ApiKey = {
	id: number;
	user: string;
	name: string;
	scope: KeyScope;
	// The key's first characters, to tell keys apart.
	prefix: string;
	createdAt: string;
	lastUsedAt: string | null;
	revokedAt: string | null;
};

// Who a request acts for.
export type Principal =
	| { kind: "session"; user: User; sessionId: number }
	| {
			kind: "key";
			user: User;
			keyId: number;
			keyName: string;
			scope: KeyScope;
	  };
export type SessionPrincipal = Extract<Principal, { kind: "session" }>;

export const SESSION_COOKIE = "spareline_session";
export const KEY_PREFIX = "sl_";
// last_used_at of a key is written at most this often.
const KEY_TOUCH_MS = 60_000;
// A session lasts this long after it was last used.
export const SESSION_DAYS = 30;
// last_seen_at is written at most this often per session.
const TOUCH_MS = 60 * 60_000;
export const MIN_PASSWORD_LENGTH = 12;

export class AuthError extends Error {}

type UserRow = { id: number; name: string; created_at: string };

const toUser = (row: UserRow): User => ({
	id: row.id,
	name: row.name,
	createdAt: row.created_at,
});

// A token with 256 random bits, as base64url without padding.
export function randomToken(): string {
	return Buffer.from(crypto.getRandomValues(new Uint8Array(32))).toString(
		"base64url",
	);
}

export function sha256(text: string): string {
	return new Bun.CryptoHasher("sha256").update(text).digest("hex");
}

// 20 characters from a 56-character alphabet without look-alikes: about 116
// bits.
const ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
export function generatePassword(length = 20): string {
	const out: string[] = [];
	// Rejection sampling keeps every character equally likely.
	const limit = 256 - (256 % ALPHABET.length);
	while (out.length < length)
		for (const byte of crypto.getRandomValues(new Uint8Array(length * 2)))
			if (byte < limit && out.length < length)
				out.push(ALPHABET[byte % ALPHABET.length] as string);
	return out.join("");
}

// Users, passwords, and sessions in the app's database.
export class Auth {
	// Verified against when a name is unknown, so a wrong name takes as long
	// as a wrong password.
	private dummyHash: Promise<string> | null = null;
	private readonly listeners = new Set<() => void>();

	constructor(
		private readonly db: Database,
		private readonly now: () => Date = () => new Date(),
	) {}

	// Calls `listener` after anything in this process that can end a session
	// or a key.
	// Open streams and sockets re-check their principal then (and on a timer,
	// for changes another process such as the CLI made).
	onChange(listener: () => void): () => void {
		this.listeners.add(listener);
		return () => {
			this.listeners.delete(listener);
		};
	}

	private changed(): void {
		for (const listener of [...this.listeners]) listener();
	}

	// Whether the principal's session still exists and has not expired, or
	// its key is not revoked.
	valid(principal: Principal): boolean {
		if (principal.kind === "key")
			return (
				this.db
					.query("SELECT 1 FROM api_keys WHERE id = ? AND revoked_at IS NULL")
					.get(principal.keyId) !== null
			);
		return (
			this.db
				.query("SELECT 1 FROM sessions WHERE id = ? AND expires_at > ?")
				.get(principal.sessionId, this.now().toISOString()) !== null
		);
	}

	listUsers(): User[] {
		return this.db
			.query<UserRow, []>(
				"SELECT id, name, created_at FROM users ORDER BY name COLLATE NOCASE",
			)
			.all()
			.map(toUser);
	}

	hasUsers(): boolean {
		return this.db.query("SELECT 1 FROM users LIMIT 1").get() !== null;
	}

	findUser(name: string): User | null {
		const row = this.db
			.query<UserRow, [string]>(
				"SELECT id, name, created_at FROM users WHERE name = ?",
			)
			.get(name.trim());
		return row ? toUser(row) : null;
	}

	// Adds a user with a generated password, which is returned once.
	async addUser(name: string): Promise<{ user: User; password: string }> {
		const clean = name.trim();
		if (!/^[\w.@-]{1,64}$/.test(clean))
			throw new AuthError(
				"a user name is 1-64 letters, digits, and . _ @ - characters",
			);
		if (this.findUser(clean))
			throw new AuthError(`user ${clean} already exists`);
		const password = generatePassword();
		const at = this.now().toISOString();
		const row = this.db
			.query<UserRow, [string, string, string, string]>(
				"INSERT INTO users (name, password_hash, created_at, password_changed_at) VALUES (?, ?, ?, ?) RETURNING id, name, created_at",
			)
			.get(clean, await hashPassword(password), at, at);
		if (!row) throw new Error("user insert returned no row");
		return { user: toUser(row), password };
	}

	removeUser(name: string): void {
		const user = this.findUser(name);
		if (!user) throw new AuthError(`no user ${name}`);
		this.db.run("DELETE FROM users WHERE id = ?", [user.id]);
		this.changed();
	}

	// Sets a new password and signs the user out everywhere except `keep`.
	async setPassword(
		userId: number,
		password: string,
		keep: number | null = null,
	): Promise<void> {
		if (password.length < MIN_PASSWORD_LENGTH)
			throw new AuthError(
				`a password needs at least ${MIN_PASSWORD_LENGTH} characters`,
			);
		const hash = await hashPassword(password);
		this.db.transaction(() => {
			this.db.run(
				"UPDATE users SET password_hash = ?, password_changed_at = ? WHERE id = ?",
				[hash, this.now().toISOString(), userId],
			);
			this.db.run("DELETE FROM sessions WHERE user_id = ? AND id IS NOT ?", [
				userId,
				keep,
			]);
		})();
		this.changed();
	}

	// Replaces a forgotten password with a generated one, returned once, and
	// ends every session of the user.
	async resetPassword(name: string): Promise<string> {
		const user = this.findUser(name);
		if (!user) throw new AuthError(`no user ${name}`);
		const password = generatePassword();
		await this.setPassword(user.id, password);
		return password;
	}

	// The user when the password matches; null otherwise.
	async verify(name: string, password: string): Promise<User | null> {
		const row = this.db
			.query<UserRow & { password_hash: string }, [string]>(
				"SELECT id, name, created_at, password_hash FROM users WHERE name = ?",
			)
			.get(name.trim());
		if (!row) {
			this.dummyHash ??= hashPassword(generatePassword());
			await Bun.password.verify(password, await this.dummyHash);
			return null;
		}
		return (await Bun.password.verify(password, row.password_hash))
			? toUser(row)
			: null;
	}

	// Starts a session and returns its token, which only the cookie holds.
	createSession(userId: number): string {
		const token = randomToken();
		const now = this.now();
		this.db.run(
			"INSERT INTO sessions (token_sha256, user_id, created_at, last_seen_at, expires_at) VALUES (?, ?, ?, ?, ?)",
			[
				sha256(token),
				userId,
				now.toISOString(),
				now.toISOString(),
				expiry(now).toISOString(),
			],
		);
		return token;
	}

	// The session's user while it is unexpired. Using it extends it, at most
	// hourly; `renewed` is called then, so the cookie can be renewed too.
	session(token: string, renewed?: () => void): SessionPrincipal | null {
		const row = this.db
			.query<
				UserRow & {
					session_id: number;
					last_seen_at: string;
					expires_at: string;
				},
				[string]
			>(
				`SELECT u.id, u.name, u.created_at, s.id AS session_id, s.last_seen_at, s.expires_at
				FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_sha256 = ?`,
			)
			.get(sha256(token));
		if (!row) return null;
		const now = this.now();
		if (Date.parse(row.expires_at) <= now.getTime()) {
			this.db.run("DELETE FROM sessions WHERE id = ?", [row.session_id]);
			return null;
		}
		if (now.getTime() - Date.parse(row.last_seen_at) >= TOUCH_MS) {
			this.db.run(
				"UPDATE sessions SET last_seen_at = ?, expires_at = ? WHERE id = ?",
				[now.toISOString(), expiry(now).toISOString(), row.session_id],
			);
			renewed?.();
		}
		return { kind: "session", user: toUser(row), sessionId: row.session_id };
	}

	endSession(sessionId: number): void {
		this.db.run("DELETE FROM sessions WHERE id = ?", [sessionId]);
		this.changed();
	}

	// Drops expired sessions; the server runs it now and then.
	pruneSessions(): void {
		this.db.run("DELETE FROM sessions WHERE expires_at <= ?", [
			this.now().toISOString(),
		]);
		this.changed();
	}

	// Creates a key for the user and returns it once; only its hash is kept.
	createKey(
		userId: number,
		name: string,
		scope: KeyScope,
	): { key: string; apiKey: ApiKey } {
		const clean = name.trim();
		if (clean.length < 1 || clean.length > 64)
			throw new AuthError("a key name is 1-64 characters");
		const key = `${KEY_PREFIX}${randomToken()}`;
		const row = this.db
			.query<{ id: number }, [number, string, string, string, string, string]>(
				"INSERT INTO api_keys (user_id, name, scope, prefix, key_sha256, created_at) VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
			)
			.get(
				userId,
				clean,
				scope,
				key.slice(0, KEY_PREFIX.length + 6),
				sha256(key),
				this.now().toISOString(),
			);
		const apiKey = this.listKeys().find(
			(candidate) => candidate.id === row?.id,
		);
		if (!apiKey) throw new Error("key insert returned no row");
		return { key, apiKey };
	}

	// Every key, revoked ones included, newest first.
	listKeys(): ApiKey[] {
		return this.db
			.query<
				{
					id: number;
					user: string;
					name: string;
					scope: KeyScope;
					prefix: string;
					created_at: string;
					last_used_at: string | null;
					revoked_at: string | null;
				},
				[]
			>(
				`SELECT k.id, u.name AS user, k.name, k.scope, k.prefix, k.created_at, k.last_used_at, k.revoked_at
				FROM api_keys k JOIN users u ON u.id = k.user_id ORDER BY k.id DESC`,
			)
			.all()
			.map((row) => ({
				id: row.id,
				user: row.user,
				name: row.name,
				scope: row.scope,
				prefix: row.prefix,
				createdAt: row.created_at,
				lastUsedAt: row.last_used_at,
				revokedAt: row.revoked_at,
			}));
	}

	revokeKey(id: number): ApiKey {
		this.db.run(
			"UPDATE api_keys SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
			[this.now().toISOString(), id],
		);
		this.changed();
		const key = this.listKeys().find((candidate) => candidate.id === id);
		if (!key) throw new AuthError(`no API key ${id}`);
		return key;
	}

	// The key's owner while the key is not revoked; using it is recorded.
	keyPrincipal(key: string): Principal | null {
		if (!key.startsWith(KEY_PREFIX)) return null;
		const row = this.db
			.query<
				UserRow & {
					key_id: number;
					key_name: string;
					scope: KeyScope;
					last_used_at: string | null;
				},
				[string]
			>(
				`SELECT u.id, u.name, u.created_at, k.id AS key_id, k.name AS key_name, k.scope, k.last_used_at
				FROM api_keys k JOIN users u ON u.id = k.user_id
				WHERE k.key_sha256 = ? AND k.revoked_at IS NULL`,
			)
			.get(sha256(key));
		if (!row) return null;
		const now = this.now();
		if (
			row.last_used_at === null ||
			now.getTime() - Date.parse(row.last_used_at) >= KEY_TOUCH_MS
		)
			this.db.run("UPDATE api_keys SET last_used_at = ? WHERE id = ?", [
				now.toISOString(),
				row.key_id,
			]);
		return {
			kind: "key",
			user: toUser(row),
			keyId: row.key_id,
			keyName: row.key_name,
			scope: row.scope,
		};
	}
}

function expiry(now: Date): Date {
	return new Date(now.getTime() + SESSION_DAYS * 86_400_000);
}

function hashPassword(password: string): Promise<string> {
	return Bun.password.hash(password, { algorithm: "argon2id" });
}
