//! The service's settings, read from the environment.

use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;
use std::time::Duration;

use spareline_adb::{Endpoint, EndpointError};
use thiserror::Error;

/// Every setting the service and its commands read from the environment, validated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// The address the HTTP server binds, `HOST`.
    pub host: String,
    /// The port the HTTP server binds, `PORT`.
    pub port: u16,
    /// The adb server, `ADB_SERVER`.
    pub adb_server: Endpoint,
    /// The directory of the database and the attachments, `DATA_DIR`.
    pub data_dir: PathBuf,
    /// A name for this host, stored on each phone it sees, `SITE`.
    pub site: String,
    /// How often each online phone's health is read and sampled, `HEALTH_INTERVAL_S`.
    pub health_interval: Duration,
    /// The built web UI, `SPARELINE_WEB_DIR`, or `WEB_DIR` as spareline-old named it.
    pub web_dir: PathBuf,
    /// How often each online phone's policy is checked, `POLICY_CHECK_INTERVAL_S`.
    pub policy_check_interval: Duration,
    /// The policy profile, `POLICY_PROFILE`, or `None` to check no settings on the phones.
    pub policy_profile: Option<PathBuf>,
}

/// One environment variable that failed validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvIssue {
    /// The variable's name, such as `PORT`.
    pub variable: &'static str,
    /// What was wrong, such as `expected a whole number from 1 to 65535, got "abc"`.
    pub message: String,
}

/// The environment held invalid values. It shows as `invalid environment:` and one indented
/// line per variable, as the live build prints it.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[must_use]
pub struct EnvError {
    issues: Vec<EnvIssue>,
}

impl EnvError {
    /// The variables that failed, in the order they were checked.
    #[must_use]
    pub fn issues(&self) -> &[EnvIssue] {
        &self.issues
    }
}

impl Display for EnvError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "invalid environment:")?;
        for issue in &self.issues {
            write!(f, "\n  {}: {}", issue.variable, issue.message)?;
        }
        Ok(())
    }
}

/// Collects the issues of one validation pass.
#[derive(Debug, Default)]
struct Issues {
    issues: Vec<EnvIssue>,
}

impl Issues {
    fn add(&mut self, variable: &'static str, message: String) {
        self.issues.push(EnvIssue { variable, message });
    }

    fn finish<T>(self, value: T) -> Result<T, EnvError> {
        if self.issues.is_empty() {
            Ok(value)
        } else {
            Err(EnvError {
                issues: self.issues,
            })
        }
    }
}

/// Reads a whole number from `min` to `max` from `variable`, or `default` when it's unset.
fn whole_number<F: Fn(&str) -> Option<String>>(
    lookup: &F,
    issues: &mut Issues,
    variable: &'static str,
    min: u64,
    max: u64,
    default: u64,
) -> u64 {
    let Some(raw) = lookup(variable) else {
        return default;
    };
    let parsed: Option<u64> = raw
        .trim()
        .parse()
        .ok()
        .filter(|value| (min..=max).contains(value));
    parsed.unwrap_or_else(|| {
        issues.add(
            variable,
            format!("expected a whole number from {min} to {max}, got \"{raw}\""),
        );
        default
    })
}

impl Config {
    /// Reads the settings through `lookup`, which returns a variable's value or `None` when it's
    /// unset, such as `|name| std::env::var(name).ok()`.
    ///
    /// # Errors
    ///
    /// Returns every invalid variable at once.
    pub fn from_env<F: Fn(&str) -> Option<String>>(lookup: F) -> Result<Self, EnvError> {
        let mut issues = Issues::default();
        let host = lookup("HOST").unwrap_or_else(|| "127.0.0.1".to_owned());
        let port = whole_number(&lookup, &mut issues, "PORT", 1, 65_535, 8740);
        let adb_raw = lookup("ADB_SERVER").unwrap_or_else(|| "127.0.0.1:5037".to_owned());
        let parsed: Result<Endpoint, EndpointError> = adb_raw.parse();
        let adb_server = parsed.unwrap_or_else(|error| {
            issues.add("ADB_SERVER", error.to_string());
            Endpoint::new("127.0.0.1".to_owned(), 5037)
        });
        let data_dir = PathBuf::from(lookup("DATA_DIR").unwrap_or_else(|| "data".to_owned()));
        let site = match lookup("SITE") {
            None => "local".to_owned(),
            Some(raw) if raw.trim().is_empty() => {
                issues.add(
                    "SITE",
                    "expected a name for this host, got only blanks".to_owned(),
                );
                String::new()
            }
            Some(raw) => raw.trim().to_owned(),
        };
        let health = whole_number(&lookup, &mut issues, "HEALTH_INTERVAL_S", 5, 3600, 60);
        let web_dir = lookup("SPARELINE_WEB_DIR")
            .or_else(|| lookup("WEB_DIR"))
            .unwrap_or_else(|| "dist/web".to_owned());
        let policy_check = whole_number(
            &lookup,
            &mut issues,
            "POLICY_CHECK_INTERVAL_S",
            30,
            86_400,
            300,
        );
        let policy_profile = lookup("POLICY_PROFILE")
            .map(|raw| raw.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let config = Self {
            host,
            port: u16::try_from(port).unwrap_or(8740),
            adb_server,
            data_dir,
            site,
            health_interval: Duration::from_secs(health),
            web_dir: PathBuf::from(web_dir),
            policy_check_interval: Duration::from_secs(policy_check),
            policy_profile,
        };
        issues.finish(config)
    }

    /// The database file, `DATA_DIR/spareline.db`.
    #[must_use]
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("spareline.db")
    }
}

