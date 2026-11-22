//! The adb server's address.

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use thiserror::Error;

/// The host and port of an adb server, such as `127.0.0.1:5037`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    host: String,
    port: u16,
}

/// An adb server address that isn't `host:port`.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("expected host:port for the adb server, got \"{address}\"")]
#[must_use]
pub struct EndpointError {
    address: String,
}

impl Endpoint {
    /// The endpoint at `host` and `port`.
    #[must_use]
    pub fn new(host: String, port: u16) -> Self {
        Self { host, port }
    }

    /// The host name or address, without brackets around an IPv6 address.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The TCP port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl FromStr for Endpoint {
    type Err = EndpointError;

    /// Reads `host:port`, or `[address]:port` for IPv6. The port is 1 to 65535.
    fn from_str(address: &str) -> Result<Self, Self::Err> {
        let error = || EndpointError {
            address: address.to_owned(),
        };
        let (host, port) = address.rsplit_once(':').ok_or_else(error)?;
        let host = host.strip_prefix('[').unwrap_or(host);
        let host = host.strip_suffix(']').unwrap_or(host);
        if host.is_empty() || host.contains(']') || port.is_empty() {
            return Err(error());
        }
        if !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(error());
        }
        let port: u16 = port.parse().map_err(|_| error())?;
        if port == 0 {
            return Err(error());
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }
}

impl Display for Endpoint {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_host_and_port() {
        let endpoint: Endpoint = "127.0.0.1:5037".parse().unwrap();
        assert_eq!(endpoint, Endpoint::new("127.0.0.1".to_owned(), 5037));
        assert_eq!(endpoint.to_string(), "127.0.0.1:5037");
    }

    #[test]
    fn strips_the_brackets_of_an_ipv6_address() {
        let endpoint: Endpoint = "[::1]:5037".parse().unwrap();
        assert_eq!(endpoint.host(), "::1");
        assert_eq!(endpoint.port(), 5037);
    }

    #[test]
    fn rejects_what_is_not_host_and_port() {
        for address in [
            "",
            "localhost",
            ":5037",
            "host:",
            "host:0",
            "host:65536",
            "host:+1",
            "h:1x",
        ] {
            let error = address.parse::<Endpoint>().unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("expected host:port for the adb server, got \"{address}\"")
            );
        }
    }
}
