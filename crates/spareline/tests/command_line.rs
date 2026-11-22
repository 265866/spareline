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
