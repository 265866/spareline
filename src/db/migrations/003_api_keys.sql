-- API keys for scripts (src/auth/). Only the SHA-256 of a key is stored;
-- prefix (its first characters) is shown so people can tell keys apart.
CREATE TABLE api_keys (
	id INTEGER PRIMARY KEY,
	user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
	name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 64),
	-- read: GET requests only. full: everything a signed-in user can do
	-- except managing keys and passwords.
	scope TEXT NOT NULL CHECK (scope IN ('read', 'full')),
	prefix TEXT NOT NULL,
	key_sha256 TEXT NOT NULL UNIQUE CHECK (length(key_sha256) = 64),
	created_at TEXT NOT NULL,
	last_used_at TEXT,
	revoked_at TEXT
);
CREATE INDEX api_keys_user ON api_keys (user_id);
