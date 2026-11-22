//! The container health check: asks the running service for `/healthz` over loopback.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;
use std::{io, str};

use thiserror::Error;

/// How long the check may take. Podman's `HealthTimeout` in the quadlet is 5 s.
const TIMEOUT: Duration = Duration::from_secs(4);

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
    /// The answer wasn't HTTP.
    #[error("{address} answered something other than HTTP")]
    NotHttp {
        /// The address asked.
        address: SocketAddr,
    },
    /// The service answered with a status outside 2xx.
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
}

impl Healthcheck {
    /// The check of the service on `127.0.0.1:port`.
    #[must_use]
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    /// Asks `GET /healthz` and passes on any 2xx, whatever the body says.
    ///
    /// # Errors
    ///
    /// Returns why the service isn't healthy.
    pub fn run(self) -> Result<(), HealthcheckError> {
        check(self.port)
    }
}

/// Asks `GET /healthz` on `127.0.0.1:port` and passes on any 2xx, whatever the body says, so a
/// stopped adb server, which makes `/healthz` answer `{"ok":false}`, doesn't roll back an update.
///
/// # Errors
///
/// Returns why the service isn't healthy.
fn check(port: u16) -> Result<(), HealthcheckError> {
    use std::io::{Read as _, Write as _};

    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let unreachable = |source| HealthcheckError::Unreachable { address, source };
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT).map_err(unreachable)?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(unreachable)?;
    stream
        .set_write_timeout(Some(TIMEOUT))
        .map_err(unreachable)?;
    let request = format!(
        "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: spareline-healthcheck\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).map_err(unreachable)?;
    let mut head = [0; 12];
    stream.read_exact(&mut head).map_err(unreachable)?;
    let status = parse_status(&head).ok_or(HealthcheckError::NotHttp { address })?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(HealthcheckError::Status { status })
    }
}

/// The status code of an HTTP/1.x status line's first 12 bytes, such as `HTTP/1.1 200`.
fn parse_status(head: &[u8; 12]) -> Option<u16> {
    let text = str::from_utf8(head).ok()?;
    let rest = text.strip_prefix("HTTP/1.")?;
    let (_, code) = rest.split_once(' ')?;
    if code.len() != 3 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    code.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    fn serve_once(answer: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let read = stream.read(&mut request).unwrap();
            assert!(
                request[..read].starts_with(b"GET /healthz HTTP/1.1\r\n"),
                "the check asks for /healthz"
            );
            stream.write_all(answer.as_bytes()).unwrap();
        });
        port
    }

    #[test]
    fn passes_on_any_2xx_whatever_the_body() {
        let port = serve_once("HTTP/1.1 200 OK\r\ncontent-length: 12\r\n\r\n{\"ok\":false}");
        Healthcheck::new(port).run().unwrap();
        let port = serve_once("HTTP/1.1 204 No Content\r\n\r\n");
        Healthcheck::new(port).run().unwrap();
    }

    #[test]
    fn fails_on_another_status() {
        let port = serve_once("HTTP/1.1 503 Service Unavailable\r\n\r\n");
        assert_eq!(
            Healthcheck::new(port).run().unwrap_err().to_string(),
            "/healthz answered 503"
        );
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
    fn fails_on_what_is_not_http() {
        let port = serve_once("SSH-2.0-OpenSSH_9.9\r\n");
        assert!(matches!(
            Healthcheck::new(port).run(),
            Err(HealthcheckError::NotHttp { .. })
        ));
    }
}
