//! The container health check: asks the running service for `/healthz` over loopback.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use thiserror::Error;

/// How long the whole check may take. Podman's `HealthTimeout` in the quadlet is 5 s, so the
/// check gives its own answer before podman kills it.
const TIMEOUT: Duration = Duration::from_secs(4);

/// The most of the answer the check reads while looking for the final status line.
const MAX_HEAD: usize = 16 * 1024;

/// Why the service isn't healthy. It shows after `unhealthy: `.
#[derive(Debug, Error)]
#[must_use]
pub enum HealthcheckError {
    /// Nothing answered, or the connection broke.
    #[error("cannot ask {address}: {source}")]
    Unreachable {
        /// The address asked.
        address: SocketAddr,
        /// The failure.
        source: io::Error,
    },
    /// The service didn't answer within the check's time.
    #[error("{address} gave no answer within {limit:?}")]
    Timeout {
        /// The address asked.
        address: SocketAddr,
        /// How long the check waited.
        limit: Duration,
    },
    /// The answer wasn't HTTP, or ended before its status line.
    #[error("{address} answered something other than HTTP")]
    NotHttp {
        /// The address asked.
        address: SocketAddr,
    },
    /// The service answered with a final status outside 2xx.
    #[error("/healthz answered {status}")]
    Status {
        /// The status code.
        status: u16,
    },
}

/// The container health check of the service on one port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Healthcheck {
    port: u16,
    timeout: Duration,
}

impl Healthcheck {
    /// The check of the service on `127.0.0.1:port`.
    #[must_use]
    pub fn new(port: u16) -> Self {
        Self {
            port,
            timeout: TIMEOUT,
        }
    }

    /// Asks `GET /healthz` within 4 s and passes on any final 2xx, whatever the body says, so a
    /// stopped adb server, which makes `/healthz` answer `{"ok":false}`, doesn't roll back an
    /// update. An interim 1xx answer is skipped; a redirect fails, since the service sends none.
    ///
    /// # Errors
    ///
    /// Returns why the service isn't healthy.
    pub fn run(self) -> Result<(), HealthcheckError> {
        use std::io::{Read as _, Write as _};

        let limit = self.timeout;
        let deadline = Instant::now() + limit;
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let failed = |source: io::Error| {
            let kind = source.kind();
            if kind == io::ErrorKind::WouldBlock || kind == io::ErrorKind::TimedOut {
                HealthcheckError::Timeout { address, limit }
            } else {
                HealthcheckError::Unreachable { address, source }
            }
        };
        let left = || {
            deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .ok_or(HealthcheckError::Timeout { address, limit })
        };
        let mut stream = TcpStream::connect_timeout(&address, left()?).map_err(failed)?;
        stream.set_write_timeout(Some(left()?)).map_err(failed)?;
        let request = format!(
            "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nUser-Agent: spareline-healthcheck\r\n\
             Connection: close\r\n\r\n",
            self.port
        );
        stream.write_all(request.as_bytes()).map_err(failed)?;
        let mut head = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            match final_status(&head) {
                Head::Final(status) if (200..300).contains(&status) => return Ok(()),
                Head::Final(status) => return Err(HealthcheckError::Status { status }),
                Head::NotHttp => return Err(HealthcheckError::NotHttp { address }),
                Head::Incomplete if head.len() >= MAX_HEAD => {
                    return Err(HealthcheckError::NotHttp { address });
                }
                Head::Incomplete => {}
            }
            stream.set_read_timeout(Some(left()?)).map_err(failed)?;
            let read = stream.read(&mut chunk).map_err(failed)?;
            if read == 0 {
                return Err(HealthcheckError::NotHttp { address });
            }
            head.extend_from_slice(&chunk[..read]);
        }
    }
}

/// What the bytes of an answer read so far say about its final status.
#[derive(Debug, PartialEq, Eq)]
enum Head {
    /// The final status line arrived, with this code.
    Final(u16),
    /// More bytes are needed.
    Incomplete,
    /// The bytes aren't an HTTP/1.x answer.
    NotHttp,
}

/// Finds the final status of an HTTP/1.x answer, skipping interim 1xx answers with their headers.
fn final_status(bytes: &[u8]) -> Head {
    let mut rest = bytes;
    loop {
        let Some(end) = rest.windows(2).position(|pair| pair == b"\r\n") else {
            return if could_start_http(rest) {
                Head::Incomplete
            } else {
                Head::NotHttp
            };
        };
        let Some(status) = status_code(&rest[..end]) else {
            return Head::NotHttp;
        };
        if !(100..200).contains(&status) || status == 101 {
            return Head::Final(status);
        }
        let Some(block_end) = rest.windows(4).position(|window| window == b"\r\n\r\n") else {
            return Head::Incomplete;
        };
        rest = &rest[block_end + 4..];
    }
}

/// Whether `bytes`, the start of a line, could still become an HTTP/1.x status line.
fn could_start_http(bytes: &[u8]) -> bool {
    let prefix = b"HTTP/1.";
    let shared = bytes.len().min(prefix.len());
    bytes[..shared] == prefix[..shared]
}

