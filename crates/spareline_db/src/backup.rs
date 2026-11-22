//! Consistent copies of a live database, and integrity checks of copies, for the deploy kit's
//! `spareline backup SOURCE DESTINATION` and `spareline backup --check FILE`.

use std::fmt::{self, Display, Formatter};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use thiserror::Error;

use crate::resolve;

/// A finished backup. Its [`Display`] is the line spareline-old printed on success:
/// `snapshot saved to <absolute destination>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupSaved {
    destination: PathBuf,
}

impl BackupSaved {
    /// The absolute path of the new copy.
    #[must_use]
    pub fn destination(&self) -> &Path {
        &self.destination
    }
}

impl Display for BackupSaved {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "snapshot saved to {}", self.destination.display())
    }
}

/// A passed integrity check. Its [`Display`] is the line spareline-old printed:
/// `integrity ok: <absolute path>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrityOk {
    file: PathBuf,
}

impl IntegrityOk {
    /// The absolute path of the checked file.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }
}

impl Display for IntegrityOk {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "integrity ok: {}", self.file.display())
    }
}

/// A failed backup or check. Its message is the one spareline-old printed on stderr.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[must_use]
pub enum BackupError {
    /// The source isn't a regular file.
    #[error("backup source is not a file: {path}")]
    SourceNotFile {
        /// The absolute source path.
        path: String,
    },
    /// Something already exists at the destination, which is never overwritten.
    #[error("backup destination already exists: {path}")]
    DestinationExists {
        /// The absolute destination path.
        path: String,
    },
    /// `PRAGMA integrity_check` didn't answer with exactly one `ok` row.
    #[error("integrity_check failed for {path}: {rows}")]
    IntegrityFailed {
        /// The absolute path of the checked file.
        path: String,
        /// The rows it answered, as JSON: `[{"integrity_check":"..."}]`.
        rows: String,
    },
    /// A file system call failed. The message names the step and the path, as the live build
    /// words it, such as `cannot read backup source /app/data/spareline.db: No such file or
    /// directory (os error 2)`.
    #[error("{message}")]
    FileSystem {
        /// The message.
        message: String,
    },
    /// SQLite failed on a file.
    #[error("SQLite failed on {path}: {message}")]
    Sqlite {
        /// The absolute path of the file.
        path: String,
        /// SQLite's message as rusqlite words it, such as `file is not a database`.
        message: String,
    },
}

/// Copies the live database `source` to `destination` with `VACUUM INTO`, which reads one
/// consistent snapshot while the service keeps writing, checks the copy with
/// `PRAGMA integrity_check`, and publishes it atomically, as spareline-old's `backupDatabase`
/// does.
///
/// The copy is written in a new directory beside the destination, named
/// `.<destination name>-XXXXXX`, then synced, hard-linked to the destination, which fails if
/// something appeared there meanwhile, and the destination's directory is synced. The temporary
/// directory is removed whether the backup succeeds or not. The source is opened read-only with
/// `busy_timeout = 30000`.
///
/// # Errors
///
/// Fails when the source isn't a regular file, the destination exists, SQLite fails, the copy
/// fails its integrity check, or a file system call fails.
pub fn backup(source: &Path, destination: &Path) -> Result<BackupSaved, BackupError> {
    let source_path = absolute(source)?;
    let destination_path = absolute(destination)?;
    let metadata = fs::metadata(&source_path).map_err(|error| BackupError::FileSystem {
        message: format!(
            "cannot read backup source {}: {error}",
            source_path.display()
        ),
    })?;
    if !metadata.is_file() {
        return Err(BackupError::SourceNotFile {
            path: source_path.display().to_string(),
        });
    }
    match fs::symlink_metadata(&destination_path) {
        Ok(_) => {
            return Err(BackupError::DestinationExists {
                path: destination_path.display().to_string(),
            });
        }
        Err(error) if is_missing(&error) => {}
        Err(error) => {
            return Err(BackupError::FileSystem {
                message: format!(
                    "cannot read backup destination {}: {error}",
                    destination_path.display()
                ),
            });
        }
    }
    let directory = destination_path
        .parent()
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
    let name = destination_path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let work = make_temporary_directory(&directory, &format!(".{name}-"))?;
    let result = copy_and_publish(&source_path, &work, &destination_path, &directory);
    let removed = match fs::remove_dir_all(&work) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(BackupError::FileSystem {
            message: format!("cannot remove {}: {error}", work.display()),
        }),
        _ => Ok(()),
    };
    result?;
    removed?;
    Ok(BackupSaved {
        destination: destination_path,
    })
}

/// Checks `file` with `PRAGMA integrity_check`, which must answer with exactly one row, `ok`.
/// The file is opened read-only by its absolute path, which the result and any failure name.
///
/// # Errors
///
/// Fails when SQLite can't open or read the file, or the check finds a problem.
pub fn check(file: &Path) -> Result<IntegrityOk, BackupError> {
    let file = absolute(file)?;
    check_integrity(&file)?;
    Ok(IntegrityOk { file })
}

