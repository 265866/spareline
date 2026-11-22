//! `spareline policy-check`: a profile's read on one phone, with every rule's state. Read only.

use std::fs;
use std::io::{self, Write};
use std::time::Duration;

use spareline_adb::{AdbClient, AdbError};
use spareline_core::Timestamp;
use spareline_core::monitor::{self, CheckContext, Policy, ProfileError};
use thiserror::Error;
use tokio::time::Instant;

/// How long the batched read may take, as the monitor allows it.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Why the check couldn't run.
#[derive(Debug, Error)]
#[must_use]
pub enum PolicyCheckError {
    /// The profile file couldn't be read.
    #[error("cannot read policy profile {path}: {source}")]
    Unreadable {
        /// The profile's path.
        path: String,
        /// The failure.
        source: io::Error,
    },
    /// The profile isn't valid.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// The phone or the adb server didn't answer.
    #[error(transparent)]
    Adb(#[from] AdbError),
    /// Writing the report failed.
    #[error("cannot write the report: {source}")]
    Output {
        /// The failure.
        #[from]
        source: io::Error,
    },
}

/// Runs the profile at `profile_path` (whose text is `profile_text`) as the monitor's read on
/// the phone `serial`, and writes each rule's state to `out` as spareline-old's
/// `scripts/policy-check.ts` did, then the data watch for a radios-off profile. It runs only the
/// read, never an enforce command.
///
/// # Errors
///
/// Returns why the check couldn't run.
pub async fn policy_check<W: Write>(
    adb: &AdbClient,
    serial: &str,
    profile_path: &str,
    profile_text: &str,
    now: Timestamp,
    out: &mut W,
) -> Result<(), PolicyCheckError> {
    let policy = Policy::load(profile_path, profile_text)?;
    let started = Instant::now();
    let reader = adb.clone().with_call_timeout(READ_TIMEOUT);
    let output = reader.shell(serial, &policy.read_script).await?;
    let elapsed = (started.elapsed().as_micros() + 500) / 1000;
    let stdout = output.stdout_text();
    let stdout = stdout.strip_prefix('\u{feff}').unwrap_or(&stdout);
    let snapshot = policy.parse(stdout);
    writeln!(
        out,
        "read: 1 shell call, {elapsed} ms, {} bytes, exit {}",
        output.stdout.len(),
        output.exit_code
    )?;
    let context = CheckContext {
        screen_session: false,
    };
    for rule in monitor::evaluate(&snapshot, context, &policy.rules) {
        let verdict = if rule.ok { "ok  " } else { "FAIL" };
        let want = if rule.ok {
            String::new()
        } else {
            format!("  (want {})", rule.want)
        };
        writeln!(out, "{verdict}  {:<22} {}{want}", rule.id, rule.observed)?;
    }
    if policy.radios_off.is_some() {
        let usage = policy.usage(&snapshot, now);
        let summary = if usage.is_empty() {
            "no internet mobile traffic in 24 h".to_owned()
        } else {
            let sims: Vec<String> = usage
                .iter()
                .map(|sim| format!("sub {}: {} bytes", sim.sub_id, sim.rx + sim.tx))
                .collect();
            sims.join(", ")
        };
        writeln!(out, "data watch: {summary}")?;
    }
    Ok(())
}

/// Reads the profile file at `path`.
///
/// # Errors
///
/// [`PolicyCheckError::Unreadable`] when it can't be read.
pub fn read_profile(path: &str) -> Result<String, PolicyCheckError> {
    fs::read_to_string(path).map_err(|source| PolicyCheckError::Unreadable {
        path: path.to_owned(),
        source,
    })
}
