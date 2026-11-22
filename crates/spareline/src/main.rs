//! `spareline`: Spare phone lines on spare phones, with an inbox for each line.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::{env, fs};

use clap::{Arg, ArgAction, ArgMatches, Command};
use spareline_adb::AdbClient;
use spareline_core::Timestamp;
use spareline_db::{AccountError, AppliedMigration, Database, KeyScope};
use spareline_server::healthcheck::Healthcheck;
use spareline_server::smoke::Smoke;
use spareline_server::{
    Clock, CommandOutcome, Config, EventLog, KeysCommand, Service, SmokeSettings, StopSignal,
    UsersCommand,
};
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
        .after_help(
            "Without a command, it runs the service, configured by the environment. The users \
             and keys commands open the database in DATA_DIR (default: data), like the service, \
             which may keep running.",
        )
        .subcommand(users_command())
        .subcommand(keys_command())
        .subcommand(Command::new("smoke").about(
            "Exit 0 if the adb server answers, every online phone answers a read-only getprop, \
             and at least SMOKE_MIN_PHONES (default 1) are online",
        ))
        .subcommand(Command::new("healthcheck").about(
            "Exit 0 if the service on 127.0.0.1:PORT answers /healthz with a 2xx, 1 otherwise",
        ))
        .subcommand(backup_command())
        .subcommand(
            Command::new("policy-check")
                .about(
                    "Run a policy profile's read on one phone and print every rule's state; \
                     changes nothing",
                )
                .arg(
                    Arg::new("serial")
                        .value_name("SERIAL")
                        .required(true)
                        .help("The phone's serial, as adb devices lists it"),
                )
                .arg(
                    Arg::new("profile")
                        .value_name("PROFILE")
                        .required(true)
                        .help("The policy profile, such as profiles/no-data.json"),
                ),
        )
}

/// A required positional argument named `id`, shown as `<value_name>`.
fn positional(id: &'static str, value_name: &'static str, help: &'static str) -> Arg {
    Arg::new(id)
        .value_name(value_name)
        .required(true)
        .help(help)
}

/// `spareline users`.
fn users_command() -> Command {
    Command::new("users")
        .about("Manage who can sign in")
        .subcommand_required(true)
        .subcommand(
            Command::new("add")
                .about("Add a user and print their generated password once")
                .arg(positional("name", "NAME", "The new user's name")),
        )
        .subcommand(Command::new("list").about("List the users"))
        .subcommand(
            Command::new("reset-password")
                .about("Print a new generated password once and end every session")
                .arg(positional("name", "NAME", "The user's name")),
        )
        .subcommand(
            Command::new("remove")
                .about("Remove a user with their sessions and keys")
                .arg(positional("name", "NAME", "The user's name")),
        )
}

/// `spareline keys`.
fn keys_command() -> Command {
    Command::new("keys")
        .about("Manage API keys")
        .subcommand_required(true)
        .subcommand(
            Command::new("add")
                .about("Make an API key for a user and print it once")
                .arg(positional("user", "USER", "The user the key acts for"))
                .arg(positional(
                    "key_name",
                    "KEY_NAME",
                    "A name to tell the key apart",
                ))
                .arg(
                    positional(
                        "scope",
                        "read|full",
                        "read: GET requests only; full: all but keys and passwords",
                    )
                    .value_parser(spareline_server::parse_scope),
                ),
        )
        .subcommand(Command::new("list").about("List every key"))
        .subcommand(
            Command::new("revoke").about("Revoke a key").arg(
                positional("id", "ID", "The key's id, as keys list prints it")
                    .value_parser(spareline_server::parse_key_id),
            ),
        )
}

/// `spareline backup`.
fn backup_command() -> Command {
    Command::new("backup")
        .about("Copy the live database to a new file, or check a copy")
        .arg(
            Arg::new("source")
                .value_name("SOURCE")
                .value_parser(clap::value_parser!(PathBuf))
                .required_unless_present("check")
                .help("The database, such as data/spareline.db"),
        )
        .arg(
            Arg::new("destination")
                .value_name("DESTINATION")
                .value_parser(clap::value_parser!(PathBuf))
                .required_unless_present("check")
                .help("The new file; it must not exist"),
        )
        .arg(
            Arg::new("check")
                .long("check")
                .value_name("FILE")
                .value_parser(clap::value_parser!(PathBuf))
                .conflicts_with_all(["source", "destination"])
                .help("Check FILE with PRAGMA integrity_check instead"),
        )
}

