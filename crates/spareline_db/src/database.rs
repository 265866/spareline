//! The connection to spareline's SQLite database, and the migrations that run when it opens.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use spareline_core::Timestamp;

use crate::accounts::Accounts;
use crate::control_sessions::ControlSessions;
use crate::database_error::DatabaseError;
use crate::device_events::DeviceEvents;
use crate::health_store::HealthStore;
use crate::inventory::Inventory;
use crate::message_store::MessageStore;
use crate::migration::{AppliedMigration, MIGRATION_FILES, Migration};
use crate::resolve;

/// The statement that creates the table of applied migrations, as spareline-old wrote it.
const CREATE_MIGRATIONS: &str = "CREATE TABLE IF NOT EXISTS _migrations (id INTEGER PRIMARY KEY, file TEXT NOT NULL, applied_at TEXT NOT NULL)";

/// The PRAGMAs every connection sets, in spareline-old's order. WAL with `synchronous = NORMAL`
/// keeps SD card writes low: a power cut loses at most the last few transactions, never
/// consistency.
const PRAGMAS: [&str; 4] = [
    "PRAGMA journal_mode = WAL",
    "PRAGMA synchronous = NORMAL",
    "PRAGMA foreign_keys = ON",
    "PRAGMA busy_timeout = 5000",
];

/// spareline's database: one SQLite connection, migrated to the current schema.
///
/// Every method is synchronous and finishes its statements before it returns, so an async
/// server can keep one `Database` behind a mutex and hold the lock only for the call. The stores
/// it hands out borrow it for as long as they live.
#[derive(Debug)]
pub struct Database {
    pub(crate) connection: Connection,
}

impl Database {
    /// Opens the database at `path`, creating it and its parent directories when they don't
    /// exist, sets the PRAGMAs, and applies the migrations it lacks, as spareline-old's `openDb`
    /// does.
    ///
    /// `clock` gives the time for each `_migrations.applied_at` and for the name of the
    /// pre-migration snapshot. `on_applied` is called after each migration commits, so the
    /// caller can log it with [`AppliedMigration::log_line`].
    ///
    /// Before the first pending migration of a database that already had migrations, the
    /// database is copied with `VACUUM INTO` to
    /// `<directory of path>/backups/pre-migration-<NNN>-<time with : and . as ->.db`.
    ///
    /// # Errors
    ///
    /// Fails when a directory can't be created, SQLite can't open the file or set a PRAGMA, the
    /// snapshot fails, or a migration fails. A failed migration is rolled back, and the ones
    /// before it stay applied.
    pub fn open(
        path: &Path,
        clock: impl Fn() -> Timestamp,
        on_applied: impl FnMut(&AppliedMigration),
    ) -> Result<Self, DatabaseError> {
        let absolute = resolve::resolve(path).map_err(|source| {
            DatabaseError::io(format!("cannot resolve {}", path.display()), source)
        })?;
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).map_err(|source| {
                DatabaseError::io(format!("cannot create {}", parent.display()), source)
            })?;
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut connection = Connection::open_with_flags(path, flags)?;
        set_pragmas(&connection)?;
        let migrations = Migration::ordered(&MIGRATION_FILES)?;
        migrate(
            &mut connection,
            &migrations,
            Some(&absolute),
            &clock,
            on_applied,
        )?;
        Ok(Self { connection })
    }

    /// Opens a new database in memory, with the PRAGMAs and every migration, for tests of code
    /// that uses the database. Nothing is snapshotted.
    ///
    /// # Errors
    ///
    /// Fails when SQLite fails.
    pub fn open_in_memory(clock: impl Fn() -> Timestamp) -> Result<Self, DatabaseError> {
        let mut connection = Connection::open_in_memory()?;
        set_pragmas(&connection)?;
        let migrations = Migration::ordered(&MIGRATION_FILES)?;
        migrate(&mut connection, &migrations, None, &clock, |_| {})?;
        Ok(Self { connection })
    }

    /// Users, sessions and API keys.
    pub fn accounts(&mut self) -> Accounts<'_> {
        Accounts::new(&mut self.connection)
    }

    /// Phones, SIMs, their placements, numbers and the edits people make to them.
    pub fn inventory(&mut self) -> Inventory<'_> {
        Inventory::new(&mut self.connection)
    }

    /// The device event log behind the event stream.
    pub fn device_events(&mut self) -> DeviceEvents<'_> {
        DeviceEvents::new(&mut self.connection)
    }

    /// Texts, their attachments' metadata, and the ingest cursors.
    pub fn messages(&mut self) -> MessageStore<'_> {
        MessageStore::new(&mut self.connection)
    }

    /// The five-minute health aggregates.
    pub fn health(&mut self) -> HealthStore<'_> {
        HealthStore::new(&mut self.connection)
    }

    /// The audit rows of screen viewers that can send input.
    pub fn control_sessions(&mut self) -> ControlSessions<'_> {
        ControlSessions::new(&mut self.connection)
    }
}

