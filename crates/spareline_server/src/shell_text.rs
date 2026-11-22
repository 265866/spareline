//! Shell output as text, decoded as spareline-old decoded it.

/// `bytes` as text, as JavaScript's `TextDecoder` read every shell output: invalid UTF-8
/// becomes U+FFFD, and a leading byte order mark is dropped.
pub(crate) fn shell_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_a_leading_byte_order_mark_only() {
        assert_eq!(shell_text(b"\xef\xbb\xbfRow: 0\n"), "Row: 0\n");
        assert_eq!(shell_text("a\u{feff}b".as_bytes()), "a\u{feff}b");
    }

    #[test]
    fn replaces_invalid_utf8() {
        assert_eq!(shell_text(b"ok \xe9"), "ok \u{fffd}");
    }
}