/// Reads `SMOKE_MIN_PHONES`, the number of online phones `spareline smoke` needs, 1 when unset.
///
/// # Errors
///
/// Returns an error when the value isn't a whole number.
pub fn smoke_min_phones<F: Fn(&str) -> Option<String>>(lookup: F) -> Result<u32, EnvError> {
    let mut issues = Issues::default();
    let minimum = whole_number(
        &lookup,
        &mut issues,
        "SMOKE_MIN_PHONES",
        0,
        u64::from(u32::MAX),
        1,
    );
    issues.finish(u32::try_from(minimum).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn read(pairs: &[[&str; 2]]) -> Result<Config, EnvError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|[name, value]| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        Config::from_env(|name| map.get(name).cloned())
    }

    #[test]
    fn the_defaults_match_spareline_old() {
        let config = read(&[]).unwrap();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 8740);
        assert_eq!(config.adb_server.to_string(), "127.0.0.1:5037");
        assert_eq!(config.data_dir, PathBuf::from("data"));
        assert_eq!(config.database_path(), PathBuf::from("data/spareline.db"));
        assert_eq!(config.site, "local");
        assert_eq!(config.health_interval, Duration::from_secs(60));
        assert_eq!(config.web_dir, PathBuf::from("dist/web"));
        assert_eq!(config.policy_check_interval, Duration::from_secs(300));
        assert_eq!(config.policy_profile, None);
    }

    #[test]
    fn reads_the_image_settings() {
        let config = read(&[
            ["HOST", "0.0.0.0"],
            ["PORT", "8740"],
            ["DATA_DIR", "/app/data"],
            ["SPARELINE_WEB_DIR", "/usr/share/spareline/web"],
            ["WEB_DIR", "/ignored"],
            ["SITE", "  pi  "],
            ["POLICY_PROFILE", " /app/profiles/nuu-b30-no-data.json "],
        ])
        .unwrap();
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.data_dir, PathBuf::from("/app/data"));
        assert_eq!(config.web_dir, PathBuf::from("/usr/share/spareline/web"));
        assert_eq!(config.site, "pi");
        assert_eq!(
            config.policy_profile,
            Some(PathBuf::from("/app/profiles/nuu-b30-no-data.json"))
        );
    }

    #[test]
    fn an_empty_profile_means_none_and_web_dir_is_the_fallback() {
        let config = read(&[["POLICY_PROFILE", "  "], ["WEB_DIR", "/web"]]).unwrap();
        assert_eq!(config.policy_profile, None);
        assert_eq!(config.web_dir, PathBuf::from("/web"));
    }

    #[test]
    fn reports_every_invalid_variable_as_the_live_build_does() {
        let error = read(&[
            ["PORT", "abc"],
            ["ADB_SERVER", "nope"],
            ["SITE", "   "],
            ["HEALTH_INTERVAL_S", "1"],
            ["POLICY_CHECK_INTERVAL_S", "86401"],
        ])
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid environment:\n  \
             PORT: expected a whole number from 1 to 65535, got \"abc\"\n  \
             ADB_SERVER: expected host:port for the adb server, got \"nope\"\n  \
             SITE: expected a name for this host, got only blanks\n  \
             HEALTH_INTERVAL_S: expected a whole number from 5 to 3600, got \"1\"\n  \
             POLICY_CHECK_INTERVAL_S: expected a whole number from 30 to 86400, got \"86401\""
        );
    }

    #[test]
    fn port_zero_and_an_empty_port_are_invalid() {
        for raw in ["0", "", "65536", "-1"] {
            let error = read(&[["PORT", raw]]).unwrap_err();
            assert_eq!(error.issues()[0].variable, "PORT", "{raw:?}");
        }
    }

    #[test]
    fn smoke_needs_one_phone_unless_told_otherwise() {
        assert_eq!(smoke_min_phones(|_| None).unwrap(), 1);
        assert_eq!(smoke_min_phones(|_| Some("0".to_owned())).unwrap(), 0);
        assert_eq!(
            smoke_min_phones(|_| Some("x".to_owned()))
                .unwrap_err()
                .to_string(),
            "invalid environment:\n  SMOKE_MIN_PHONES: expected a whole number from 0 to \
             4294967295, got \"x\""
        );
    }
}
