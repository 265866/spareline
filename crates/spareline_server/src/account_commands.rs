//! The `users` and `keys` commands, which manage who can sign in.

use spareline_core::Timestamp;
use spareline_db::{AccountError, Database, KeyScope, Passwords};

/// A `users` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsersCommand {
    /// `users add NAME`.
    Add {
        /// The new user's name.
        name: String,
    },
    /// `users list`.
    List,
    /// `users reset-password NAME`.
    ResetPassword {
        /// The user's name.
        name: String,
    },
    /// `users remove NAME`.
    Remove {
        /// The user's name.
        name: String,
    },
}

/// A `keys` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeysCommand {
    /// `keys add USER KEY_NAME read|full`.
    Add {
        /// The user the key acts for.
        user: String,
        /// A name to tell the key apart.
        key_name: String,
        /// What the key may do.
        scope: KeyScope,
    },
    /// `keys list`.
    List,
    /// `keys revoke ID`.
    Revoke {
        /// The key's id.
        id: i64,
    },
}

/// What a command printed and its exit code: 0, or 1 when it refused, with the reason printed
/// on standard output, as spareline-old's command line did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    /// The lines for standard output.
    pub lines: Vec<String>,
    /// The exit code.
    pub code: u8,
}

impl CommandOutcome {
    fn done(lines: Vec<String>) -> Self {
        Self { lines, code: 0 }
    }

    fn refused(message: String) -> Self {
        Self {
            lines: vec![message],
            code: 1,
        }
    }
}

/// Runs a `users` command against `database` at `now`.
///
/// # Errors
///
/// Returns a database failure that isn't a refusal, such as a broken file; refusals, such as an
/// unknown name, are an outcome with code 1.
pub fn run_users(
    database: &mut Database,
    command: &UsersCommand,
    now: Timestamp,
) -> Result<CommandOutcome, AccountError> {
    let mut accounts = database.accounts();
    let outcome = match command {
        UsersCommand::List => {
            let users = accounts.list_users()?;
            if users.is_empty() {
                return Ok(CommandOutcome::done(vec!["No users yet.".to_owned()]));
            }
            let lines: Vec<String> = users
                .iter()
                .map(|user| format!("{}\tsince {}", user.name, user.created_at))
                .collect();
            Ok(lines)
        }
        UsersCommand::Add { name } => {
            accounts
                .add_user(name, &Passwords::new(), now)
                .map(|added| {
                    vec![format!(
                        "Added {}. Password (shown once): {}",
                        added.user.name, added.password
                    )]
                })
        }
        UsersCommand::ResetPassword { name } => accounts
            .reset_password(name, &Passwords::new(), now)
            .map(|password| {
                vec![format!(
                    "New password for {name} (shown once; every session ended): {password}"
                )]
            }),
        UsersCommand::Remove { name } => accounts
            .remove_user(name)
            .map(|()| vec![format!("Removed {name} and ended their sessions.")]),
    };
    settle(outcome)
}

/// Runs a `keys` command against `database` at `now`.
///
/// # Errors
///
/// Returns a database failure that isn't a refusal; refusals are an outcome with code 1.
pub fn run_keys(
    database: &mut Database,
    command: &KeysCommand,
    now: Timestamp,
) -> Result<CommandOutcome, AccountError> {
    let mut accounts = database.accounts();
    let outcome = match command {
        KeysCommand::List => {
            let keys = accounts.list_keys()?;
            if keys.is_empty() {
                return Ok(CommandOutcome::done(vec!["No API keys yet.".to_owned()]));
            }
            let lines: Vec<String> = keys
                .iter()
                .map(|key| {
                    let used = match (&key.revoked_at, &key.last_used_at) {
                        (Some(revoked), _) => format!("revoked {revoked}"),
                        (None, Some(used)) => format!("last used {used}"),
                        (None, None) => "last used never".to_owned(),
                    };
                    format!(
                        "{}\t{}\t{}\t{}\t{}\t{used}",
                        key.id,
                        key.prefix,
                        key.user,
                        key.name,
                        key.scope.as_str()
                    )
                })
                .collect();
            Ok(lines)
        }
        KeysCommand::Add {
            user,
            key_name,
            scope,
        } => match accounts.find_user(user)? {
            None => Err(AccountError::NoUser { name: user.clone() }),
            Some(owner) => accounts
                .create_key(owner.id, key_name, *scope, now)
                .map(|added| {
                    vec![format!(
                        "Added {} key {} \"{}\" for {}. Key (shown once): {}",
                        added.api_key.scope.as_str(),
                        added.api_key.id,
                        added.api_key.name,
                        owner.name,
                        added.key
                    )]
                }),
        },
        KeysCommand::Revoke { id } => accounts
            .revoke_key(*id, now)
            .map(|key| vec![format!("Revoked key {} \"{}\".", key.id, key.name)]),
    };
    settle(outcome)
}

