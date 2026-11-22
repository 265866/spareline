//! File system error messages in the form Node.js gives them, which spareline-old's backup
//! command printed.

use std::io;
use std::path::Path;

/// The message Node.js gives for a failed file system call: `ENOENT: no such file or directory,
/// stat '/app/data/spareline.db'`. An error Node.js would name differently, or one without an
/// operating system code, keeps Rust's description.
pub(crate) fn node_message(error: &io::Error, syscall: &str, path: &Path) -> String {
    let path = path.display();
    match error.raw_os_error().and_then(errno_text) {
        Some(Errno { code, text }) => format!("{code}: {text}, {syscall} '{path}'"),
        None => format!("{error}, {syscall} '{path}'"),
    }
}

/// The message Node.js gives for a failed `link(source, destination)`.
pub(crate) fn node_link_message(error: &io::Error, source: &Path, destination: &Path) -> String {
    let source = source.display();
    let destination = destination.display();
    match error.raw_os_error().and_then(errno_text) {
        Some(Errno { code, text }) => {
            format!("{code}: {text}, link '{source}' -> '{destination}'")
        }
        None => format!("{error}, link '{source}' -> '{destination}'"),
    }
}

/// An error code's name and libuv's description of it.
struct Errno {
    code: &'static str,
    text: &'static str,
}

/// The name and libuv description of the Linux error numbers a backup can meet. The numbers are
/// the same on x86-64 and ARM64.
fn errno_text(number: i32) -> Option<Errno> {
    let (code, text) = match number {
        1 => ("EPERM", "operation not permitted"),
        2 => ("ENOENT", "no such file or directory"),
        5 => ("EIO", "i/o error"),
        13 => ("EACCES", "permission denied"),
        17 => ("EEXIST", "file already exists"),
        18 => ("EXDEV", "cross-device link not permitted"),
        20 => ("ENOTDIR", "not a directory"),
        21 => ("EISDIR", "illegal operation on a directory"),
        28 => ("ENOSPC", "no space left on device"),
        30 => ("EROFS", "read-only file system"),
        36 => ("ENAMETOOLONG", "name too long"),
        40 => ("ELOOP", "too many symbolic links encountered"),
        _ => return None,
    };
    Some(Errno { code, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_what_node_writes() {
        let error = io::Error::from_raw_os_error(2);
        assert_eq!(
            node_message(&error, "stat", Path::new("/data/spareline.db")),
            "ENOENT: no such file or directory, stat '/data/spareline.db'"
        );
        let exists = io::Error::from_raw_os_error(17);
        assert_eq!(
            node_link_message(&exists, Path::new("/b/.x-1/snapshot.db"), Path::new("/b/x")),
            "EEXIST: file already exists, link '/b/.x-1/snapshot.db' -> '/b/x'"
        );
    }
}