/// The code of a status line such as `HTTP/1.1 200 OK`: `HTTP/1.`, a digit, a space, three
/// digits, then the end of the line or a space.
fn status_code(line: &[u8]) -> Option<u16> {
    let rest = line.strip_prefix(b"HTTP/1.")?;
    let [version, b' ', a, b, c, tail @ ..] = rest else {
        return None;
    };
    let digits = [*version, *a, *b, *c];
    if !digits.iter().all(u8::is_ascii_digit) || !(tail.is_empty() || tail[0] == b' ') {
        return None;
    }
    Some(u16::from(a - b'0') * 100 + u16::from(b - b'0') * 10 + u16::from(c - b'0'))
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;

    use super::*;

    /// A stand-in server's port, and the request it read.
    struct Served {
        port: u16,
        request: Receiver<Vec<u8>>,
    }

    /// A server that answers one connection with `answer`, sent in `parts` pieces with `pause`
    /// between them, and reports the request it read.
    fn serve(answer: &'static [u8], parts: usize, pause: Duration) -> Served {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen, request) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 1024];
            let read = stream.read(&mut buffer).unwrap();
            seen.send(buffer[..read].to_vec()).unwrap();
            let size = answer.len().div_ceil(parts.max(1)).max(1);
            for piece in answer.chunks(size) {
                if stream.write_all(piece).is_err() {
                    return;
                }
                thread::sleep(pause);
            }
        });
        Served { port, request }
    }

    fn check(answer: &'static [u8]) -> Result<(), HealthcheckError> {
        let Served { port, request } = serve(answer, 1, Duration::ZERO);
        let result = Healthcheck::new(port).run();
        let sent = request.recv().unwrap();
        assert!(
            sent.starts_with(b"GET /healthz HTTP/1.1\r\n"),
            "the check asks for /healthz"
        );
        result
    }

    #[test]
    fn passes_on_any_2xx_whatever_the_body() {
        check(b"HTTP/1.1 200 OK\r\ncontent-length: 12\r\n\r\n{\"ok\":false}").unwrap();
        check(b"HTTP/1.0 204 No Content\r\n\r\n").unwrap();
        check(b"HTTP/1.1 200\r\n\r\n").unwrap();
    }

    #[test]
    fn skips_an_interim_answer() {
        check(b"HTTP/1.1 103 Early Hints\r\nlink: </a>\r\n\r\nHTTP/1.1 200 OK\r\n\r\n").unwrap();
    }

    #[test]
    fn fails_on_another_final_status() {
        let error = check(b"HTTP/1.1 503 Service Unavailable\r\n\r\n").unwrap_err();
        assert_eq!(error.to_string(), "/healthz answered 503");
        let error = check(b"HTTP/1.1 301 Moved Permanently\r\nlocation: /\r\n\r\n").unwrap_err();
        assert_eq!(error.to_string(), "/healthz answered 301");
    }

    #[test]
    fn a_status_with_four_digits_is_not_http() {
        assert!(matches!(
            check(b"HTTP/1.1 2000 OK\r\n\r\n"),
            Err(HealthcheckError::NotHttp { .. })
        ));
    }

    #[test]
    fn a_short_or_strange_answer_is_not_http() {
        for answer in [&b"HTTP/1.0 20"[..], b"ok", b"SSH-2.0-OpenSSH_9.9\r\n"] {
            let leaked: &'static [u8] = Box::leak(answer.to_vec().into_boxed_slice());
            assert!(
                matches!(check(leaked), Err(HealthcheckError::NotHttp { .. })),
                "{answer:?}"
            );
        }
    }

    #[test]
    fn fails_when_nothing_listens() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let message = Healthcheck::new(port).run().unwrap_err().to_string();
        assert!(
            message.starts_with(&format!("cannot ask 127.0.0.1:{port}: ")),
            "{message}"
        );
    }

    #[test]
    fn a_trickling_answer_still_ends_within_the_deadline() {
        let Served {
            port,
            request: _request,
        } = serve(b"HTTP/1.1 200 OK\r\n\r\n", 19, Duration::from_millis(60));
        let check = Healthcheck {
            port,
            timeout: Duration::from_millis(300),
        };
        let error = check.run().unwrap_err();
        assert!(matches!(error, HealthcheckError::Timeout { .. }), "{error}");
        assert!(
            error.to_string().ends_with("gave no answer within 300ms"),
            "{error}"
        );
    }

    #[test]
    fn reads_status_lines_strictly() {
        assert_eq!(status_code(b"HTTP/1.1 200 OK"), Some(200));
        assert_eq!(status_code(b"HTTP/1.1 200"), Some(200));
        assert_eq!(status_code(b"HTTP/1.1 2000"), None);
        assert_eq!(status_code(b"HTTP/2 200"), None);
        assert_eq!(final_status(b"HTT"), Head::Incomplete);
        assert_eq!(final_status(b"HTTP/1.1 100 Continue\r\n"), Head::Incomplete);
    }
}
