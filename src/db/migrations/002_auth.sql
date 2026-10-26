-- People who can sign in, and their sessions (src/auth/).

CREATE TABLE users (
	id INTEGER PRIMARY KEY,
	name TEXT NOT NULL UNIQUE COLLATE NOCASE CHECK (length(name) BETWEEN 1 AND 64),
	-- argon2id, in the PHC string format Bun.password writes.
	password_hash TEXT NOT NULL,
	created_at TEXT NOT NULL,
	password_changed_at TEXT NOT NULL
);

-- A signed-in browser. Only the SHA-256 of the cookie's token is stored, so
-- a copy of the database cannot be used to sign in.
CREATE TABLE sessions (
	id INTEGER PRIMARY KEY,
	token_sha256 TEXT NOT NULL UNIQUE CHECK (length(token_sha256) = 64),
	user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
	created_at TEXT NOT NULL,
	last_seen_at TEXT NOT NULL,
	expires_at TEXT NOT NULL
);
CREATE INDEX sessions_user ON sessions (user_id);
