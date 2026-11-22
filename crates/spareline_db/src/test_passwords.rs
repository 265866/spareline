//! Password hashing for tests.

use crate::password_hash::Parameters;
use crate::passwords::Passwords;

/// Hashes with the lowest cost argon2 allows, so tests that hash often stay fast. Verifying
/// reads the cost from the hash, so these hashes check like any other.
pub(crate) fn cheap() -> Passwords {
    Passwords::with_parameters(Parameters {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
    })
}
