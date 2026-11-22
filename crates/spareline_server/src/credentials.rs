//! What a request presents to sign in, and the session cookie the server sets.

use axum::http::{HeaderMap, Method, header};

/// The session cookie's name.
pub const SESSION_COOKIE: &str = "spareline_session";

/// How long a session lasts after its last use, in seconds: 30 days.
pub const SESSION_MAX_AGE: u64 = 30 * 86_400;

/// Paths anyone may request. Everything else under `/api/` needs a session or an API key; the
/// web app's pages and assets are public, so the sign-in page can load.
const PUBLIC_API: [&str; 4] = [
    "/api/docs",
    "/api/docs/json",
    "/api/v1/auth/login",
    "/api/v1/auth/me",
];

/// Paths only a signed-in browser may use: a leaked key must not mint more keys or change a
/// password.
const SESSION_ONLY: [&str; 2] = ["/api/v1/auth/", "/api/v1/api-keys"];

/// What a request presents. A `Bearer` header wins over the cookie, even when the key turns out
/// invalid, as in spareline-old.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credentials {
    /// An API key from `Authorization: Bearer <key>`.
    Key(String),
    /// A session token from the `spareline_session` cookie.
    Session(String),
    /// Neither.
    None,
}

impl Credentials {
    /// Reads the credentials from the request's headers as spareline-old's guard did: every
    /// `Authorization` header joined with `, ` and read byte for byte, so a key with non-ASCII
    /// text is still a key, an invalid one, and the cookie is ignored; anything that isn't
    /// `Bearer <token>` falls back to the cookie.
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let joined: Vec<String> = headers
            .get_all(header::AUTHORIZATION)
            .iter()
            .map(|value| value.as_bytes().iter().copied().map(char::from).collect())
            .collect();
        if !joined.is_empty()
            && let Some(key) = bearer(&joined.join(", "))
        {
            return Self::Key(key.to_owned());
        }
        match session_token(headers) {
            Some(token) if !token.is_empty() => Self::Session(token),
            Some(_) | None => Self::None,
        }
    }
}

/// The token of `Bearer <token>`, matched as spareline-old did: `Bearer` in any case, at least
/// one blank, then a token without blanks and nothing after it.
fn bearer(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_at_checked(6)?;
    if !scheme.eq_ignore_ascii_case("bearer") || !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let token = rest.trim_start();
    (!token.is_empty() && !token.contains(char::is_whitespace)).then_some(token)
}

/// The value of the first `spareline_session` cookie, from every `Cookie` header. Headers are
/// split as bytes, so another cookie holding non-ASCII text doesn't hide the session.
fn session_token(headers: &HeaderMap) -> Option<String> {
    for cookie in headers.get_all(header::COOKIE) {
        for part in cookie.as_bytes().split(|&byte| byte == b';') {
            let Some(equals) = part.iter().position(|&byte| byte == b'=') else {
                continue;
            };
            let (name, value) = (&part[..equals], &part[equals + 1..]);
            if name.trim_ascii() == SESSION_COOKIE.as_bytes() {
                return Some(String::from_utf8_lossy(value.trim_ascii()).into_owned());
            }
        }
    }
    None
}

/// How a request reached the server, which decides whether the cookie may carry `Secure`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Plain HTTP, such as an SSH tunnel to the loopback port.
    Http,
    /// HTTPS, ended by a proxy that says so in `X-Forwarded-Proto`.
    Https,
}

impl Transport {
    /// How the request arrived: HTTPS when a proxy says so in `X-Forwarded-Proto`.
    #[must_use]
    pub fn of(headers: &HeaderMap) -> Self {
        let https = headers
            .get("x-forwarded-proto")
            .is_some_and(|value| value.as_bytes() == b"https");
        if https { Self::Https } else { Self::Http }
    }
}

/// The `Set-Cookie` value that stores `token` for `max_age` seconds; an empty token and 0 sign
/// the browser out.
#[must_use]
pub fn session_cookie(token: &str, max_age: u64, transport: Transport) -> String {
    let mut cookie =
        format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}");
    if transport == Transport::Https {
        cookie.push_str("; Secure");
    }
    cookie
}

/// Whether `path` is open to requests without credentials.
#[must_use]
pub fn is_public(path: &str) -> bool {
    PUBLIC_API.contains(&path)
}

/// Whether `path` is open only to a signed-in browser, never to an API key.
#[must_use]
pub fn is_session_only(path: &str) -> bool {
    SESSION_ONLY.iter().any(|prefix| path.starts_with(prefix))
}