/// What `spareline backup` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum BackupRequest {
    /// `backup SOURCE DESTINATION`.
    Copy {
        /// The live database.
        source: PathBuf,
        /// The new file.
        destination: PathBuf,
    },
    /// `backup --check FILE`.
    Check {
        /// The copy to check.
        file: PathBuf,
    },
}

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Invocation {
    /// `--version`.
    Version,
    /// `users ...`.
    Users(UsersCommand),
    /// `keys ...`.
    Keys(KeysCommand),
    /// `smoke`.
    Smoke,
    /// `healthcheck`.
    Healthcheck,
    /// `backup ...`.
    Backup(BackupRequest),
    /// `policy-check SERIAL PROFILE`.
    PolicyCheck {
        /// The phone's serial.
        serial: String,
        /// The profile's path.
        profile: String,
    },
    /// No command: run the service.
    Service,
}

impl Invocation {
    /// Reads the parsed command line.
    fn from_matches(matches: &ArgMatches) -> Self {
        if matches.get_flag("version") {
            return Self::Version;
        }
        match matches.subcommand() {
            Some(("users", sub)) => Self::Users(users_from(sub)),
            Some(("keys", sub)) => Self::Keys(keys_from(sub)),
            Some(("smoke", _)) => Self::Smoke,
            Some(("healthcheck", _)) => Self::Healthcheck,
            Some(("backup", sub)) => Self::Backup(backup_from(sub)),
            Some(("policy-check", sub)) => Self::PolicyCheck {
                serial: text(sub, "serial"),
                profile: text(sub, "profile"),
            },
            Some((name, _)) => unreachable!("clap accepted the command {name}, which nothing runs"),
            None => Self::Service,
        }
    }
}

/// The value of the required argument `id`, which clap has checked is there.
fn text(matches: &ArgMatches, id: &str) -> String {
    matches
        .get_one::<String>(id)
        .cloned()
        .unwrap_or_else(|| unreachable!("clap requires the argument {id}"))
}

/// Reads `users <COMMAND>`.
fn users_from(matches: &ArgMatches) -> UsersCommand {
    match matches.subcommand() {
        Some(("add", sub)) => UsersCommand::Add {
            name: text(sub, "name"),
        },
        Some(("list", _)) => UsersCommand::List,
        Some(("reset-password", sub)) => UsersCommand::ResetPassword {
            name: text(sub, "name"),
        },
        Some(("remove", sub)) => UsersCommand::Remove {
            name: text(sub, "name"),
        },
        other => unreachable!("clap accepted users {other:?}, which nothing runs"),
    }
}

/// Reads `keys <COMMAND>`.
fn keys_from(matches: &ArgMatches) -> KeysCommand {
    match matches.subcommand() {
        Some(("add", sub)) => KeysCommand::Add {
            user: text(sub, "user"),
            key_name: text(sub, "key_name"),
            scope: *sub
                .get_one::<KeyScope>("scope")
                .unwrap_or_else(|| unreachable!("clap requires the scope")),
        },
        Some(("list", _)) => KeysCommand::List,
        Some(("revoke", sub)) => KeysCommand::Revoke {
            id: *sub
                .get_one::<i64>("id")
                .unwrap_or_else(|| unreachable!("clap requires the id")),
        },
        other => unreachable!("clap accepted keys {other:?}, which nothing runs"),
    }
}

