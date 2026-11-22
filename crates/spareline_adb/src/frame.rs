//! The adb server's wire formats: smart-socket requests and statuses, `shell,v2` packets and
//! `sync:` records.

use std::{io, str};

use tokio::io::AsyncReadExt as _;
use tokio::net::TcpStream;

use crate::adb_error::{AdbError, AdbErrorKind};

/// No adb frame comes close to this; a longer length means corrupt framing, and reading it would
/// let a broken peer grow memory without bound.
pub(crate) const MAX_FRAME: usize = 4 * 1024 * 1024;

/// The largest `DATA` record `sync:` accepts.
pub(crate) const SYNC_DATA_MAX: usize = 64 * 1024;

pub(crate) const SHELL_STDOUT: u8 = 1;
pub(crate) const SHELL_STDERR: u8 = 2;
pub(crate) const SHELL_EXIT: u8 = 3;

/// A smart-socket request: four lowercase hex digits of the payload's length, then the payload.
pub(crate) fn encode_request(service: &str) -> Result<Vec<u8>, AdbError> {
    if service.contains('\0') {
        return Err(AdbError::new(
            AdbErrorKind::Protocol,
            "adb requests cannot contain NUL bytes".to_owned(),
        ));
    }
    let length = service.len();
    if length > 0xff_ff {
        return Err(AdbError::new(
            AdbErrorKind::Protocol,
            format!("adb request is {length} bytes, over the 65535 limit"),
        ));
    }
    let mut out = format!("{length:04x}").into_bytes();
    out.extend_from_slice(service.as_bytes());
    Ok(out)
}

/// Reads four hex digits as a length.
pub(crate) fn parse_hex_length(bytes: &[u8]) -> Result<usize, AdbError> {
    let invalid = || {
        AdbError::new(
            AdbErrorKind::Protocol,
            format!(
                "expected a 4-digit hex length, got {:?}",
                String::from_utf8_lossy(bytes)
            ),
        )
    };
    if bytes.len() != 4 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(invalid());
    }
    let text = str::from_utf8(bytes).map_err(|_| invalid())?;
    usize::from_str_radix(text, 16).map_err(|_| invalid())
}

/// Exactly `length` bytes, or a `Closed` error that says how many arrived.
pub(crate) async fn read_exact(stream: &mut TcpStream, length: usize) -> Result<Vec<u8>, AdbError> {
    if length > MAX_FRAME {
        return Err(AdbError::new(
            AdbErrorKind::Protocol,
            format!("refusing to read {length} bytes in one frame (limit {MAX_FRAME})"),
        ));
    }
    let mut buffer = vec![0; length];
    let mut filled = 0;
    while filled < length {
        let read = stream
            .read(&mut buffer[filled..])
            .await
            .map_err(|error| closed(&error))?;
        if read == 0 {
            return Err(AdbError::new(
                AdbErrorKind::Closed,
                format!("connection closed after {filled} of {length} expected bytes"),
            ));
        }
        filled += read;
    }
    Ok(buffer)
}

/// Exactly `length` bytes, or `None` when the peer closed cleanly before any of them arrived.
pub(crate) async fn read_exact_or_end(
    stream: &mut TcpStream,
    length: usize,
) -> Result<Option<Vec<u8>>, AdbError> {
    let mut first = [0; 1];
    let read = stream
        .read(&mut first)
        .await
        .map_err(|error| closed(&error))?;
    if read == 0 {
        return Ok(None);
    }
    let mut out = vec![first[0]];
    out.extend(read_exact(stream, length - 1).await?);
    Ok(Some(out))
}

/// Everything the peer sends until it closes.
pub(crate) async fn read_to_end(stream: &mut TcpStream) -> Result<Vec<u8>, AdbError> {
    let mut out = Vec::new();
    stream
        .read_to_end(&mut out)
        .await
        .map_err(|error| closed(&error))?;
    Ok(out)
}

/// One length-prefixed string, as the server sends after `FAIL`, for `host:version` and for each
/// device-list update.
pub(crate) async fn read_hex_string(stream: &mut TcpStream) -> Result<String, AdbError> {
    let length = parse_hex_length(&read_exact(stream, 4).await?)?;
    let payload = read_exact(stream, length).await?;
    Ok(String::from_utf8_lossy(&payload).into_owned())
}

