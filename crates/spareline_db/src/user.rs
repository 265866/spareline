//! People who can sign in.

use crate::password_hash::PasswordHash;

/// A user, as the `users` table holds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// The row id.
    pub id: i64,
    /// The name, in the letter case it was added with.
    pub name: String,
    /// When the user was added, in the 24-character form.
    pub created_at: String,
}

/// A user just added, with the generated password, which is shown once and stored only hashed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewUser {
    /// The user.
    pub user: User,
    /// The generated password.
    pub password: String,
}

/// A user found by name for a sign-in, with the stored hash to check the password against
/// with [`Passwords::verify_login`](crate::Passwords::verify_login), outside the database lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginCandidate {
    /// The user.
    pub user: User,
    pub(crate) hash: PasswordHash,
}
