//! The schema migrations, embedded byte for byte from spareline-old.

use spareline_core::Timestamp;

use crate::database_error::DatabaseError;

/// The migration files, in the order spareline-old applies them. Their bytes match
/// `spareline-old/src/db/migrations/` exactly, and no migration is added: the deploy kit's
/// rollback guidance treats `max(_migrations.id) = 3` as safe for the old version.
pub(crate) const MIGRATION_FILES: [MigrationFile; 3] = [
    MigrationFile {
        file: "001_init.sql",
        sql: include_str!("migrations/001_init.sql"),
    },
    MigrationFile {
        file: "002_auth.sql",
        sql: include_str!("migrations/002_auth.sql"),
    },
    MigrationFile {
        file: "003_api_keys.sql",
        sql: include_str!("migrations/003_api_keys.sql"),
    },
];

/// A migration file: its name and its SQL.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MigrationFile {
    pub(crate) file: &'static str,
    pub(crate) sql: &'static str,
}

/// A migration with the id its file name gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Migration {
    pub(crate) id: i64,
    pub(crate) file: &'static str,
    pub(crate) sql: &'static str,
}

impl Migration {
    /// The migrations among `files` whose names end in `.sql`, in ascending order of id, as
    /// spareline-old's `listMigrations` reads a directory. Ids may skip numbers but must be
    /// unique and start at 001.
    ///
    /// # Errors
    ///
    /// Fails, with spareline-old's message, on a file name not of the form `NNN_name.sql` and
    /// on an id that is 000 or repeats.
    pub(crate) fn ordered(files: &[MigrationFile]) -> Result<Vec<Self>, DatabaseError> {
        let mut migrations = Vec::new();
        for file in files {
            if file.file.strip_suffix(".sql").is_none() {
                continue;
            }
            let id = migration_id(file.file).ok_or_else(|| DatabaseError::Migrations {
                message: format!("migration file name must be NNN_name.sql: {}", file.file),
            })?;
            migrations.push(Self {
                id,
                file: file.file,
                sql: file.sql,
            });
        }
        migrations.sort_by_key(|migration| migration.id);
        let mut previous: Option<&Self> = None;
        for migration in &migrations {
            if migration.id == 0 || previous.is_some_and(|before| before.id >= migration.id) {
                return Err(DatabaseError::Migrations {
                    message: format!(
                        "migration numbers must be unique and start at 001; {} and {} conflict",
                        previous.map_or("(none)", |before| before.file),
                        migration.file
                    ),
                });
            }
            previous = Some(migration);
        }
        Ok(migrations)
    }
}

/// The id of a file named `NNN_name.sql`, where `name` is one or more of `a-z`, `0-9` and `_`.
fn migration_id(file: &str) -> Option<i64> {
    let stem = file.strip_suffix(".sql")?;
    let digits = stem.get(..3)?;
    let name = stem.get(3..)?.strip_prefix('_')?;
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) || !name_ok {
        return None;
    }
    digits.parse().ok()
}

/// A migration that [`Database::open`](crate::Database::open) applied, for the caller to log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedMigration {
    id: i64,
    file: &'static str,
    at: Timestamp,
}

impl AppliedMigration {
    pub(crate) fn new(id: i64, file: &'static str, at: Timestamp) -> Self {
        Self { id, file, at }
    }

    /// The migration's id, such as 1 for `001_init.sql`.
    #[must_use]
    pub fn id(&self) -> i64 {
        self.id
    }

    /// The migration's file name, such as `001_init.sql`.
    #[must_use]
    pub fn file(&self) -> &'static str {
        self.file
    }

    /// When it finished.
    #[must_use]
    pub fn at(&self) -> Timestamp {
        self.at
    }

    /// The log line spareline-old wrote to stderr for it:
    /// `{"at":"2026-10-28T08:41:51.104Z","event":"migration-applied","file":"001_init.sql"}`.
    #[must_use]
    pub fn log_line(&self) -> String {
        format!(
            r#"{{"at":"{}","event":"migration-applied","file":"{}"}}"#,
            self.at, self.file
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn file(file: &'static str) -> MigrationFile {
        MigrationFile { file, sql: "" }
    }

    #[test]
    fn the_embedded_migrations_are_in_order() {
        let ids: Vec<i64> = Migration::ordered(&MIGRATION_FILES)
            .unwrap()
            .iter()
            .map(|migration| migration.id)
            .collect();
        assert_eq!(ids, [1, 2, 3]);
    }

    #[test]
    fn numbers_may_skip_ranges_but_must_be_unique() {
        let files = [
            file("020_screens.sql"),
            file("001_init.sql"),
            file("010_sms.sql"),
            file("notes.txt"),
        ];
        let ids: Vec<i64> = Migration::ordered(&files)
            .unwrap()
            .iter()
            .map(|migration| migration.id)
            .collect();
        assert_eq!(ids, [1, 10, 20]);
        let conflict = Migration::ordered(&[file("010_sms.sql"), file("010_other.sql")])
            .unwrap_err()
            .to_string();
        assert!(conflict.contains("conflict"), "{conflict}");
        let zero = Migration::ordered(&[file("000_zero.sql")])
            .unwrap_err()
            .to_string();
        assert_eq!(
            zero,
            "migration numbers must be unique and start at 001; (none) and 000_zero.sql conflict"
        );
    }

    #[test]
    fn names_follow_the_pattern() {
        for bad in [
            "1_init.sql",
            "001-init.sql",
            "001_Init.sql",
            "001_.sql",
            "0011_x.sql",
        ] {
            let error = Migration::ordered(&[file(bad)]).unwrap_err().to_string();
            assert_eq!(
                error,
                format!("migration file name must be NNN_name.sql: {bad}")
            );
        }
    }

    #[test]
    fn the_log_line_is_spareline_olds() {
        let applied =
            AppliedMigration::new(1, "001_init.sql", Timestamp::from_millis(1_793_176_911_104));
        assert_eq!(
            applied.log_line(),
            r#"{"at":"2026-10-28T08:41:51.104Z","event":"migration-applied","file":"001_init.sql"}"#
        );
    }
}
