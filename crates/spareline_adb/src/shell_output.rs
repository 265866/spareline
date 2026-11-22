//! What a finished shell command printed and returned.

/// The output of a command run with the `shell,v2` protocol, which keeps stdout, stderr and the
/// exit code apart.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellOutput {
    /// Everything the command wrote to stdout.
    pub stdout: Vec<u8>,
    /// Everything the command wrote to stderr.
    pub stderr: Vec<u8>,
    /// The command's exit code.
    pub exit_code: u8,
}

impl ShellOutput {
    /// Stdout as text, with invalid UTF-8 replaced.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Stderr as text, with invalid UTF-8 replaced.
    #[must_use]
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}