/// Whether a read-only key may make the request: a `GET` or `HEAD` that isn't a WebSocket
/// upgrade, since a screen's socket sends input.
#[must_use]
pub fn is_read_only(method: &Method, headers: &HeaderMap) -> bool {
    let reads = *method == Method::GET || *method == Method::HEAD;
    let upgrades = headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    reads && !upgrades
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[[&'static str; 2]]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for [name, value] in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn a_bearer_key_wins_over_the_cookie() {
        let both = headers(&[
            ["authorization", "bearer   sl_abc"],
            ["cookie", "spareline_session=tok"],
        ]);
        assert_eq!(
            Credentials::from_headers(&both),
            Credentials::Key("sl_abc".to_owned())
        );
    }

    #[test]
    fn a_malformed_authorization_falls_back_to_the_cookie() {
        for value in ["Basic xyz", "Bearer", "Bearer a b", "Bearersl_abc"] {
            let map = headers(&[
                ["authorization", value],
                ["cookie", "spareline_session=tok"],
            ]);
            assert_eq!(
                Credentials::from_headers(&map),
                Credentials::Session("tok".to_owned()),
                "{value}"
            );
        }
    }

    #[test]
    fn a_non_ascii_cookie_does_not_hide_the_session() {
        let mut map = HeaderMap::new();
        map.append(
            header::COOKIE,
            HeaderValue::from_bytes(b"other=\xe2\x9c\x93; spareline_session=tok").unwrap(),
        );
        assert_eq!(
            Credentials::from_headers(&map),
            Credentials::Session("tok".to_owned())
        );
    }

    #[test]
    fn a_non_ascii_bearer_is_still_a_key_and_ignores_the_cookie() {
        let mut map = headers(&[["cookie", "spareline_session=tok"]]);
        map.append(
            header::AUTHORIZATION,
            HeaderValue::from_bytes(b"Bearer sl_\xc3\xa9").unwrap(),
        );
        assert_eq!(
            Credentials::from_headers(&map),
            Credentials::Key("sl_\u{c3}\u{a9}".to_owned())
        );
    }

    #[test]
    fn two_authorization_headers_read_as_one_and_fall_back_to_the_cookie() {
        let twice = headers(&[
            ["authorization", "Bearer a"],
            ["authorization", "Bearer b"],
            ["cookie", "spareline_session=tok"],
        ]);
        assert_eq!(
            Credentials::from_headers(&twice),
            Credentials::Session("tok".to_owned())
        );
    }

    #[test]
    fn reads_the_first_session_cookie_among_others() {
        let map = headers(&[[
            "cookie",
            "theme=dark; spareline_session = tok1 ; spareline_session=tok2",
        ]]);
        assert_eq!(
            Credentials::from_headers(&map),
            Credentials::Session("tok1".to_owned())
        );
        assert_eq!(
            Credentials::from_headers(&HeaderMap::new()),
            Credentials::None
        );
        let empty = headers(&[["cookie", "spareline_session="]]);
        assert_eq!(Credentials::from_headers(&empty), Credentials::None);
    }

    #[test]
    fn writes_the_cookie_spareline_old_wrote() {
        assert_eq!(
            session_cookie("tok", SESSION_MAX_AGE, Transport::Http),
            "spareline_session=tok; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000"
        );
        assert_eq!(
            session_cookie("", 0, Transport::Https),
            "spareline_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0; Secure"
        );
        assert_eq!(
            Transport::of(&headers(&[["x-forwarded-proto", "https"]])),
            Transport::Https
        );
        assert_eq!(
            Transport::of(&headers(&[["x-forwarded-proto", "http"]])),
            Transport::Http
        );
    }

    #[test]
    fn sorts_paths_as_the_guard_needs() {
        assert!(is_public("/api/v1/auth/login"));
        assert!(!is_public("/api/v1/auth/login/"));
        assert!(is_session_only("/api/v1/auth/me"));
        assert!(is_session_only("/api/v1/api-keys/3"));
        assert!(!is_session_only("/api/v1/phones"));
    }

    #[test]
    fn a_read_key_may_only_read_without_a_socket() {
        let none = HeaderMap::new();
        assert!(is_read_only(&Method::GET, &none));
        assert!(is_read_only(&Method::HEAD, &none));
        assert!(!is_read_only(&Method::POST, &none));
        let upgrade = headers(&[["upgrade", "WebSocket"]]);
        assert!(!is_read_only(&Method::GET, &upgrade));
    }
}