/// Reads `OKAY`, or returns the server's `FAIL` message as an error that names `what`.
pub(crate) async fn read_status(stream: &mut TcpStream, what: &str) -> Result<(), AdbError> {
    let status = read_exact(stream, 4).await?;
    match status.as_slice() {
        b"OKAY" => Ok(()),
        b"FAIL" => {
            let message = read_hex_string(stream)
                .await
                .unwrap_or_else(|_| "(no message)".to_owned());
            Err(AdbError::new(
                AdbErrorKind::Fail,
                format!("{what}: {message}"),
            ))
        }
        other => Err(AdbError::new(
            AdbErrorKind::Protocol,
            format!(
                "{what}: expected OKAY or FAIL, got {:?}",
                String::from_utf8_lossy(other)
            ),
        )),
    }
}

/// Sends `service` and reads its status, naming the service in an error.
pub(crate) async fn request(stream: &mut TcpStream, service: &str) -> Result<(), AdbError> {
    write_all(stream, &encode_request(service)?).await?;
    read_status(stream, service).await
}

/// Writes all of `data`.
pub(crate) async fn write_all(stream: &mut TcpStream, data: &[u8]) -> Result<(), AdbError> {
    use tokio::io::AsyncWriteExt as _;

    stream.write_all(data).await.map_err(|error| {
        AdbError::new(
            AdbErrorKind::Closed,
            format!("connection closed while writing: {error}"),
        )
    })
}

/// A `sync:` record: a four-letter id, then a little-endian 32-bit length or value, then the
/// payload, if any.
pub(crate) fn encode_sync(id: [u8; 4], value: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&id);
    out.extend_from_slice(&value.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// A `sync:` record with a payload, whose length is its value.
pub(crate) fn encode_sync_data(id: [u8; 4], payload: &[u8]) -> Result<Vec<u8>, AdbError> {
    let length = u32::try_from(payload.len()).map_err(|_| {
        AdbError::new(
            AdbErrorKind::Protocol,
            format!("sync record of {} bytes is too long", payload.len()),
        )
    })?;
    Ok(encode_sync(id, length, payload))
}

/// Reads a `sync:` status: `OKAY`, or `FAIL` with a message, which becomes an error naming `what`.
pub(crate) async fn read_sync_status(stream: &mut TcpStream, what: &str) -> Result<(), AdbError> {
    let header = read_exact(stream, 8).await?;
    let value = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    match &header[..4] {
        b"OKAY" => Ok(()),
        b"FAIL" => {
            let length = usize::try_from(value).unwrap_or(usize::MAX);
            let message = read_exact(stream, length).await?;
            Err(AdbError::new(
                AdbErrorKind::Fail,
                format!("{what}: {}", String::from_utf8_lossy(&message)),
            ))
        }
        other => Err(AdbError::new(
            AdbErrorKind::Protocol,
            format!(
                "{what}: expected OKAY or FAIL, got {:?}",
                String::from_utf8_lossy(other)
            ),
        )),
    }
}

fn closed(error: &io::Error) -> AdbError {
    AdbError::new(AdbErrorKind::Closed, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_its_hex_length_then_the_payload() {
        assert_eq!(encode_request("host:version").unwrap(), b"000chost:version");
    }

    #[test]
    fn a_request_with_a_nul_is_refused() {
        let error = encode_request("shell:echo\0rm").unwrap_err();
        assert_eq!(error.kind(), AdbErrorKind::Protocol);
    }

    #[test]
    fn a_request_over_the_limit_is_refused() {
        let error = encode_request(&"x".repeat(0x1_00_00)).unwrap_err();
        assert_eq!(
            error.message(),
            "adb request is 65536 bytes, over the 65535 limit"
        );
    }

    #[test]
    fn hex_lengths_take_four_hex_digits() {
        assert_eq!(parse_hex_length(b"00Ff").unwrap(), 255);
        assert_eq!(
            parse_hex_length(b"0x1f").unwrap_err().message(),
            "expected a 4-digit hex length, got \"0x1f\""
        );
    }

    #[test]
    fn sync_records_carry_a_little_endian_value() {
        assert_eq!(
            encode_sync(*b"DONE", 0x01_02_03_04, &[]),
            b"DONE\x04\x03\x02\x01"
        );
        assert_eq!(
            encode_sync_data(*b"DATA", b"ab").unwrap(),
            b"DATA\x02\0\0\0ab"
        );
    }
}