/// Turns a refusal into an outcome with code 1, and passes a database failure on.
fn settle(outcome: Result<Vec<String>, AccountError>) -> Result<CommandOutcome, AccountError> {
    match outcome {
        Ok(lines) => Ok(CommandOutcome::done(lines)),
        Err(AccountError::Database(error)) => Err(AccountError::Database(error)),
        Err(refusal) => Ok(CommandOutcome::refused(refusal.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: i64) -> Timestamp {
        Timestamp::from_millis(1_793_000_000_000 + millis)
    }

    fn database() -> Database {
        Database::open_in_memory(|| at(0)).unwrap()
    }

    fn users(database: &mut Database, command: &UsersCommand) -> CommandOutcome {
        run_users(database, command, at(1)).unwrap()
    }

    fn keys(database: &mut Database, command: &KeysCommand) -> CommandOutcome {
        run_keys(database, command, at(2)).unwrap()
    }

    #[test]
    fn users_add_list_reset_and_remove_print_what_spareline_old_printed() {
        let mut database = database();
        assert_eq!(
            users(&mut database, &UsersCommand::List).lines,
            ["No users yet."]
        );
        let added = users(
            &mut database,
            &UsersCommand::Add {
                name: " Alice ".to_owned(),
            },
        );
        assert_eq!(added.code, 0);
        let line = &added.lines[0];
        let password = line
            .strip_prefix("Added Alice. Password (shown once): ")
            .unwrap_or_else(|| panic!("{line}"));
        assert_eq!(password.len(), 20);
        assert_eq!(
            users(&mut database, &UsersCommand::List).lines,
            ["Alice\tsince 2026-10-26T07:33:20.001Z"]
        );
        let reset = users(
            &mut database,
            &UsersCommand::ResetPassword {
                name: "alice".to_owned(),
            },
        );
        assert!(
            reset.lines[0]
                .starts_with("New password for alice (shown once; every session ended): "),
            "{reset:?}"
        );
        assert_eq!(
            users(
                &mut database,
                &UsersCommand::Remove {
                    name: "ALICE".to_owned()
                }
            )
            .lines,
            ["Removed ALICE and ended their sessions."]
        );
    }

    #[test]
    fn a_refusal_prints_its_reason_and_exits_1() {
        let mut database = database();
        let unknown = users(
            &mut database,
            &UsersCommand::Remove {
                name: "bob".to_owned(),
            },
        );
        assert_eq!(unknown, CommandOutcome::refused("no user bob".to_owned()));
        let bad = users(
            &mut database,
            &UsersCommand::Add {
                name: "a b".to_owned(),
            },
        );
        assert_eq!(
            bad.lines,
            ["a user name is 1-64 letters, digits, and . _ @ - characters"]
        );
        users(
            &mut database,
            &UsersCommand::Add {
                name: "carol".to_owned(),
            },
        );
        let twice = users(
            &mut database,
            &UsersCommand::Add {
                name: "Carol".to_owned(),
            },
        );
        assert_eq!(
            twice,
            CommandOutcome::refused("user Carol already exists".to_owned())
        );
    }

    #[test]
    fn keys_add_list_and_revoke_print_what_spareline_old_printed() {
        let mut database = database();
        assert_eq!(
            keys(&mut database, &KeysCommand::List).lines,
            ["No API keys yet."]
        );
        users(
            &mut database,
            &UsersCommand::Add {
                name: "Dave".to_owned(),
            },
        );
        let added = keys(
            &mut database,
            &KeysCommand::Add {
                user: "dave".to_owned(),
                key_name: " deploy ".to_owned(),
                scope: KeyScope::Read,
            },
        );
        let line = &added.lines[0];
        let key = line
            .strip_prefix("Added read key 1 \"deploy\" for Dave. Key (shown once): ")
            .unwrap_or_else(|| panic!("{line}"));
        assert!(key.starts_with("sl_") && key.len() == 46, "{key}");
        let listed = keys(&mut database, &KeysCommand::List);
        assert_eq!(
            listed.lines,
            [format!(
                "1\t{}\tDave\tdeploy\tread\tlast used never",
                &key[..9]
            )]
        );
        assert_eq!(
            keys(&mut database, &KeysCommand::Revoke { id: 1 }).lines,
            ["Revoked key 1 \"deploy\"."]
        );
        let revoked = keys(&mut database, &KeysCommand::List);
        assert!(
            revoked.lines[0].ends_with("\trevoked 2026-10-26T07:33:20.002Z"),
            "{revoked:?}"
        );
        assert_eq!(
            keys(&mut database, &KeysCommand::Revoke { id: 9 }),
            CommandOutcome::refused("no API key 9".to_owned())
        );
        assert_eq!(
            keys(
                &mut database,
                &KeysCommand::Add {
                    user: "nobody".to_owned(),
                    key_name: "x".to_owned(),
                    scope: KeyScope::Full,
                },
            ),
            CommandOutcome::refused("no user nobody".to_owned())
        );
    }
}