/// The body of [`backup`] after the temporary directory exists.
fn copy_and_publish(
    source: &Path,
    work: &Path,
    destination: &Path,
    directory: &Path,
) -> Result<(), BackupError> {
    let temporary = work.join("snapshot.db");
    {
        let live = Connection::open_with_flags(
            source,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| sqlite(&error, source))?;
        live.busy_timeout(Duration::from_secs(30))
            .map_err(|error| sqlite(&error, source))?;
        let target = temporary.to_str().ok_or_else(|| BackupError::FileSystem {
            message: format!("path is not valid UTF-8: {}", temporary.display()),
        })?;
        live.execute("VACUUM INTO ?", [target])
            .map_err(|error| sqlite(&error, source))?;
    }
    check_integrity(&temporary)?;
    sync(&temporary, File::options().read(true).write(true))?;
    fs::hard_link(&temporary, destination).map_err(|error| BackupError::FileSystem {
        message: format!(
            "cannot publish the backup at {}: {error}",
            destination.display()
        ),
    })?;
    sync(directory, File::options().read(true))
}

/// Opens `path` with `options` and syncs it to disk.
fn sync(path: &Path, options: &OpenOptions) -> Result<(), BackupError> {
    options
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| BackupError::FileSystem {
            message: format!("cannot sync {}: {error}", path.display()),
        })
}

/// One row of `PRAGMA integrity_check`, as spareline-old's error message shows it.
#[derive(Serialize)]
struct IntegrityRow {
    integrity_check: String,
}

/// Fails unless `PRAGMA integrity_check` of `path` answers with exactly one row, `ok`.
fn check_integrity(path: &Path) -> Result<(), BackupError> {
    let database = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| sqlite(&error, path))?;
    let rows: Vec<IntegrityRow> = database
        .prepare("PRAGMA integrity_check")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| {
                    Ok(IntegrityRow {
                        integrity_check: row.get(0)?,
                    })
                })?
                .collect()
        })
        .map_err(|error| sqlite(&error, path))?;
    match rows.as_slice() {
        [only] if only.integrity_check == "ok" => Ok(()),
        _ => Err(BackupError::IntegrityFailed {
            path: path.display().to_string(),
            rows: serde_json::to_string(&rows).unwrap_or_default(),
        }),
    }
}

/// Creates a new directory named `prefix` followed by six random letters and digits in
/// `directory`, readable only by its owner.
fn make_temporary_directory(directory: &Path, prefix: &str) -> Result<PathBuf, BackupError> {
    const LETTERS: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut builder = DirBuilder::new();
    restrict_to_owner(&mut builder);
    for _ in 0..100 {
        let mut suffix = String::with_capacity(6);
        while suffix.len() < 6 {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|error| BackupError::FileSystem {
                message: format!("cannot read random bytes: {error}"),
            })?;
            for byte in bytes {
                if byte < 248 && suffix.len() < 6 {
                    suffix.push(char::from(LETTERS[usize::from(byte % 62)]));
                }
            }
        }
        let candidate = directory.join(format!("{prefix}{suffix}"));
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(BackupError::FileSystem {
                    message: format!(
                        "cannot write the backup at {}: {error} at path \"{}\"",
                        directory.display(),
                        candidate.display()
                    ),
                });
            }
        }
    }
    Err(BackupError::FileSystem {
        message: format!(
            "cannot write the backup at {}: every temporary name tried exists",
            directory.display()
        ),
    })
}

/// Makes `builder` create directories with mode 0700.
fn restrict_to_owner(builder: &mut DirBuilder) {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
}

/// `path` made absolute against the working directory, with `.` and `..` resolved.
fn absolute(path: &Path) -> Result<PathBuf, BackupError> {
    resolve::resolve(path).map_err(|error| BackupError::FileSystem {
        message: format!("cannot resolve {}: {error}", path.display()),
    })
}

