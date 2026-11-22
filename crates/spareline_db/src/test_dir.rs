//! Temporary directories for tests, removed when dropped.

use std::path::{Path, PathBuf};
use std::{env, fs, io};

/// A new, empty directory under the system's temporary directory, removed with its contents
/// when dropped.
#[derive(Debug)]
pub(crate) struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub(crate) fn new() -> Self {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).expect("the OS random source works in tests");
        let name = u64::from_le_bytes(bytes);
        let path = env::temp_dir().join(format!("spareline-db-test-{name:016x}"));
        fs::create_dir(&path).expect("a new temporary directory can be created");
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            assert_eq!(
                error.kind(),
                io::ErrorKind::NotFound,
                "cannot remove {}: {error}",
                self.path.display()
            );
        }
    }
}
