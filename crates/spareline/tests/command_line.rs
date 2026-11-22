//! Tests for the command line of `spareline`.

use std::process::{Command, Output};

/// Runs the built `spareline` with `arguments`.
fn run(arguments: &[&str]) -> Output {
    let binary = env!("CARGO_BIN_EXE_spareline");
    Command::new(binary)
        .args(arguments)
        .output()
        .expect("the binary should start")
}

#[test]
fn version_prints_the_version_line_and_succeeds() {
    let output = run(&["--version"]);
    assert!(
        output.status.success(),
        "CI's Container image job runs the image with `--version` and needs exit 0: {output:?}"
    );
    let name = "spareline";
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{name} {}\n", env!("CARGO_PKG_VERSION")),
        "`--version` should print the name and the version"
    );
}

#[test]
fn an_unknown_argument_fails() {
    let output = run(&["--unknown"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "an argument the service doesn't know should fail: {output:?}"
    );
}

/// Runs the built `spareline` with `arguments` and the environment variables `variables`.
fn run_with(arguments: &[&str], variables: &[[&str; 2]]) -> Output {
    let binary = env!("CARGO_BIN_EXE_spareline");
    Command::new(binary)
        .args(arguments)
        .env_clear()
        .envs(variables.iter().map(|[name, value]| (name, value)))
        .output()
        .expect("the binary should start")
}

#[test]
fn smoke_fails_when_the_adb_server_is_unreachable() {
    let output = run_with(&["smoke"], &[["ADB_SERVER", "127.0.0.1:1"]]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("smoke failed: cannot reach the adb server at 127.0.0.1:1: "),
        "the deploy kit's log shows this line: {stderr}"
    );
}

#[test]
fn smoke_reports_an_invalid_environment() {
    let output = run_with(&["smoke"], &[["PORT", "abc"]]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "smoke failed: invalid environment:\n  PORT: expected a whole number from 1 to 65535, got \
         \"abc\"\n"
    );
}

#[test]
fn healthcheck_fails_when_nothing_listens() {
    let output = run_with(&["healthcheck"], &[["PORT", "1"]]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("unhealthy: cannot ask 127.0.0.1:1: "),
        "{stderr}"
    );
}