/// Whether a failed `lstat` means nothing is there. Only a missing entry counts: a parent that is
/// a file fails as Node's `lstat` reported it, `ENOTDIR`.
fn is_missing(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

/// A SQLite failure on the file at the absolute `path`, as the live build words it.
fn sqlite(error: &rusqlite::Error, path: &Path) -> BackupError {
    BackupError::Sqlite {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;
    use crate::{Database, test_clock};
    use std::os::unix::fs as unix_fs;

    fn database_with_a_note(path: &Path, note: &str) -> Database {
        let database = Database::open(path, test_clock::fixed, |_| {}).unwrap();
        database
            .connection
            .execute(
                "INSERT INTO numbers (e164_source, note, created_at) VALUES ('none', ?, '2026-10-21')",
                [note],
            )
            .unwrap();
        database
    }

    #[test]
    fn a_backup_is_a_consistent_copy_that_passes_integrity_check() {
        let dir = TestDir::new();
        let path = dir.path().join("spareline.db");
        let live = database_with_a_note(&path, "Craigslist");
        let target = dir.path().join("backup.db");
        let saved = backup(&path, &target).unwrap();
        assert_eq!(
            saved.to_string(),
            format!("snapshot saved to {}", target.display())
        );
        drop(live);
        let copy = Connection::open(&target).unwrap();
        let note: String = copy
            .query_row("SELECT note FROM numbers", [], |row| row.get(0))
            .unwrap();
        assert_eq!(note, "Craigslist");
        let mode: String = copy
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete", "a VACUUM INTO copy isn't in WAL mode");
        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".backup.db-"))
            .collect();
        assert_eq!(
            leftovers,
            Vec::<String>::new(),
            "the temporary directory is removed"
        );
        assert_eq!(
            check(&target).unwrap().to_string(),
            format!("integrity ok: {}", target.display())
        );
    }

    #[test]
    fn a_backup_never_overwrites() {
        let dir = TestDir::new();
        let path = dir.path().join("spareline.db");
        let _live = database_with_a_note(&path, "first");
        let target = dir.path().join("backup.db");
        backup(&path, &target).unwrap();
        let error = backup(&path, &target).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("backup destination already exists: {}", target.display())
        );
        assert!(target.exists(), "the first copy stays");
        let dangling = dir.path().join("dangling.db");
        unix_fs::symlink(dir.path().join("missing"), &dangling).unwrap();
        assert!(
            matches!(
                backup(&path, &dangling),
                Err(BackupError::DestinationExists { .. })
            ),
            "a dangling symbolic link counts as existing"
        );
    }

    #[test]
    fn a_relative_destination_is_reported_absolute() {
        let dir = TestDir::new();
        let path = dir.path().join("spareline.db");
        let _live = database_with_a_note(&path, "x");
        let target = dir.path().join("sub").join("..").join("copy.db");
        let saved = backup(&path, &target).unwrap();
        assert_eq!(saved.destination(), dir.path().join("copy.db"));
    }

    #[test]
    fn a_missing_source_or_directory_fails_as_the_live_build_words_it() {
        let dir = TestDir::new();
        let missing = dir.path().join("nothing.db");
        assert_eq!(
            backup(&missing, &dir.path().join("x.db"))
                .unwrap_err()
                .to_string(),
            format!(
                "cannot read backup source {}: No such file or directory (os error 2)",
                missing.display()
            )
        );
        assert_eq!(
            backup(dir.path(), &dir.path().join("x.db")).unwrap_err(),
            BackupError::SourceNotFile {
                path: dir.path().display().to_string()
            }
        );
        let path = dir.path().join("spareline.db");
        let _live = database_with_a_note(&path, "x");
        let nowhere = dir.path().join("no-such-dir").join("x.db");
        let message = backup(&path, &nowhere).unwrap_err().to_string();
        let parent = nowhere.parent().unwrap().display().to_string();
        assert!(
            message.starts_with(&format!(
                "cannot write the backup at {parent}: No such file or directory (os error 2) at \
                 path \"{parent}/.x.db-"
            )),
            "{message}"
        );
    }

    #[test]
    fn a_destination_under_a_file_names_the_destination() {
        let dir = TestDir::new();
        let path = dir.path().join("spareline.db");
        let _live = database_with_a_note(&path, "x");
        let file = dir.path().join("a-file");
        fs::write(&file, b"x").unwrap();
        let under = file.join("x.db");
        assert_eq!(
            backup(&path, &under).unwrap_err().to_string(),
            format!(
                "cannot read backup destination {}: Not a directory (os error 20)",
                under.display()
            )
        );
    }

    #[test]
    fn a_failed_backup_leaves_nothing_beside_the_destination() {
        let dir = TestDir::new();
        let junk = dir.path().join("junk.db");
        fs::write(&junk, b"this is not a database file, just some bytes to make it long enough to have a header page").unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        assert!(backup(&junk, &out.join("copy.db")).is_err());
        let left: Vec<_> = fs::read_dir(&out).unwrap().collect();
        assert!(
            left.is_empty(),
            "the temporary directory is removed: {left:?}"
        );
    }

    #[test]
    fn check_refuses_a_file_that_is_not_a_database() {
        let dir = TestDir::new();
        let junk = dir.path().join("junk.db");
        fs::write(&junk, b"this is not a database file, just some bytes to make it long enough to have a header page").unwrap();
        let error = check(&junk).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "SQLite failed on {}: file is not a database",
                junk.display()
            )
        );
        let missing_path = dir.path().join("missing.db");
        let missing = check(&missing_path).unwrap_err();
        assert_eq!(
            missing.to_string(),
            format!(
                "SQLite failed on {0}: unable to open database file: {0}",
                missing_path.display()
            )
        );
    }
}
