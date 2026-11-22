//! API keys for scripts, as the API shows them.

use rusqlite::ToSql;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use serde::Serialize;

/// What an API key may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyScope {
    /// `GET` and `HEAD` requests, and no screen WebSocket.
    Read,
    /// Everything a signed-in user can do except managing keys and passwords.
    Full,
}

impl KeyScope {
    /// The scope as the API and the database spell it: `read` or `full`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Full => "full",
        }
    }

    /// The scope `text` names, or `None` for anything but `read` and `full`.
    #[must_use]
    pub fn from_name(text: &str) -> Option<Self> {
        match text {
            "read" => Some(Self::Read),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

impl FromSql for KeyScope {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        Self::from_name(text)
            .ok_or_else(|| FromSqlError::Other(format!("unknown key scope {text:?}").into()))
    }
}

impl ToSql for KeyScope {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

/// An API key as `GET /api/v1/api-keys` lists it. Only the key's first characters are kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyView {
    /// The row id.
    pub id: i64,
    /// The owner's name.
    pub user: String,
    /// The key's name.
    pub name: String,
    /// What the key may do.
    pub scope: KeyScope,
    /// The key's first 9 characters, `sl_` and 6 more, to tell keys apart.
    pub prefix: String,
    /// When the key was made.
    pub created_at: String,
    /// When the key was last used, written at most once a minute.
    pub last_used_at: Option<String>,
    /// When the key was revoked.
    pub revoked_at: Option<String>,
}

/// A key just made, as `POST /api/v1/api-keys` answers: `{"key": "sl_...", "apiKey": {...}}`.
/// The key itself is shown once; the database keeps only its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApiKey {
    /// The key.
    pub key: String,
    /// How it is listed.
    pub api_key: ApiKeyView,
}
