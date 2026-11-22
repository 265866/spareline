//! `spareline`: Spare phone lines on spare phones, with an inbox for each line.

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Arg, ArgAction, ArgMatches, Command};
use spareline_adb::AdbClient;
use spareline_server::Config;
use spareline_server::healthcheck::Healthcheck;
use spareline_server::smoke::Smoke;
use tokio::runtime::Builder;

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
        .after_help(
            "Without a command, it runs the service, configured by the environment. The users \
             and keys commands open the database in DATA_DIR (default: data), like the service, \
             which may keep running.",
        )
        .subcommand(Command::new("smoke").about(
            "Exit 0 if the adb server answers, every online phone answers a read-only getprop, \
             and at least SMOKE_MIN_PHONES (default 1) are online",
        ))
        .subcommand(Command::new("healthcheck").about(
            "Exit 0 if the service on 127.0.0.1:PORT answers /healthz with a 2xx, 1 otherwise",
        ))
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
    if matches.get_flag("version") {
        writeln!(io::stdout().lock(), "{}", spareline_core::version_line())?;
        return Ok(ExitCode::SUCCESS);
    }
    match matches.subcommand() {
        Some(("smoke", _)) => run_smoke(),
        Some(("healthcheck", _)) => run_healthcheck(),
        Some((name, _)) => unknown(name),
        None => run_service(&matches),
    }
}

/// Reads the environment through the process's variables.
fn environment(name: &str) -> Option<String> {
    env::var(name).ok()
}

fn run_smoke() -> io::Result<ExitCode> {
    let outcome = Config::from_env(environment)
        .and_then(|config| {
            spareline_server::smoke_min_phones(environment).map(|minimum| (config, minimum))
        })
        .map_err(|error| error.to_string())
        .and_then(|(config, minimum)| {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            let adb = AdbClient::new(config.adb_server);
            let mut stdout = io::stdout().lock();
            runtime
                .block_on(Smoke::new(&adb, minimum).run(&mut stdout))
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

fn unknown(name: &str) -> io::Result<ExitCode> {
    writeln!(io::stderr().lock(), "unknown command {name}")?;
    Ok(ExitCode::from(2))
}

/// Runs the service, which so far only prints its version line.
fn run_service(_matches: &ArgMatches) -> io::Result<ExitCode> {
    writeln!(io::stdout().lock(), "{}", spareline_core::version_line())?;
    Ok(ExitCode::SUCCESS)
}