/// Sets [`PRAGMAS`]. Several of them answer with a row, which is read and dropped.
fn set_pragmas(connection: &Connection) -> Result<(), DatabaseError> {
    for pragma in PRAGMAS {
        let mut statement = connection.prepare(pragma)?;
        let mut rows = statement.query([])?;
        while rows.next()?.is_some() {}
    }
    Ok(())
}

/// Applies the migrations whose ids `_migrations` lacks, each in its own transaction with its
/// `_migrations` row, as spareline-old's `runMigrations` does. `database_path` is the absolute
/// path of a database on disk, which is snapshotted before its first pending migration when it
/// already had migrations; an in-memory database has none.
pub(crate) fn migrate(
    connection: &mut Connection,
    migrations: &[Migration],
    database_path: Option<&Path>,
    clock: &dyn Fn() -> Timestamp,
    mut on_applied: impl FnMut(&AppliedMigration),
) -> Result<(), DatabaseError> {
    connection.execute_batch(CREATE_MIGRATIONS)?;
    let applied: HashSet<i64> = {
        let mut statement = connection.prepare("SELECT id FROM _migrations")?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    let mut snapshot_due = database_path.filter(|_| !applied.is_empty());
    for migration in migrations {
        if applied.contains(&migration.id) {
            continue;
        }
        if let Some(path) = snapshot_due.take() {
            snapshot(connection, path, migration.id, clock())?;
        }
        let transaction = connection.transaction()?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO _migrations (id, file, applied_at) VALUES (?, ?, ?)",
            (migration.id, migration.file, clock().to_string()),
        )?;
        transaction.commit()?;
        on_applied(&AppliedMigration::new(
            migration.id,
            migration.file,
            clock(),
        ));
    }
    Ok(())
}

