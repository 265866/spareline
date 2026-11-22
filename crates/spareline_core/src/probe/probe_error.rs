//! A probe script that failed.

use thiserror::Error;

use crate::text;

/// A probe script that exited with a status above 1.
#[must_use]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("probe script exited {exit_code}: {stderr}")]
pub struct ProbeError {
    /// The script's exit status.
    pub exit_code: i32,
    /// The script's standard error, trimmed.
    pub stderr: String,
}

/// Checks a probe script's exit status. `grep` exits 1 when it matches nothing, which isn't a
/// failure: the section it would fill is simply empty. So only a status above 1 fails.
///
/// # Errors
///
/// [`ProbeError`] with the status and the trimmed standard error when `exit_code` is above 1.
pub fn check_probe_exit(exit_code: i32, stderr: &str) -> Result<(), ProbeError> {
    if exit_code > 1 {
        return Err(ProbeError {
            exit_code,
            stderr: text::trim(stderr).to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_up_to_1_pass() {
        assert_eq!(check_probe_exit(0, "noise"), Ok(()));
        assert_eq!(check_probe_exit(1, ""), Ok(()));
        assert_eq!(check_probe_exit(-1, ""), Ok(()));
        let error = check_probe_exit(127, " sh: dumpsys: not found\n").expect_err("fails");
        assert_eq!(
            error.to_string(),
            "probe script exited 127: sh: dumpsys: not found"
        );
    }
}
