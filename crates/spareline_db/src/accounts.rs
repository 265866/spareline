//! Users, their passwords, browser sessions and API keys, as spareline-old's `Auth` kept them.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, Row};
use spareline_core::Timestamp;

use crate::account_error::AccountError;
use crate::api_key::{ApiKeyView, KeyScope, NewApiKey};
use crate::database_error::DatabaseError;
use crate::password_hash::PasswordHash;
use crate::passwords::Passwords;
use crate::principal::{KeyPrincipal, Principal, SessionLookup, SessionPrincipal};
use crate::user::{LoginCandidate, NewUser, User};
use crate::{js_text, token};

/// The first characters of every API key.
pub const KEY_PREFIX: &str = "sl_";

/// How long a session lasts after it was last used: 30 days. The session cookie's `Max-Age` is
/// this many seconds, 2592000.
pub const SESSION_LIFETIME: Duration = Duration::from_hours(30 * 24);

/// How often a session's `last_seen_at` and expiry are written at most.
const SESSION_TOUCH: Duration = Duration::from_hours(1);

/// How often a key's `last_used_at` is written at most.
const KEY_TOUCH: Duration = Duration::from_secs(60);

const SELECT_KEYS: &str = "SELECT k.id, u.name AS user, k.name, k.scope, k.prefix, k.created_at, k.last_used_at, k.revoked_at
    FROM api_keys k JOIN users u ON u.id = k.user_id";

/// Users, sessions and API keys in the database.
///
/// The methods that can end a session or a key, [`remove_user`](Self::remove_user),
/// [`set_password`](Self::set_password), [`reset_password`](Self::reset_password),
/// [`end_session`](Self::end_session), [`prune_sessions`](Self::prune_sessions) and
/// [`revoke_key`](Self::revoke_key), are where spareline-old told open streams to re-check
/// their principal with [`valid`](Self::valid).
#[derive(Debug)]
pub struct Accounts<'db> {
    connection: &'db mut Connection,
}

impl<'db> Accounts<'db> {
    pub(crate) fn new(connection: &'db mut Connection) -> Self {
        Self { connection }
    }
}

