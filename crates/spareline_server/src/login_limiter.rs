//! Failed password checks per user name, in memory.

use std::collections::HashMap;
use std::time::Duration;

use spareline_core::Timestamp;

/// How many failures inside [`WINDOW`] lock a name out.
const MAX_FAILURES: usize = 10;

/// How long a failure counts.
const WINDOW: Duration = Duration::from_mins(15);

/// Past this many names, a failure first sweeps out names whose failures all expired.
const SWEEP_ABOVE: usize = 10_000;

/// Failed password checks per key, such as `name:alice`. A key with ten failures inside 15
/// minutes is refused until its oldest one is that old. A restart forgets them. There is no
/// per-address key: behind the reverse proxy every client has the proxy's address, so one client
/// could lock everyone out.
#[derive(Debug, Default)]
pub struct LoginLimiter {
    failures: HashMap<String, Vec<Timestamp>>,
}

impl LoginLimiter {
    /// An empty limiter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The key for a user name: `name:` and the trimmed name in lowercase.
    #[must_use]
    pub fn key_for(name: &str) -> String {
        format!("name:{}", name.trim().to_lowercase())
    }

    /// Whole seconds to wait when any of `keys` is over the limit at `now`, otherwise `None`.
    pub fn retry_after(&mut self, keys: &[String], now: Timestamp) -> Option<u64> {
        let mut wait = Duration::ZERO;
        for key in keys {
            let recent = self.recent(key, now);
            if recent.len() >= MAX_FAILURES {
                let unlocks = recent[0].plus(WINDOW);
                wait = wait.max(unlocks.since(now));
            }
        }
        (wait > Duration::ZERO).then(|| {
            wait.as_millis()
                .div_ceil(1000)
                .try_into()
                .unwrap_or(u64::MAX)
        })
    }

    /// Records an attempt as a failure at `now`. Callers record it before checking the password
    /// and clear it with [`LoginLimiter::succeed`], so a burst of concurrent attempts can't all
    /// pass [`LoginLimiter::retry_after`] first.
    pub fn fail(&mut self, keys: &[String], now: Timestamp) {
        if self.failures.len() > SWEEP_ABOVE {
            let names: Vec<String> = self.failures.keys().cloned().collect();
            for name in names {
                self.recent(&name, now);
            }
        }
        for key in keys {
            self.recent(key, now);
            self.failures.entry(key.clone()).or_default().push(now);
        }
    }

    /// Forgets every failure of `keys`.
    pub fn succeed(&mut self, keys: &[String]) {
        for key in keys {
            self.failures.remove(key);
        }
    }

    /// The failures of `key` still inside the window at `now`, oldest first; drops the rest.
    fn recent(&mut self, key: &str, now: Timestamp) -> Vec<Timestamp> {
        let cutoff = now.minus(WINDOW);
        let recent: Vec<Timestamp> = self.failures.get(key).map_or_else(Vec::new, |times| {
            times.iter().copied().filter(|&at| at > cutoff).collect()
        });
        if recent.is_empty() {
            self.failures.remove(key);
        } else {
            self.failures.insert(key.to_owned(), recent.clone());
        }
        recent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_millis(1_793_000_000_000 + seconds * 1000)
    }

    #[test]
    fn ten_failures_lock_a_name_until_the_oldest_expires() {
        let mut limiter = LoginLimiter::new();
        let keys = vec![LoginLimiter::key_for(" Alice ")];
        assert_eq!(keys[0], "name:alice");
        for second in 0..9 {
            limiter.fail(&keys, at(second));
            assert_eq!(
                limiter.retry_after(&keys, at(second)),
                None,
                "after {} failures",
                second + 1
            );
        }
        limiter.fail(&keys, at(9));
        assert_eq!(limiter.retry_after(&keys, at(9)), Some(15 * 60 - 9));
        assert_eq!(limiter.retry_after(&keys, at(899)), Some(1));
        assert_eq!(
            limiter.retry_after(&keys, at(900)),
            None,
            "the oldest failure has expired"
        );
    }

    #[test]
    fn waits_round_up_to_whole_seconds() {
        let mut limiter = LoginLimiter::new();
        let keys = vec![LoginLimiter::key_for("bob")];
        for _ in 0..10 {
            limiter.fail(&keys, at(0));
        }
        let almost = Timestamp::from_millis(at(899).millis() + 1);
        assert_eq!(limiter.retry_after(&keys, almost), Some(1));
    }

    #[test]
    fn a_success_forgets_the_failures() {
        let mut limiter = LoginLimiter::new();
        let keys = vec![LoginLimiter::key_for("carol")];
        for second in 0..10 {
            limiter.fail(&keys, at(second));
        }
        limiter.succeed(&keys);
        assert_eq!(limiter.retry_after(&keys, at(10)), None);
    }

    #[test]
    fn names_are_counted_apart() {
        let mut limiter = LoginLimiter::new();
        let alice = vec![LoginLimiter::key_for("alice")];
        let bob = vec![LoginLimiter::key_for("bob")];
        for second in 0..10 {
            limiter.fail(&alice, at(second));
        }
        assert_eq!(limiter.retry_after(&bob, at(10)), None);
        assert!(limiter.retry_after(&alice, at(10)).is_some());
    }
}
