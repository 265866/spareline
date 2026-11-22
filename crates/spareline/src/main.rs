//! `spareline`: Spare phone lines on spare phones, with an inbox for each line.

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Arg, ArgAction, ArgMatches, Command};
use spareline_adb::AdbClient;
use spareline_server::healthcheck::Healthcheck;
use spareline_server::smoke::Smoke;
use spareline_server::{Config, SmokeSettings};
use tokio::runtime::{Builder, Runtime};

/// The command line, built with clap's builder API as `docs/style.md` asks.
fn command() -> Command {
    Command::new("spareline")
        .about("Spare phone lines on spare phones, with an inbox for each line.")
        .disable_version_flag(true)
        .arg(
            Arg::new("version")
                .long("version")
                .action(ArgAction::SetTrue)
                .help("Print the version line and exit"),
        )
        .after_help("Without a command, it runs the service, configured by the environment.")
        .subcommand(Command::new("smoke").about(
            "Exit 0 if the adb server answers, every online phone answers a read-only getprop, \
             and at least SMOKE_MIN_PHONES (default 1) are online",
        ))
        .subcommand(Command::new("healthcheck").about(
            "Exit 0 if the service on 127.0.0.1:PORT answers /healthz with a 2xx, 1 otherwise",
        ))
}

/// What the command line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Invocation {
    /// `--version`.
    Version,
    /// `smoke`.
    Smoke,
    /// `healthcheck`.
    Healthcheck,
    /// No command: run the service.
    Service,
}

impl Invocation {
    /// Reads the parsed command line.
    fn from_matches(matches: &ArgMatches) -> Self {
        if matches.get_flag("version") {
            return Self::Version;
        }
        match matches.subcommand_name() {
            Some("smoke") => Self::Smoke,
            Some("healthcheck") => Self::Healthcheck,
            Some(name) => unreachable!("clap accepted the command {name}, which nothing runs"),
            None => Self::Service,
        }
    }
}

fn main() -> io::Result<ExitCode> {
    let matches = match command().try_get_matches() {
        Ok(matches) => matches,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            error.print()?;
            return Ok(ExitCode::from(code));
        }
    };
    match Invocation::from_matches(&matches) {
        Invocation::Version => {
            writeln!(io::stdout().lock(), "{}", spareline_core::version_line())?;
            Ok(ExitCode::SUCCESS)
        }
        Invocation::Smoke => run_smoke(&Builder::new_multi_thread().enable_all().build()?),
        Invocation::Healthcheck => run_healthcheck(),
        Invocation::Service => run_service(),
    }
}

/// Reads one variable of the process's environment.
fn environment(name: &str) -> Option<OsString> {
    env::var_os(name)
}

fn run_smoke(runtime: &Runtime) -> io::Result<ExitCode> {
    let outcome = SmokeSettings::from_env(environment)
        .map_err(|error| error.to_string())
        .and_then(|settings| {
            let adb = AdbClient::new(settings.config.adb_server);
            let mut stdout = io::stdout().lock();
            runtime
                .block_on(Smoke::new(&adb, settings.min_phones).run(&mut stdout))
                .map_err(|error| error.to_string())
        });
    match outcome {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(message) => {
            writeln!(io::stderr().lock(), "smoke failed: {message}")?;
            Ok(ExitCode::FAILURE)
        }
    }
}

fn run_healthcheck() -> io::Result<ExitCode> {
    let outcome = Config::from_env(environment)
        .map_err(|error| error.to_string())
        .and_then(|config| {
            Healthcheck::new(config.port)
                .run()
                .map_err(|error| error.to_string())
        });
    match outcome {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(message) => {
            writeln!(io::stderr().lock(), "unhealthy: {message}")?;
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Runs the service, which so far only prints its version line.
fn run_service() -> io::Result<ExitCode> {
    writeln!(io::stdout().lock(), "{}", spareline_core::version_line())?;
    Ok(ExitCode::SUCCESS)
}
