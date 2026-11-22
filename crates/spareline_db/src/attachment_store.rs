//! MMS part files, content-addressed under `DATA_DIR/attachments/`.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::{io, process};

use spareline_core::Timestamp;

use crate::database_error::DatabaseError;
use crate::token;

/// The directory of MMS part files, each named by the lowercase hex SHA-256 of its bytes, so
/// the same image sent twice is stored once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentStore {
    directory: PathBuf,
}

/// A saved part: its digest, which names its file, and its size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedAttachment {
    /// The lowercase hex SHA-256 of the bytes.
    pub sha256: String,
    /// The number of bytes.
    pub size: i64,
}

impl AttachmentStore {
    /// The store in `directory`, normally `DATA_DIR/attachments`. Nothing is created until a
    /// part is saved.
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// Saves `bytes` under their SHA-256 unless a file of that name exists. The bytes go to
    /// `<path>.<pid>.<now in ms>.tmp`, are synced, then renamed, so a crash or power cut never
    /// leaves a partial file under the final name.
    ///
    /// # Errors
    ///
    /// Fails when the directory or the file can't be written.
    pub fn save(&self, bytes: &[u8], now: Timestamp) -> Result<SavedAttachment, DatabaseError> {
        let sha256 = token::sha256_hex(bytes);
        let path = self.directory.join(&sha256);
        if !path.exists() {
            fs::create_dir_all(&self.directory).map_err(|source| {
                DatabaseError::io(
                    format!("cannot create {}", self.directory.display()),
                    source,
                )
            })?;
            let temporary = PathBuf::from(format!(
                "{}.{}.{}.tmp",
                path.display(),
                process::id(),
                now.millis()
            ));
            write_synced(&temporary, bytes).map_err(|source| {
                DatabaseError::io(format!("cannot write {}", temporary.display()), source)
            })?;
            fs::rename(&temporary, &path).map_err(|source| {
                DatabaseError::io(format!("cannot rename {}", temporary.display()), source)
            })?;
        }
        Ok(SavedAttachment {
            sha256,
            size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        })
    }

    /// The path of the part with `sha256`, or `None` when `sha256` isn't 64 lowercase hex
    /// digits, so a stored value can't name a path outside the directory.
    #[must_use]
    pub fn path(&self, sha256: &str) -> Option<PathBuf> {
        let valid = sha256.len() == 64
            && sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        valid.then(|| self.directory.join(sha256))
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    #[test]
    fn saves_once_by_content() {
        let dir = TestDir::new();
        let store = AttachmentStore::new(dir.path().join("attachments"));
        let saved = store.save(b"abc", Timestamp::from_millis(1)).unwrap();
        assert_eq!(
            saved.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(saved.size, 3);
        let path = store.path(&saved.sha256).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        fs::write(&path, b"kept").unwrap();
        assert_eq!(
            store.save(b"abc", Timestamp::from_millis(2)).unwrap(),
            saved
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            b"kept",
            "an existing file isn't rewritten"
        );
        let names: Vec<String> = fs::read_dir(dir.path().join("attachments"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [saved.sha256.as_str()], "no temporary file is left");
    }

    #[test]
    fn paths_need_a_sha256() {
        let store = AttachmentStore::new(PathBuf::from("/data/attachments"));
        assert_eq!(store.path("../spareline.db"), None);
        assert_eq!(store.path(&"A".repeat(64)), None);
        assert_eq!(
            store.path(&"a".repeat(64)),
            Some(PathBuf::from(format!(
                "/data/attachments/{}",
                "a".repeat(64)
            )))
        );
    }
}
