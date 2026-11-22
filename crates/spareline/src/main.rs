//! `spareline`: Spare phone lines on spare phones, with an inbox for each line.

use std::ffi::OsString;
use std::process::ExitCode;
use std::{env, io};

fn main() -> io::Result<ExitCode> {
    use std::io::Write as _;

    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    match arguments.as_slice() {
        [] => run(),
        [flag] if flag == "--version" => {
            let line = spareline_core::version_line();
            writeln!(io::stdout().lock(), "{line}")?;
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            let name = env!("CARGO_BIN_NAME");
            writeln!(io::stderr().lock(), "usage: {name} [--version]")?;
            Ok(ExitCode::from(2))
        }
    }
}

/// Runs `spareline`, which so far only prints its version line.
fn run() -> io::Result<ExitCode> {
    use std::io::Write as _;

    let line = spareline_core::version_line();
    writeln!(io::stdout().lock(), "{line}")?;
    Ok(ExitCode::SUCCESS)
}