/// Copies the database to `backups/pre-migration-<NNN>-<stamp>.db` beside it, before the
/// migration `id`.
fn snapshot(
    connection: &Connection,
    database_path: &Path,
    id: i64,
    now: Timestamp,
) -> Result<(), DatabaseError> {
    let directory = database_path
        .parent()
        .unwrap_or_else(|| Path::new("/"))
        .join("backups");
    fs::create_dir_all(&directory).map_err(|source| {
        DatabaseError::io(format!("cannot create {}", directory.display()), source)
    })?;
    let stamp = now.to_string().replace([':', '.'], "-");
    let target = directory.join(format!("pre-migration-{id:03}-{stamp}.db"));
    let target_text = target.to_str().ok_or_else(|| DatabaseError::NonUtf8Path {
        path: target.display().to_string(),
    })?;
    connection.execute("VACUUM INTO ?", [target_text])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Mutex;
    use std::thread;

    use rusqlite::types::FromSql;
    use serde::Deserialize;

    use super::*;
    use crate::account_error::AccountError;
    use crate::migration::MigrationFile;
    use crate::passwords::Passwords;
    use crate::test_clock;
    use crate::test_dir::TestDir;

    const EXPECTED_TABLES: [&str; 15] = [
        "api_keys",
        "attachments",
        "control_sessions",
        "device_events",
        "health_5m",
        "ingest_cursors",
        "inventory_edits",
        "messages",
        "number_bindings",
        "numbers",
        "phones",
        "sessions",
        "sim_placements",
        "sims",
        "users",
    ];

    /// A clock that moves on a millisecond each time it is read.
    fn ticking() -> impl Fn() -> Timestamp {
        let ticks = Cell::new(0);
        move || {
            ticks.set(ticks.get() + 1);
            Timestamp::from_millis(test_clock::START.millis() + ticks.get())
        }
    }

    fn pragma<T: FromSql>(database: &Database, name: &str) -> T {
        database
            .connection
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .unwrap()
    }

    /// One row of `sqlite_master`, as the fixture holds it.
    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct SchemaRow {
        #[serde(rename = "type")]
        kind: String,
        name: String,
        tbl_name: String,
        sql: Option<String>,
    }

    fn schema(connection: &Connection) -> Vec<SchemaRow> {
        let mut statement = connection
            .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name")
            .unwrap();
        statement
            .query_map([], |row| {
                Ok(SchemaRow {
                    kind: row.get(0)?,
                    name: row.get(1)?,
                    tbl_name: row.get(2)?,
                    sql: row.get(3)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<SchemaRow>, _>>()
            .unwrap()
    }

    fn file_migrations(files: &[MigrationFile]) -> Vec<Migration> {
        Migration::ordered(files).unwrap()
    }

    fn open_raw(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        set_pragmas(&connection).unwrap();
        connection
    }

    #[test]
    fn a_new_database_gets_every_table_wal_and_foreign_keys() {
        let dir = TestDir::new();
        let database = Database::open(
            &dir.path().join("data").join("spareline.db"),
            test_clock::fixed,
            |_| {},
        )
        .unwrap();
        let mut statement = database
            .connection
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_migrations' ORDER BY name",
            )
            .unwrap();
        let tables: Vec<String> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap();
        drop(statement);
        assert_eq!(tables, EXPECTED_TABLES);
        assert_eq!(pragma::<String>(&database, "journal_mode"), "wal");
        assert_eq!(pragma::<i64>(&database, "foreign_keys"), 1);
        assert_eq!(pragma::<i64>(&database, "synchronous"), 1, "NORMAL is 1");
        assert_eq!(pragma::<i64>(&database, "busy_timeout"), 5000);
        assert_eq!(pragma::<i64>(&database, "page_size"), 4096);
        assert_eq!(pragma::<String>(&database, "encoding"), "UTF-8");
        assert_eq!(pragma::<i64>(&database, "auto_vacuum"), 0);
        assert_eq!(pragma::<i64>(&database, "user_version"), 0);
        assert!(
            !dir.path().join("data").join("backups").exists(),
            "a new database isn't snapshotted"
        );
    }

    #[test]
    fn the_schema_is_exactly_what_spareline_old_migrated() {
        let dir = TestDir::new();
        let database =
            Database::open(&dir.path().join("spareline.db"), test_clock::fixed, |_| {}).unwrap();
        let migrated = schema(&database.connection);
        let fixture: Vec<SchemaRow> =
            serde_json::from_str(include_str!("../tests/fixtures/schema.json")).unwrap();
        assert_eq!(
            migrated, fixture,
            "the schema Bun's openDb wrote from spareline-old's migrations"
        );

        let reference = Connection::open(dir.path().join("reference.db")).unwrap();
        reference.execute_batch(CREATE_MIGRATIONS).unwrap();
        for file in MIGRATION_FILES {
            reference.execute_batch(file.sql).unwrap();
        }
        assert_eq!(
            migrated,
            schema(&reference),
            "the embedded files applied by plain execute_batch"
        );
        let max: i64 = database
            .connection
            .query_row("SELECT max(id) FROM _migrations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            max, 3,
            "the deploy kit's rollback guidance relies on max(id) = 3"
        );
    }

    #[test]
    fn migrations_are_recorded_and_logged_once() {
        let dir = TestDir::new();
        let path = dir.path().join("spareline.db");
        let mut logged = Vec::new();
        let database =
            Database::open(&path, ticking(), |applied| logged.push(applied.log_line())).unwrap();
        assert_eq!(
            logged,
            [
                r#"{"at":"2026-10-26T00:00:00.002Z","event":"migration-applied","file":"001_init.sql"}"#,
                r#"{"at":"2026-10-26T00:00:00.004Z","event":"migration-applied","file":"002_auth.sql"}"#,
                r#"{"at":"2026-10-26T00:00:00.006Z","event":"migration-applied","file":"003_api_keys.sql"}"#,
            ]
        );
        let mut statement = database
            .connection
            .prepare("SELECT id, file, applied_at FROM _migrations ORDER BY id")
            .unwrap();
        let rows: Vec<String> = statement
            .query_map([], |row| {
                Ok(format!(
                    "{}|{}|{}",
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?
                ))
            })
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap();
        assert_eq!(
            rows,
            [
                "1|001_init.sql|2026-10-26T00:00:00.001Z",
                "2|002_auth.sql|2026-10-26T00:00:00.003Z",
                "3|003_api_keys.sql|2026-10-26T00:00:00.005Z",
            ]
        );
        drop(statement);
        drop(database);
        let mut again = 0;
        drop(Database::open(&path, ticking(), |_| again += 1).unwrap());
        assert_eq!(again, 0, "reopening applies nothing twice");
        assert!(
            !dir.path().join("backups").exists(),
            "nothing pending, nothing snapshotted"
        );
    }

    #[test]
    fn a_later_migration_snapshots_the_existing_database_first() {
        let dir = TestDir::new();
        let path = dir.path().join("spareline.db");
        drop(Database::open(&path, test_clock::fixed, |_| {}).unwrap());
        {
            let connection = open_raw(&path);
            connection
                .execute(
                    "INSERT INTO numbers (e164_source, note, created_at) VALUES ('none', 'Facebook ad', '2026-10-21')",
                    [],
                )
                .unwrap();
        }
        let mut files = MIGRATION_FILES.to_vec();
        files.push(MigrationFile {
            file: "004_add_number_order.sql",
            sql: "ALTER TABLE numbers ADD COLUMN sort INTEGER;",
        });
        let mut connection = open_raw(&path);
        migrate(
            &mut connection,
            &file_migrations(&files),
            Some(&path),
            &test_clock::fixed,
            |_| {},
        )
        .unwrap();
        let row: String = connection
            .query_row(
                "SELECT note || '|' || coalesce(sort, 'null') FROM numbers",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(row, "Facebook ad|null");
        let backups: Vec<String> = fs::read_dir(dir.path().join("backups"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(backups, ["pre-migration-004-2026-10-26T00-00-00-000Z.db"]);
        let snapshot = Connection::open(dir.path().join("backups").join(&backups[0])).unwrap();
        let columns: i64 = snapshot
            .query_row(
                "SELECT count(*) FROM pragma_table_info('numbers') WHERE name = 'sort'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(columns, 0, "the snapshot is from before the migration");
    }

    #[test]
    fn a_new_database_is_not_snapshotted_and_an_existing_one_once_per_run() {
        let dir = TestDir::new();
        let path = dir.path().join("spareline.db");
        let first = [
            MigrationFile {
                file: "001_a.sql",
                sql: "CREATE TABLE a (x);",
            },
            MigrationFile {
                file: "020_c.sql",
                sql: "CREATE TABLE c (x);",
            },
        ];
        let mut connection = open_raw(&path);
        migrate(
            &mut connection,
            &file_migrations(&first),
            Some(&path),
            &test_clock::fixed,
            |_| {},
        )
        .unwrap();
        assert!(!dir.path().join("backups").exists());
        let mut second = first.to_vec();
        second.push(MigrationFile {
            file: "010_b.sql",
            sql: "CREATE TABLE b (x);",
        });
        second.push(MigrationFile {
            file: "030_d.sql",
            sql: "CREATE TABLE d (x);",
        });
        migrate(
            &mut connection,
            &file_migrations(&second),
            Some(&path),
            &test_clock::fixed,
            |_| {},
        )
        .unwrap();
        let mut statement = connection
            .prepare("SELECT id FROM _migrations ORDER BY id")
            .unwrap();
        let ids: Vec<i64> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<i64>, _>>()
            .unwrap();
        assert_eq!(ids, [1, 10, 20, 30]);
        let backups: Vec<String> = fs::read_dir(dir.path().join("backups"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(
            backups[0].starts_with("pre-migration-010-"),
            "{}",
            backups[0]
        );
    }

    #[test]
    fn a_failed_migration_rolls_back_alone() {
        let dir = TestDir::new();
        let path = dir.path().join("spareline.db");
        let files = [
            MigrationFile {
                file: "001_a.sql",
                sql: "CREATE TABLE a (x);",
            },
            MigrationFile {
                file: "002_b.sql",
                sql: "CREATE TABLE b (x); CREATE TABLE a (y);",
            },
        ];
        let mut connection = open_raw(&path);
        let error = migrate(
            &mut connection,
            &file_migrations(&files),
            Some(&path),
            &test_clock::fixed,
            |_| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name IN ('a', 'b')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1, "a stays, b is rolled back");
        let ids: i64 = connection
            .query_row("SELECT count(*) FROM _migrations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(ids, 1);
    }

    #[test]
    fn a_database_behind_a_mutex_serves_other_threads() {
        let shared = Mutex::new(Database::open_in_memory(test_clock::fixed).unwrap());
        let passwords = Passwords::new();
        let has_users = thread::scope(|scope| {
            scope
                .spawn(|| {
                    assert!(
                        matches!(
                            passwords.hash_new("short"),
                            Err(AccountError::PasswordTooShort)
                        ),
                        "a shared Passwords checks lengths on any thread"
                    );
                    shared.lock().unwrap().accounts().has_users().unwrap()
                })
                .join()
                .unwrap()
        });
        assert!(!has_users);
    }

    #[test]
    fn constraints_hold() {
        let database = Database::open_in_memory(test_clock::fixed).unwrap();
        let connection = database.connection;
        let now = "2026-10-21T00:00:00.000Z";
        connection
            .execute(
                "INSERT INTO phones (serial, site, last_state, first_seen_at) VALUES ('A', 'home', 'device', ?)",
                [now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sims (iccid, first_seen_at, last_seen_at) VALUES ('8901', ?1, ?1), ('8902', ?1, ?1)",
                [now],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (1, 1, 0, 1, ?)",
                [now],
            )
            .unwrap();
        let unique = connection
            .execute(
                "INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (2, 1, 0, 2, ?)",
                [now],
            )
            .unwrap_err();
        assert!(unique.to_string().contains("UNIQUE"), "{unique}");
        let check = connection
            .execute(
                "INSERT INTO numbers (e164, e164_source, created_at) VALUES ('5125550101', 'detected', ?)",
                [now],
            )
            .unwrap_err();
        assert!(check.to_string().contains("CHECK"), "{check}");
        let foreign = connection
            .execute(
                "INSERT INTO sim_placements (sim_id, phone_id, slot, sub_id, from_at) VALUES (9, 1, 1, 1, ?)",
                [now],
            )
            .unwrap_err();
        assert!(foreign.to_string().contains("FOREIGN KEY"), "{foreign}");
    }
}