impl Accounts<'_> {
    /// Whether the principal's session still exists and hasn't expired at `now`, or its key
    /// isn't revoked. Open streams check this every few seconds.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn valid(&self, principal: &Principal, now: Timestamp) -> Result<bool, DatabaseError> {
        let found = match principal {
            Principal::Key(key) => self
                .connection
                .query_row(
                    "SELECT 1 FROM api_keys WHERE id = ? AND revoked_at IS NULL",
                    [key.key_id],
                    |_| Ok(()),
                )
                .optional()?,
            Principal::Session(session) => self
                .connection
                .query_row(
                    "SELECT 1 FROM sessions WHERE id = ? AND expires_at > ?",
                    (session.session_id, now.to_string()),
                    |_| Ok(()),
                )
                .optional()?,
        };
        Ok(found.is_some())
    }

    /// Every user, ordered by name without ASCII case.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn list_users(&self) -> Result<Vec<User>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name, created_at FROM users ORDER BY name COLLATE NOCASE")?;
        let users = statement
            .query_map([], user_from)?
            .collect::<Result<Vec<User>, _>>()?;
        Ok(users)
    }

    /// Whether any user exists.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn has_users(&self) -> Result<bool, DatabaseError> {
        let found = self
            .connection
            .query_row("SELECT 1 FROM users LIMIT 1", [], |_| Ok(()))
            .optional()?;
        Ok(found.is_some())
    }

    /// The user named `name` after trimming it, compared without ASCII case.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn find_user(&self, name: &str) -> Result<Option<User>, DatabaseError> {
        let user = self
            .connection
            .query_row(
                "SELECT id, name, created_at FROM users WHERE name = ?",
                [js_text::trim(name)],
                user_from,
            )
            .optional()?;
        Ok(user)
    }

    /// Adds a user with a generated password, which is returned once. For the command line;
    /// it hashes while it holds the database.
    ///
    /// # Errors
    ///
    /// Fails with [`AccountError::InvalidUserName`] unless the trimmed name is 1-64 ASCII
    /// letters, digits, `.`, `_`, `@` and `-`, with [`AccountError::UserExists`] when the name
    /// is taken in any letter case, and when SQLite or the random source fails.
    pub fn add_user(
        &mut self,
        name: &str,
        passwords: &Passwords,
        now: Timestamp,
    ) -> Result<NewUser, AccountError> {
        let clean = js_text::trim(name);
        if !is_valid_user_name(clean) {
            return Err(AccountError::InvalidUserName);
        }
        if self.find_user(clean)?.is_some() {
            return Err(AccountError::UserExists {
                name: clean.to_owned(),
            });
        }
        let password = Passwords::generate_password()?;
        let hash = passwords.hash(&password)?;
        let at = now.to_string();
        let user = self.connection.query_row(
            "INSERT INTO users (name, password_hash, created_at, password_changed_at) VALUES (?, ?, ?, ?) RETURNING id, name, created_at",
            (clean, hash.as_str(), &at, &at),
            user_from,
        )?;
        Ok(NewUser { user, password })
    }

    /// Removes the user named `name`, and with them their sessions and keys.
    ///
    /// # Errors
    ///
    /// Fails with [`AccountError::NoUser`], naming `name` as given, when no user has it, and
    /// when SQLite fails.
    pub fn remove_user(&mut self, name: &str) -> Result<(), AccountError> {
        let user = self.find_user(name)?.ok_or_else(|| AccountError::NoUser {
            name: name.to_owned(),
        })?;
        self.connection
            .execute("DELETE FROM users WHERE id = ?", [user.id])?;
        Ok(())
    }

    /// Stores `hash` as the user's password, from
    /// [`Passwords::hash_new`](crate::Passwords::hash_new), and ends every session of the user
    /// except `keep`.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does; then nothing changes.
    pub fn set_password(
        &mut self,
        user_id: i64,
        hash: &PasswordHash,
        keep: Option<i64>,
        now: Timestamp,
    ) -> Result<(), DatabaseError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE users SET password_hash = ?, password_changed_at = ? WHERE id = ?",
            (hash.as_str(), now.to_string(), user_id),
        )?;
        transaction.execute(
            "DELETE FROM sessions WHERE user_id = ? AND id IS NOT ?",
            (user_id, keep),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Replaces a forgotten password with a generated one, returned once, and ends every
    /// session of the user. For the command line; it hashes while it holds the database.
    ///
    /// # Errors
    ///
    /// Fails with [`AccountError::NoUser`], naming `name` as given, when no user has it, and
    /// when SQLite or the random source fails.
    pub fn reset_password(
        &mut self,
        name: &str,
        passwords: &Passwords,
        now: Timestamp,
    ) -> Result<String, AccountError> {
        let user = self.find_user(name)?.ok_or_else(|| AccountError::NoUser {
            name: name.to_owned(),
        })?;
        let password = Passwords::generate_password()?;
        let hash = passwords.hash(&password)?;
        self.set_password(user.id, &hash, None, now)?;
        Ok(password)
    }

    /// The user named `name` after trimming, with their stored hash, for a sign-in. Check the
    /// password with [`Passwords::verify_login`](crate::Passwords::verify_login), which also
    /// takes `None`, so an unknown name costs as much as a wrong password.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn login_candidate(&self, name: &str) -> Result<Option<LoginCandidate>, DatabaseError> {
        let candidate = self
            .connection
            .query_row(
                "SELECT id, name, created_at, password_hash FROM users WHERE name = ?",
                [js_text::trim(name)],
                |row| {
                    Ok(LoginCandidate {
                        user: user_from(row)?,
                        hash: PasswordHash::from_stored(row.get(3)?),
                    })
                },
            )
            .optional()?;
        Ok(candidate)
    }

    /// Starts a session for the user and returns its token, which only the cookie holds; the
    /// database keeps its SHA-256. It expires [`SESSION_LIFETIME`] after `now`.
    ///
    /// # Errors
    ///
    /// Fails when SQLite or the random source does.
    pub fn create_session(&mut self, user_id: i64, now: Timestamp) -> Result<String, AccountError> {
        let token = token::random_token()?;
        let at = now.to_string();
        self.connection.execute(
            "INSERT INTO sessions (token_sha256, user_id, created_at, last_seen_at, expires_at) VALUES (?, ?, ?, ?, ?)",
            (
                token::sha256_hex(token.as_bytes()),
                user_id,
                &at,
                &at,
                now.plus(SESSION_LIFETIME).to_string(),
            ),
        )?;
        Ok(token)
    }

    /// The session whose token is `token`, while it is unexpired at `now`. An expired one is
    /// deleted. Using a session at least an hour after it was last seen moves its
    /// `last_seen_at` to `now` and its expiry to [`SESSION_LIFETIME`] after `now`, and reports
    /// [`SessionLookup::renewed`].
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn session(
        &mut self,
        token: &str,
        now: Timestamp,
    ) -> Result<Option<SessionLookup>, DatabaseError> {
        let found = self
            .connection
            .query_row(
                "SELECT u.id, u.name, u.created_at, s.id AS session_id, s.last_seen_at, s.expires_at
                FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_sha256 = ?",
                [token::sha256_hex(token.as_bytes())],
                |row| {
                    Ok(StoredSession {
                        user: user_from(row)?,
                        session_id: row.get(3)?,
                        last_seen_at: row.get(4)?,
                        expires_at: row.get(5)?,
                    })
                },
            )
            .optional()?;
        let Some(stored) = found else {
            return Ok(None);
        };
        if Timestamp::parse_iso(&stored.expires_at).is_some_and(|expires| expires <= now) {
            self.connection
                .execute("DELETE FROM sessions WHERE id = ?", [stored.session_id])?;
            return Ok(None);
        }
        let renewed = Timestamp::parse_iso(&stored.last_seen_at)
            .is_some_and(|seen| now.since(seen) >= SESSION_TOUCH);
        if renewed {
            self.connection.execute(
                "UPDATE sessions SET last_seen_at = ?, expires_at = ? WHERE id = ?",
                (
                    now.to_string(),
                    now.plus(SESSION_LIFETIME).to_string(),
                    stored.session_id,
                ),
            )?;
        }
        Ok(Some(SessionLookup {
            principal: SessionPrincipal {
                user: stored.user,
                session_id: stored.session_id,
            },
            renewed,
        }))
    }

    /// Ends the session, as signing out does.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn end_session(&mut self, session_id: i64) -> Result<(), DatabaseError> {
        self.connection
            .execute("DELETE FROM sessions WHERE id = ?", [session_id])?;
        Ok(())
    }

    /// Deletes the sessions that expired by `now`, and returns how many there were.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn prune_sessions(&mut self, now: Timestamp) -> Result<usize, DatabaseError> {
        let deleted = self.connection.execute(
            "DELETE FROM sessions WHERE expires_at <= ?",
            [now.to_string()],
        )?;
        Ok(deleted)
    }

    /// Makes a key for the user, `sl_` and 32 random bytes in base64url, and returns it once;
    /// the database keeps its first 9 characters and its SHA-256.
    ///
    /// # Errors
    ///
    /// Fails with [`AccountError::InvalidKeyName`] unless the trimmed name is 1-64 characters,
    /// counted in UTF-16 code units, and when SQLite or the random source fails.
    pub fn create_key(
        &mut self,
        user_id: i64,
        name: &str,
        scope: KeyScope,
        now: Timestamp,
    ) -> Result<NewApiKey, AccountError> {
        let clean = js_text::trim(name);
        if !(1..=64).contains(&js_text::utf16_len(clean)) {
            return Err(AccountError::InvalidKeyName);
        }
        let key = format!("{KEY_PREFIX}{}", token::random_token()?);
        let prefix = &key[..KEY_PREFIX.len() + 6];
        let id: i64 = self.connection.query_row(
            "INSERT INTO api_keys (user_id, name, scope, prefix, key_sha256, created_at) VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
            (user_id, clean, scope, prefix, token::sha256_hex(key.as_bytes()), now.to_string()),
            |row| row.get(0),
        )?;
        let api_key = self.key(id)?.ok_or_else(|| {
            AccountError::Database(DatabaseError::Sqlite(rusqlite::Error::QueryReturnedNoRows))
        })?;
        Ok(NewApiKey { key, api_key })
    }

    /// Every key, revoked ones included, newest first.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn list_keys(&self) -> Result<Vec<ApiKeyView>, DatabaseError> {
        let mut statement = self
            .connection
            .prepare(&format!("{SELECT_KEYS} ORDER BY k.id DESC"))?;
        let keys = statement
            .query_map([], key_from)?
            .collect::<Result<Vec<ApiKeyView>, _>>()?;
        Ok(keys)
    }

    /// Revokes the key at `now` unless it is revoked already, and returns it with the time it
    /// was first revoked.
    ///
    /// # Errors
    ///
    /// Fails with [`AccountError::NoApiKey`] when no key has the id, and when SQLite fails.
    pub fn revoke_key(&mut self, id: i64, now: Timestamp) -> Result<ApiKeyView, AccountError> {
        self.connection.execute(
            "UPDATE api_keys SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
            (now.to_string(), id),
        )?;
        self.key(id)?.ok_or(AccountError::NoApiKey { id })
    }

    /// The owner of `key` while the key isn't revoked. Using a key writes `last_used_at` when
    /// it is unset or at least a minute old.
    ///
    /// # Errors
    ///
    /// Fails when SQLite does.
    pub fn key_principal(
        &mut self,
        key: &str,
        now: Timestamp,
    ) -> Result<Option<KeyPrincipal>, DatabaseError> {
        if !key.starts_with(KEY_PREFIX) {
            return Ok(None);
        }
        let found = self
            .connection
            .query_row(
                "SELECT u.id, u.name, u.created_at, k.id AS key_id, k.name AS key_name, k.scope, k.last_used_at
                FROM api_keys k JOIN users u ON u.id = k.user_id
                WHERE k.key_sha256 = ? AND k.revoked_at IS NULL",
                [token::sha256_hex(key.as_bytes())],
                |row| {
                    Ok(StoredKey {
                        principal: KeyPrincipal {
                            user: user_from(row)?,
                            key_id: row.get(3)?,
                            key_name: row.get(4)?,
                            scope: row.get(5)?,
                        },
                        last_used_at: row.get(6)?,
                    })
                },
            )
            .optional()?;
        let Some(stored) = found else {
            return Ok(None);
        };
        let touch = match &stored.last_used_at {
            None => true,
            Some(used) => {
                Timestamp::parse_iso(used).is_some_and(|used| now.since(used) >= KEY_TOUCH)
            }
        };
        if touch {
            self.connection.execute(
                "UPDATE api_keys SET last_used_at = ? WHERE id = ?",
                (now.to_string(), stored.principal.key_id),
            )?;
        }
        Ok(Some(stored.principal))
    }

    /// The key with the id, as [`list_keys`](Self::list_keys) shows it.
    fn key(&self, id: i64) -> Result<Option<ApiKeyView>, DatabaseError> {
        let key = self
            .connection
            .query_row(&format!("{SELECT_KEYS} WHERE k.id = ?"), [id], key_from)
            .optional()?;
        Ok(key)
    }
}

