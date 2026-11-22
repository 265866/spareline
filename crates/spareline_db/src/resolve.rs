//! Absolute paths, as Node's `path.resolve` writes them.

use std::path::{Component, Path, PathBuf};
use std::{env, io};

/// `path` made absolute against the current directory, with `.` and `..` resolved by their
/// text alone, as Node's `path.resolve` does. Symbolic links are left as they are, and a
/// trailing separator is dropped.
///
/// # Errors
///
/// Fails when `path` is relative and the current directory can't be read.
pub(crate) fn resolve(path: &Path) -> io::Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                resolved.push(component);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_dots_by_text() {
        assert_eq!(
            resolve(Path::new("/var/./tmp/../data/spareline.db/")).unwrap(),
            PathBuf::from("/var/data/spareline.db")
        );
        assert_eq!(resolve(Path::new("/..")).unwrap(), PathBuf::from("/"));
        let current = env::current_dir().unwrap();
        assert_eq!(resolve(Path::new("a/../b")).unwrap(), current.join("b"));
    }
}
