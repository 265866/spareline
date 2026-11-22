//! Tests for the command line of `spareline`.

use std::fs;
use std::path::{Path, PathBuf};
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

/// A new, empty directory for one test under Cargo's temporary directory for integration tests.
fn scratch(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    if path.exists() {
        fs::remove_dir_all(&path).expect("an old scratch directory can be removed");
    }
    fs::create_dir_all(&path).expect("a scratch directory can be created");
    path
}

/// Runs the built `spareline` with `arguments` and only `DATA_DIR` set to `data_dir`.
fn run_in(data_dir: &Path, arguments: &[&str]) -> Output {
    let data = data_dir.to_str().expect("the scratch path is UTF-8");
    run_with(arguments, &[["DATA_DIR", data]])
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn users_and_keys_manage_the_database_in_data_dir() {
    let data = scratch("users-and-keys").join("data");
    let listed = run_in(&data, &["users", "list"]);
    assert_eq!(listed.status.code(), Some(0), "{listed:?}");
    assert_eq!(stdout(&listed), "No users yet.\n");
    let log = stderr(&listed);
    assert_eq!(log.lines().count(), 3, "one line per migration: {log}");
    assert!(
        log.lines()
            .all(|line| line.contains(r#""event":"migration-applied""#)),
        "{log}"
    );
    assert!(data.join("spareline.db").is_file());

    let added = run_in(&data, &["users", "add", "Erin"]);
    assert_eq!(added.status.code(), Some(0), "{added:?}");
    assert!(
        stdout(&added).starts_with("Added Erin. Password (shown once): "),
        "{added:?}"
    );
    assert_eq!(stderr(&added), "", "nothing is left to migrate");

    let key = run_in(&data, &["keys", "add", "erin", "ci", "full"]);
    assert_eq!(key.status.code(), Some(0), "{key:?}");
    assert!(
        stdout(&key).starts_with("Added full key 1 \"ci\" for Erin. Key (shown once): sl_"),
        "{key:?}"
    );
    let revoked = run_in(&data, &["keys", "revoke", "1"]);
    assert_eq!(stdout(&revoked), "Revoked key 1 \"ci\".\n");

    let refused = run_in(&data, &["users", "remove", "nobody"]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert_eq!(stdout(&refused), "no user nobody\n");
    assert_eq!(stderr(&refused), "", "a refusal goes to stdout only");
}

#[test]
fn bad_command_arguments_fail_with_exit_2_as_the_live_build_does() {
    let data = scratch("bad-arguments");
    let revoke = run_in(&data, &["keys", "revoke", "x"]);
    assert_eq!(revoke.status.code(), Some(2), "{revoke:?}");
    assert!(
        stderr(&revoke).starts_with(
            "error: invalid value 'x' for '<ID>': expected a key id of digits, got \"x\""
        ),
        "{revoke:?}"
    );
    let too_large = run_in(&data, &["keys", "revoke", "99999999999999999999"]);
    assert_eq!(too_large.status.code(), Some(2), "{too_large:?}");
    assert!(
        stderr(&too_large).starts_with(
            "error: invalid value '99999999999999999999' for '<ID>': number too large to fit in \
             target type"
        ),
        "{too_large:?}"
    );
    let scope = run_in(&data, &["keys", "add", "a", "b", "x"]);
    assert_eq!(scope.status.code(), Some(2), "{scope:?}");
    assert!(
        stderr(&scope).starts_with(
            "error: invalid value 'x' for '<read|full>': a key scope is read or full, not \"x\""
        ),
        "{scope:?}"
    );
    let users = run_in(&data, &["users"]);
    assert_eq!(users.status.code(), Some(2), "{users:?}");
    let backup = run_in(&data, &["backup"]);
    assert_eq!(backup.status.code(), Some(2), "{backup:?}");
    assert!(
        stderr(&backup).contains("Usage: spareline backup <SOURCE> <DESTINATION>"),
        "{backup:?}"
    );
    let both = run_in(&data, &["backup", "--check", "a", "b"]);
    assert_eq!(both.status.code(), Some(2), "{both:?}");
    assert!(
        fs::read_dir(&data)
            .expect("the scratch exists")
            .next()
            .is_none(),
        "a refused command line opens no database"
    );
}

#[test]
fn an_uncreatable_data_dir_is_reported() {
    let file = scratch("uncreatable").join("file");
    fs::write(&file, "").expect("the scratch is writable");
    let inside = file.join("data");
    let output = run_in(&inside, &["users", "list"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr(&output).starts_with(&format!(
            "cannot create the directory {}: ",
            inside.display()
        )),
        "{output:?}"
    );
}

#[test]
fn backup_copies_a_database_and_checks_the_copy() {
    let dir = scratch("backup");
    let data = dir.join("data");
    let listed = run_in(&data, &["users", "list"]);
    assert_eq!(listed.status.code(), Some(0), "{listed:?}");
    let source = data.join("spareline.db");
    let copy = dir.join("copy.db");
    let source_text = source.to_str().expect("UTF-8 path");
    let copy_text = copy.to_str().expect("UTF-8 path");
    let saved = run_in(&data, &["backup", source_text, copy_text]);
    assert_eq!(saved.status.code(), Some(0), "{saved:?}");
    assert_eq!(stdout(&saved), format!("snapshot saved to {copy_text}\n"));
    let checked = run_in(&data, &["backup", "--check", copy_text]);
    assert_eq!(checked.status.code(), Some(0), "{checked:?}");
    assert_eq!(stdout(&checked), format!("integrity ok: {copy_text}\n"));
    let again = run_in(&data, &["backup", source_text, copy_text]);
    assert_eq!(again.status.code(), Some(1), "{again:?}");
    assert_eq!(
        stderr(&again),
        format!("backup destination already exists: {copy_text}\n")
    );
    let missing = dir.join("missing.db");
    let missing_text = missing.to_str().expect("UTF-8 path");
    let unreadable = run_in(&data, &["backup", missing_text, copy_text]);
    assert_eq!(unreadable.status.code(), Some(1), "{unreadable:?}");
    assert!(
        stderr(&unreadable).contains(missing_text),
        "the failure names the source: {unreadable:?}"
    );
}

#[test]
fn policy_check_reports_a_bad_adb_server_a_missing_profile_and_an_unreachable_server() {
    let bad = run_with(
        &["policy-check", "S", "profile.json"],
        &[["ADB_SERVER", "nope"]],
    );
    assert_eq!(bad.status.code(), Some(1), "{bad:?}");
    assert_eq!(
        stderr(&bad),
        "invalid environment:\n  ADB_SERVER: expected host:port for the adb server, got \"nope\"\n"
    );
    let missing = run_with(
        &["policy-check", "S", "/nonexistent/profile.json"],
        &[["ADB_SERVER", "127.0.0.1:1"]],
    );
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    assert!(
        stderr(&missing).starts_with("cannot read policy profile /nonexistent/profile.json: "),
        "{missing:?}"
    );
    let profile = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../spareline_core/tests/fixtures/no-data.json"
    );
    let unreachable = run_with(
        &["policy-check", "S", profile],
        &[["ADB_SERVER", "127.0.0.1:1"]],
    );
    assert_eq!(unreachable.status.code(), Some(1), "{unreachable:?}");
    assert!(
        stderr(&unreachable).starts_with("cannot reach the adb server at 127.0.0.1:1: "),
        "{unreachable:?}"
    );
}
