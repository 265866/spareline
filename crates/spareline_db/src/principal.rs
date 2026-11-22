//! Who a request acts for.

use crate::api_key::KeyScope;
use crate::user::User;

/// Who a request acts for: a signed-in browser or an API key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    /// A browser session.
    Session(SessionPrincipal),
    /// An API key.
    Key(KeyPrincipal),
}

impl Principal {
    /// The user the request acts for.
    #[must_use]
    pub fn user(&self) -> &User {
        match self {
            Self::Session(session) => &session.user,
            Self::Key(key) => &key.user,
        }
    }
}

/// A request signed in with a session cookie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPrincipal {
    /// The session's user.
    pub user: User,
    /// The `sessions` row id.
    pub session_id: i64,
}

/// A request made with an API key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPrincipal {
    /// The key's owner.
    pub user: User,
    /// The `api_keys` row id.
    pub key_id: i64,
    /// The key's name.
    pub key_name: String,
    /// What the key may do.
    pub scope: KeyScope,
}

/// A session found by its token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLookup {
    /// Who the session acts for.
    pub principal: SessionPrincipal,
    /// Whether this use extended the session, which happens at most hourly; the server sends a
    /// fresh cookie then.
    pub renewed: bool,
}