/// A session row with its user.
struct StoredSession {
    user: User,
    session_id: i64,
    last_seen_at: String,
    expires_at: String,
}

/// A key row with its owner.
struct StoredKey {
    principal: KeyPrincipal,
    last_used_at: Option<String>,
}

/// Whether `name` matches spareline-old's `^[\w.@-]{1,64}$`, where `\w` is ASCII.
fn is_valid_user_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'@' | b'-'))
}

/// A user from the first three columns: id, name, `created_at`.
fn user_from(row: &Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
    })
}

fn key_from(row: &Row<'_>) -> rusqlite::Result<ApiKeyView> {
    Ok(ApiKeyView {
        id: row.get(0)?,
        user: row.get(1)?,
        name: row.get(2)?,
        scope: row.get(3)?,
        prefix: row.get(4)?,
        created_at: row.get(5)?,
        last_used_at: row.get(6)?,
        revoked_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, test_clock, test_passwords};

    fn days(count: u64) -> Duration {
        Duration::from_hours(24 * count)
    }

    /// A database with one user, alice.
    struct Alice {
        database: Database,
        user: User,
    }

    fn with_alice() -> Alice {
        let mut database = Database::open_in_memory(test_clock::fixed).unwrap();
        let user = database
            .accounts()
            .add_user("alice", &test_passwords::cheap(), test_clock::START)
            .unwrap()
            .user;
        Alice { database, user }
    }

    fn count(database: &Database, table: &str) -> i64 {
        database
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn verify(database: &mut Database, name: &str, password: &str) -> Option<User> {
        let candidate = database.accounts().login_candidate(name).unwrap();
        test_passwords::cheap()
            .verify_login(candidate, password)
            .unwrap()
    }

    #[test]
    fn a_new_user_gets_a_generated_password_that_signs_in() {
        let mut database = Database::open_in_memory(test_clock::fixed).unwrap();
        let passwords = test_passwords::cheap();
        assert!(!database.accounts().has_users().unwrap());
        let NewUser { user, password } = database
            .accounts()
            .add_user(" alice ", &passwords, test_clock::START)
            .unwrap();
        assert_eq!(user.name, "alice");
        assert_eq!(user.created_at, "2026-10-26T00:00:00.000Z");
        assert_eq!(password.len(), 20);
        assert!(database.accounts().has_users().unwrap());
        assert_eq!(
            verify(&mut database, "alice", &password),
            Some(user.clone())
        );
        assert_eq!(
            verify(&mut database, "ALICE", &password).map(|found| found.id),
            Some(user.id),
            "names are compared without ASCII case"
        );
        assert_eq!(
            verify(&mut database, "alice", &format!("{password}x")),
            None
        );
        assert_eq!(verify(&mut database, "bob", &password), None);
        let stored: String = database
            .connection
            .query_row("SELECT password_hash FROM users", [], |row| row.get(0))
            .unwrap();
        assert!(stored.starts_with("$argon2id$v=19$"), "{stored}");
        assert!(!stored.contains(&password));
        let changed: String = database
            .connection
            .query_row("SELECT password_changed_at FROM users", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(changed, "2026-10-26T00:00:00.000Z");
    }

    #[test]
    fn user_names_are_checked_and_unique() {
        let mut database = Database::open_in_memory(test_clock::fixed).unwrap();
        let passwords = test_passwords::cheap();
        let mut add = |name: &str| {
            database
                .accounts()
                .add_user(name, &passwords, test_clock::START)
                .map(|added| added.user.name)
        };
        assert_eq!(add("a.b_c@d-e").unwrap(), "a.b_c@d-e");
        assert_eq!(add(&"x".repeat(64)).unwrap(), "x".repeat(64));
        assert_eq!(add("alice").unwrap(), "alice");
        assert_eq!(
            add("Alice").unwrap_err().to_string(),
            "user Alice already exists"
        );
        for bad in ["", "   ", "a b", "\u{e9}t\u{e9}", &"x".repeat(65)] {
            let error = add(bad).unwrap_err();
            assert_eq!(
                error.to_string(),
                "a user name is 1-64 letters, digits, and . _ @ - characters",
                "{bad:?}"
            );
            assert!(error.is_user_error());
        }
        let missing = database.accounts().remove_user(" nobody").unwrap_err();
        assert_eq!(missing.to_string(), "no user  nobody", "the name as given");
        let listed: Vec<String> = database
            .accounts()
            .list_users()
            .unwrap()
            .into_iter()
            .map(|user| user.name)
            .collect();
        assert_eq!(listed, ["a.b_c@d-e", "alice", "x".repeat(64).as_str()]);
    }

    #[test]
    fn a_session_holds_only_the_tokens_hash_slides_with_use_and_expires() {
        let mut database = Database::open_in_memory(test_clock::fixed).unwrap();
        let user = database
            .accounts()
            .add_user("alice", &test_passwords::cheap(), test_clock::START)
            .unwrap()
            .user;
        let token = database
            .accounts()
            .create_session(user.id, test_clock::START)
            .unwrap();
        assert_eq!(token.len(), 43);
        let row: (String, String, String) = database
            .connection
            .query_row(
                "SELECT token_sha256, last_seen_at, expires_at FROM sessions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row.0, token::sha256_hex(token.as_bytes()));
        assert_eq!(row.1, "2026-10-26T00:00:00.000Z");
        assert_eq!(row.2, "2026-11-25T00:00:00.000Z");
        let found = database
            .accounts()
            .session(&token, test_clock::START)
            .unwrap()
            .unwrap();
        assert_eq!(found.principal.user.name, "alice");
        assert!(!found.renewed);
        assert_eq!(
            database
                .accounts()
                .session(&format!("{token}x"), test_clock::START)
                .unwrap(),
            None
        );
        let mut now = test_clock::START;
        for _ in 0..3 {
            now = now.plus(days(20));
            let lookup = database.accounts().session(&token, now).unwrap().unwrap();
            assert!(lookup.renewed, "used every 20 days, it never expires");
        }
        now = now.plus(days(31));
        assert_eq!(database.accounts().session(&token, now).unwrap(), None);
        assert_eq!(
            count(&database, "sessions"),
            0,
            "an expired session is deleted on lookup"
        );
    }

    #[test]
    fn a_session_is_extended_at_most_hourly() {
        let mut database = Database::open_in_memory(test_clock::fixed).unwrap();
        let user = database
            .accounts()
            .add_user("alice", &test_passwords::cheap(), test_clock::START)
            .unwrap()
            .user;
        let token = database
            .accounts()
            .create_session(user.id, test_clock::START)
            .unwrap();
        let renewed = |database: &mut Database, minutes: u64| {
            database
                .accounts()
                .session(&token, test_clock::START.plus(Duration::from_mins(minutes)))
                .unwrap()
                .unwrap()
                .renewed
        };
        assert!(!renewed(&mut database, 0));
        assert!(!renewed(&mut database, 59));
        assert!(renewed(&mut database, 60));
        assert!(!renewed(&mut database, 61));
        let expires: String = database
            .connection
            .query_row("SELECT expires_at FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(expires, "2026-11-25T01:00:00.000Z");
    }

    #[test]
    fn a_new_password_ends_every_other_session_and_removing_a_user_ends_all() {
        let mut database = Database::open_in_memory(test_clock::fixed).unwrap();
        let passwords = test_passwords::cheap();
        let user = database
            .accounts()
            .add_user("alice", &passwords, test_clock::START)
            .unwrap()
            .user;
        let now = test_clock::START;
        let here = database.accounts().create_session(user.id, now).unwrap();
        let there = database.accounts().create_session(user.id, now).unwrap();
        let keep = database
            .accounts()
            .session(&here, now)
            .unwrap()
            .unwrap()
            .principal;
        assert!(matches!(
            passwords.hash_new("short"),
            Err(AccountError::PasswordTooShort)
        ));
        let hash = passwords.hash_new("correct horse battery").unwrap();
        database
            .accounts()
            .set_password(user.id, &hash, Some(keep.session_id), now)
            .unwrap();
        assert!(database.accounts().session(&here, now).unwrap().is_some());
        assert_eq!(database.accounts().session(&there, now).unwrap(), None);
        assert!(verify(&mut database, "alice", "correct horse battery").is_some());

        let reset = database
            .accounts()
            .reset_password("alice", &passwords, now)
            .unwrap();
        assert_eq!(database.accounts().session(&here, now).unwrap(), None);
        assert!(verify(&mut database, "alice", &reset).is_some());
        assert!(matches!(
            database.accounts().reset_password("bob", &passwords, now),
            Err(AccountError::NoUser { .. })
        ));

        let again = database.accounts().create_session(user.id, now).unwrap();
        database
            .accounts()
            .create_key(user.id, "script", KeyScope::Full, now)
            .unwrap();
        database.accounts().remove_user("ALICE").unwrap();
        assert_eq!(database.accounts().session(&again, now).unwrap(), None);
        assert_eq!(
            count(&database, "api_keys"),
            0,
            "removing a user removes their keys"
        );
    }

    #[test]
    fn validity_follows_ending_expiring_and_password_changes() {
        let mut database = Database::open_in_memory(test_clock::fixed).unwrap();
        let passwords = test_passwords::cheap();
        let user = database
            .accounts()
            .add_user("alice", &passwords, test_clock::START)
            .unwrap()
            .user;
        let now = test_clock::START;
        let session = |database: &mut Database| {
            let token = database.accounts().create_session(user.id, now).unwrap();
            Principal::Session(
                database
                    .accounts()
                    .session(&token, now)
                    .unwrap()
                    .unwrap()
                    .principal,
            )
        };
        let first = session(&mut database);
        let second = session(&mut database);
        assert!(database.accounts().valid(&first, now).unwrap());
        assert!(database.accounts().valid(&second, now).unwrap());
        let Principal::Session(ended) = &first else {
            panic!("a session principal");
        };
        database.accounts().end_session(ended.session_id).unwrap();
        assert!(!database.accounts().valid(&first, now).unwrap());
        let hash = passwords.hash_new("a long new password").unwrap();
        database
            .accounts()
            .set_password(user.id, &hash, None, now)
            .unwrap();
        assert!(!database.accounts().valid(&second, now).unwrap());
        let third = session(&mut database);
        assert!(
            !database
                .accounts()
                .valid(&third, now.plus(days(31)))
                .unwrap()
        );
        assert!(
            database
                .accounts()
                .valid(&third, now.plus(days(29)))
                .unwrap()
        );
    }

    #[test]
    fn expired_sessions_are_pruned() {
        let mut database = Database::open_in_memory(test_clock::fixed).unwrap();
        let user = database
            .accounts()
            .add_user("alice", &test_passwords::cheap(), test_clock::START)
            .unwrap()
            .user;
        database
            .accounts()
            .create_session(user.id, test_clock::START)
            .unwrap();
        let later = test_clock::START.plus(days(31));
        database.accounts().create_session(user.id, later).unwrap();
        assert_eq!(database.accounts().prune_sessions(later).unwrap(), 1);
        assert_eq!(count(&database, "sessions"), 1);
    }

    #[test]
    fn key_names_are_1_to_64_javascript_characters() {
        let Alice { mut database, user } = with_alice();
        let now = test_clock::START;
        for bad in [" ", "", &"k".repeat(65), &"\u{1f600}".repeat(33)] {
            let error = database
                .accounts()
                .create_key(user.id, bad, KeyScope::Read, now)
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "a key name is 1-64 characters",
                "{bad:?}"
            );
        }
        assert!(
            database
                .accounts()
                .create_key(user.id, &"\u{1f600}".repeat(32), KeyScope::Read, now)
                .is_ok(),
            "32 emoji are 64 UTF-16 units"
        );
    }

    #[test]
    fn api_keys_keep_only_their_hash_and_serve_their_json() {
        let Alice { mut database, user } = with_alice();
        let now = test_clock::START;
        let NewApiKey { key, api_key } = database
            .accounts()
            .create_key(user.id, " script ", KeyScope::Read, now)
            .unwrap();
        assert_eq!(key.len(), 46);
        assert!(key.starts_with("sl_"));
        assert_eq!(
            api_key,
            ApiKeyView {
                id: api_key.id,
                user: "alice".to_owned(),
                name: "script".to_owned(),
                scope: KeyScope::Read,
                prefix: key[..9].to_owned(),
                created_at: "2026-10-26T00:00:00.000Z".to_owned(),
                last_used_at: None,
                revoked_at: None,
            }
        );
        let json = serde_json::to_string(&NewApiKey {
            key: "sl_x".to_owned(),
            api_key: api_key.clone(),
        })
        .unwrap();
        assert_eq!(
            json,
            format!(
                r#"{{"key":"sl_x","apiKey":{{"id":{},"user":"alice","name":"script","scope":"read","prefix":"{}","createdAt":"2026-10-26T00:00:00.000Z","lastUsedAt":null,"revokedAt":null}}}}"#,
                api_key.id,
                &key[..9]
            )
        );
        let stored: String = database
            .connection
            .query_row(
                "SELECT key_sha256 FROM api_keys WHERE id = ?",
                [api_key.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, token::sha256_hex(key.as_bytes()));
    }

    #[test]
    fn api_keys_record_use_once_a_minute_and_stop_when_revoked() {
        let Alice { mut database, user } = with_alice();
        let mut now = test_clock::START;
        let NewApiKey { key, api_key } = database
            .accounts()
            .create_key(user.id, "script", KeyScope::Read, now)
            .unwrap();
        let principal = database
            .accounts()
            .key_principal(&key, now)
            .unwrap()
            .unwrap();
        assert_eq!(principal.key_name, "script");
        assert_eq!(principal.scope, KeyScope::Read);
        assert_eq!(principal.user.name, "alice");
        let last_used = |database: &mut Database| {
            database.accounts().list_keys().unwrap()[0]
                .last_used_at
                .clone()
        };
        assert_eq!(
            last_used(&mut database).as_deref(),
            Some("2026-10-26T00:00:00.000Z")
        );
        now = now.plus(Duration::from_secs(30));
        database.accounts().key_principal(&key, now).unwrap();
        assert_eq!(
            last_used(&mut database).as_deref(),
            Some("2026-10-26T00:00:00.000Z")
        );
        now = now.plus(Duration::from_secs(30));
        database.accounts().key_principal(&key, now).unwrap();
        assert_eq!(
            last_used(&mut database).as_deref(),
            Some("2026-10-26T00:01:00.000Z")
        );

        assert_eq!(
            database
                .accounts()
                .key_principal(&format!("{key}x"), now)
                .unwrap(),
            None
        );
        assert_eq!(
            database.accounts().key_principal(&key[3..], now).unwrap(),
            None
        );
        let revoked = database.accounts().revoke_key(api_key.id, now).unwrap();
        assert_eq!(
            revoked.revoked_at.as_deref(),
            Some("2026-10-26T00:01:00.000Z")
        );
        assert_eq!(database.accounts().key_principal(&key, now).unwrap(), None);
        let again = database
            .accounts()
            .revoke_key(api_key.id, now.plus(Duration::from_secs(5)))
            .unwrap();
        assert_eq!(
            again.revoked_at, revoked.revoked_at,
            "revoking again keeps the first time"
        );
        let missing = database.accounts().revoke_key(999, now).unwrap_err();
        assert_eq!(missing.to_string(), "no API key 999");
        let key_principal = Principal::Key(principal);
        assert!(!database.accounts().valid(&key_principal, now).unwrap());
        let newer = database
            .accounts()
            .create_key(user.id, "newer", KeyScope::Full, now)
            .unwrap()
            .api_key;
        let ids: Vec<i64> = database
            .accounts()
            .list_keys()
            .unwrap()
            .iter()
            .map(|key| key.id)
            .collect();
        assert_eq!(ids, [newer.id, api_key.id], "newest first");
    }
}