/// Reads `backup`'s arguments.
fn backup_from(matches: &ArgMatches) -> BackupRequest {
    let path = |id: &str| matches.get_one::<PathBuf>(id).cloned();
    match (path("check"), path("source"), path("destination")) {
        (Some(file), _, _) => BackupRequest::Check { file },
        (None, Some(source), Some(destination)) => BackupRequest::Copy {
            source,
            destination,
        },
        other => unreachable!("clap requires SOURCE and DESTINATION without --check: {other:?}"),
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
        Invocation::Users(command) => {
            run_accounts(|database, now| spareline_server::run_users(database, &command, now))
        }
        Invocation::Keys(command) => {
            run_accounts(|database, now| spareline_server::run_keys(database, &command, now))
        }
        Invocation::Smoke => run_smoke(&Builder::new_multi_thread().enable_all().build()?),
        Invocation::Healthcheck => run_healthcheck(),
        Invocation::Backup(request) => run_backup(&request),
        Invocation::PolicyCheck { serial, profile } => run_policy_check(
            &Builder::new_current_thread().enable_all().build()?,
            &serial,
            &profile,
        ),
        Invocation::Service => run_service(&Builder::new_multi_thread().enable_all().build()?),
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

/// Opens the database in `DATA_DIR`, logging each migration it applies, runs an account
/// command on it and prints the outcome.
fn run_accounts(
    command: impl FnOnce(&mut Database, Timestamp) -> Result<CommandOutcome, AccountError>,
) -> io::Result<ExitCode> {
    let outcome = Config::data_dir_from_env(environment)
        .map_err(|error| error.to_string())
        .and_then(|data_dir| {
            fs::create_dir_all(&data_dir)
                .map_err(|error| {
                    format!(
                        "cannot create the directory {}: {error}",
                        data_dir.display()
                    )
                })
                .map(|()| data_dir)
        })
        .and_then(|data_dir| {
            let mut applied = Vec::new();
            let opened = Database::open(
                &data_dir.join("spareline.db"),
                Timestamp::now,
                |migration: &AppliedMigration| applied.push(migration.log_line()),
            );
            let mut stderr = io::stderr().lock();
            for line in &applied {
                writeln!(stderr, "{line}").map_err(|error| error.to_string())?;
            }
            opened.map_err(|error| error.to_string())
        })
        .and_then(|mut database| {
            command(&mut database, Timestamp::now()).map_err(|error| error.to_string())
        });
    match outcome {
        Ok(outcome) => {
            let mut stdout = io::stdout().lock();
            for line in &outcome.lines {
                writeln!(stdout, "{line}")?;
            }
            Ok(ExitCode::from(outcome.code))
        }
        Err(message) => {
            writeln!(io::stderr().lock(), "{message}")?;
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Copies or checks a database, printing the result line on stdout, or the failure on stderr.
fn run_backup(request: &BackupRequest) -> io::Result<ExitCode> {
    let outcome = match request {
        BackupRequest::Copy {
            source,
            destination,
        } => spareline_db::backup(source, destination).map(|saved| saved.to_string()),
        BackupRequest::Check { file } => spareline_db::check(file).map(|ok| ok.to_string()),
    };
    match outcome {
        Ok(line) => {
            writeln!(io::stdout().lock(), "{line}")?;
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            writeln!(io::stderr().lock(), "{error}")?;
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Runs `policy-check` against the adb server in `ADB_SERVER`.
fn run_policy_check(runtime: &Runtime, serial: &str, profile: &str) -> io::Result<ExitCode> {
    let outcome = Config::adb_server_from_env(environment)
        .map_err(|error| error.to_string())
        .and_then(|endpoint| {
            let text =
                spareline_server::read_profile(profile).map_err(|error| error.to_string())?;
            let adb = AdbClient::new(endpoint);
            let mut stdout = io::stdout().lock();
            runtime
                .block_on(spareline_server::policy_check(
                    &adb,
                    serial,
                    profile,
                    &text,
                    Timestamp::now(),
                    &mut stdout,
                ))
                .map_err(|error| error.to_string())
        });
    match outcome {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(message) => {
            writeln!(io::stderr().lock(), "{message}")?;
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Runs the service until SIGTERM or SIGINT. Invalid settings, an invalid policy profile, a
/// database that can't be opened or a port that can't be bound print the reason on stderr and
/// exit 1, as spareline-old did.
fn run_service(runtime: &Runtime) -> io::Result<ExitCode> {
    let config = match Config::from_env(environment) {
        Ok(config) => config,
        Err(error) => {
            writeln!(io::stderr().lock(), "{error}")?;
            return Ok(ExitCode::FAILURE);
        }
    };
    let line = spareline_core::version_line();
    let version = line.rsplit(' ').next().unwrap_or(&line).to_owned();
    let log = EventLog::stderr(Clock::system());
    let outcome = runtime.block_on(async move {
        let service = Service::start(config, version, log).await?;
        service.serve(StopSignal::wait()).await
    });
    match outcome {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            writeln!(io::stderr().lock(), "{error}")?;
            Ok(ExitCode::FAILURE)
        }
    }
}
