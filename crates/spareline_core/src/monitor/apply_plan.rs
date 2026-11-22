//! Running enforce commands on a phone: the radio guard, the file pushes, and the one shell
//! script that runs the commands and reports each one's exit code.

use std::sync::LazyLock;

use regex::Regex;

use crate::monitor::js_regex;
use crate::monitor::rule::PhoneFile;
use crate::{android, monitor, text};

static EXIT_CODE: LazyLock<Regex> =
    LazyLock::new(|| js_regex::pattern(r"(?:^|\n)rc=([0-9]+){s}*$"));

/// A command the radio guard refused; nothing in its batch ran.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("refusing to run \"{command}\" ({hazard})")]
#[must_use]
pub struct RefusedCommand {
    /// The command.
    pub command: String,
    /// Why it could turn a radio or data on.
    pub hazard: String,
}

/// Files that some of a batch's commands need on the phone first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// The files, pushed in order with mode 0644.
    pub files: Vec<PhoneFile>,
    /// The commands that need them.
    pub commands: Vec<String>,
}

/// How one command went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    /// The command.
    pub command: String,
    /// Its exit code, or `None` when it didn't run or its exit code went missing.
    pub exit_code: Option<i64>,
    /// Its combined output, trimmed, or why it didn't run.
    pub output: String,
}

/// One batch of commands for one phone, from the guard to the results.
///
/// The server builds it with [`ApplyPlan::try_new`], pushes each upload's files and reports a failed
/// push with [`ApplyPlan::push_failed`], runs [`ApplyPlan::script`] as one shell call when there
/// is one, and reads the results with [`ApplyPlan::results`]. A failed push skips only the
/// commands that need its file; the radio and data commands in the same batch still run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyPlan {
    commands: Vec<String>,
    uploads: Vec<Upload>,
    skipped: Vec<SkippedCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SkippedCommand {
    command: String,
    reason: String,
}

impl ApplyPlan {
    /// A batch of `commands`, in the order they run, with the files some of them need.
    ///
    /// # Errors
    ///
    /// [`RefusedCommand`] for the first command the radio guard refuses, before anything is
    /// pushed or run.
    pub fn try_new(commands: Vec<String>, uploads: Vec<Upload>) -> Result<Self, RefusedCommand> {
        for command in &commands {
            if let Some(hazard) = monitor::radio_hazard(command) {
                return Err(RefusedCommand {
                    command: command.clone(),
                    hazard,
                });
            }
        }
        Ok(Self {
            commands,
            uploads,
            skipped: Vec::new(),
        })
    }

    /// The commands, in the order they run.
    #[must_use]
    pub fn commands(&self) -> &[String] {
        &self.commands
    }

    /// The files to push before the script runs. An empty batch pushes nothing.
    #[must_use]
    pub fn uploads(&self) -> &[Upload] {
        if self.commands.is_empty() {
            return &[];
        }
        &self.uploads
    }

    /// Records that pushing `path` for the upload at `upload` failed with `error`: that upload's
    /// commands won't run, and the server pushes none of its remaining files.
    pub fn push_failed(&mut self, upload: usize, path: &str, error: &str) {
        let Some(upload) = self.uploads.get(upload) else {
            return;
        };
        let reason = format!("not run: pushing {path} failed: {error}");
        for command in &upload.commands {
            self.skipped.push(SkippedCommand {
                command: command.clone(),
                reason: reason.clone(),
            });
        }
    }

    /// The commands that will run, skipped ones left out.
    fn runnable(&self) -> Vec<&str> {
        self.commands
            .iter()
            .filter(|command| self.skip_reason(command).is_none())
            .map(String::as_str)
            .collect()
    }

    fn skip_reason(&self, command: &str) -> Option<&str> {
        self.skipped
            .iter()
            .find(|skipped| skipped.command == command)
            .map(|skipped| skipped.reason.as_str())
    }

    /// The one shell script that runs every command that isn't skipped, or `None` when none is
    /// left. Each command gets a section, `printf '\n@@sl:run:<i>\n'; <cmd> 2>&1; echo "rc=$?"`,
    /// and the script ends with `; sleep 1` when a command starts with `svc `, since `svc`
    /// returns before the radio settles and the re-check should read the result. Run it with a
    /// 120 s timeout.
    #[must_use]
    pub fn script(&self) -> Option<String> {
        let runnable = self.runnable();
        if runnable.is_empty() {
            return None;
        }
        let parts: Vec<String> = runnable
            .iter()
            .enumerate()
            .map(|(index, command)| {
                format!(
                    "{}; {command} 2>&1; echo \"rc=$?\"",
                    android::section_marker(&format!("run:{index}"))
                )
            })
            .collect();
        let mut script = parts.join("; ");
        if runnable.iter().any(|command| command.starts_with("svc ")) {
            script.push_str("; sleep 1");
        }
        Some(script)
    }

    /// Each command's result, in command order, from the script's standard output, or from no
    /// output when [`Self::script`] was `None`. A command listed twice takes the first run's
    /// result, as the old monitor matched results by command text.
    #[must_use]
    pub fn results(&self, stdout: &str) -> Vec<CommandResult> {
        let sections = android::split_sections(stdout);
        let ran: Vec<CommandResult> = self
            .runnable()
            .iter()
            .enumerate()
            .map(|(index, command)| {
                let body = sections.get(&format!("run:{index}")).unwrap_or_default();
                let exit = EXIT_CODE.captures(body);
                CommandResult {
                    command: (*command).to_owned(),
                    exit_code: exit
                        .as_ref()
                        .map(|exit| exit[1].parse().unwrap_or(i64::MAX)),
                    output: text::trim(&EXIT_CODE.replace(body, "")).to_owned(),
                }
            })
            .collect();
        self.commands
            .iter()
            .map(|command| {
                ran.iter()
                    .find(|result| result.command == *command)
                    .cloned()
                    .unwrap_or_else(|| CommandResult {
                        command: command.clone(),
                        exit_code: None,
                        output: self.skip_reason(command).unwrap_or_default().to_owned(),
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{LOCK_SCREEN_DEX_PHONE_PATH, LOCK_SCREEN_DEX_SHA256};

    fn commands(list: &[&str]) -> Vec<String> {
        list.iter().map(|&command| command.to_owned()).collect()
    }

    fn result(command: &str, exit_code: Option<i64>, output: &str) -> CommandResult {
        CommandResult {
            command: command.to_owned(),
            exit_code,
            output: output.to_owned(),
        }
    }

    #[test]
    fn the_script_marks_each_command_and_waits_after_svc() {
        let plan = ApplyPlan::try_new(
            commands(&[
                "svc wifi disable",
                "settings put global wifi_scan_always_enabled 0",
            ]),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            plan.script().unwrap(),
            "printf '\\n@@sl:run:0\\n'; svc wifi disable 2>&1; echo \"rc=$?\"; \
             printf '\\n@@sl:run:1\\n'; settings put global wifi_scan_always_enabled 0 2>&1; \
             echo \"rc=$?\"; sleep 1"
        );
        let buffer = ApplyPlan::try_new(
            commands(&["logcat -b main,system,radio,crash -G 8M"]),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            buffer.script().unwrap(),
            "printf '\\n@@sl:run:0\\n'; logcat -b main,system,radio,crash -G 8M 2>&1; echo \"rc=$?\""
        );
        assert!(
            !ApplyPlan::try_new(commands(&["cmd svc x"]), Vec::new())
                .unwrap()
                .script()
                .unwrap()
                .ends_with("sleep 1")
        );
    }

    #[test]
    fn the_output_gives_each_commands_exit_code_and_trimmed_output() {
        let plan = ApplyPlan::try_new(
            commands(&[
                "svc data disable",
                "pm uninstall --user 0 a.b",
                "settings put global mobile_data 0",
            ]),
            Vec::new(),
        )
        .unwrap();
        let stdout =
            "\n@@sl:run:0\nrc=0\n\n@@sl:run:1\nFailure [not installed for 0]\nrc=1\n\n@@sl:run:2\n";
        assert_eq!(
            plan.results(stdout),
            [
                result("svc data disable", Some(0), ""),
                result(
                    "pm uninstall --user 0 a.b",
                    Some(1),
                    "Failure [not installed for 0]"
                ),
                result("settings put global mobile_data 0", None, ""),
            ]
        );
        assert_eq!(
            plan.results("\n@@sl:run:0\nrc=1\nrc=0  \n")[0],
            result("svc data disable", Some(0), "rc=1"),
            "only a final rc= line is the exit code"
        );
    }

    #[test]
    fn the_radio_guard_refuses_the_whole_batch() {
        let error = ApplyPlan::try_new(
            commands(&["svc data disable", "svc wifi enable"]),
            Vec::new(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "refusing to run \"svc wifi enable\" (svc ... enable)"
        );
    }

    #[test]
    fn a_failed_push_skips_only_the_commands_that_need_the_file() {
        let notice = "CLASSPATH=/data/local/tmp/spareline-lockscreen.dex app_process / x";
        let mut plan = ApplyPlan::try_new(
            commands(&["svc wifi disable", notice]),
            vec![Upload {
                files: vec![PhoneFile {
                    path: LOCK_SCREEN_DEX_PHONE_PATH,
                    sha256: LOCK_SCREEN_DEX_SHA256,
                }],
                commands: commands(&[notice]),
            }],
        )
        .unwrap();
        assert_eq!(plan.uploads().len(), 1);
        plan.push_failed(
            0,
            LOCK_SCREEN_DEX_PHONE_PATH,
            "push: No space left on device",
        );
        assert_eq!(
            plan.script().unwrap(),
            "printf '\\n@@sl:run:0\\n'; svc wifi disable 2>&1; echo \"rc=$?\"; sleep 1"
        );
        assert_eq!(
            plan.results("\n@@sl:run:0\nrc=0\n"),
            [
                result("svc wifi disable", Some(0), ""),
                result(
                    notice,
                    None,
                    "not run: pushing /data/local/tmp/spareline-lockscreen.dex failed: push: No \
                     space left on device"
                ),
            ]
        );
    }

    #[test]
    fn nothing_to_run_means_no_script_and_no_pushes() {
        let empty = ApplyPlan::try_new(
            Vec::new(),
            vec![Upload {
                files: Vec::new(),
                commands: Vec::new(),
            }],
        )
        .unwrap();
        assert_eq!(empty.script(), None);
        assert_eq!(empty.uploads(), []);
        assert_eq!(empty.results(""), []);
    }

    #[test]
    fn a_repeated_command_takes_the_first_runs_result() {
        let plan = ApplyPlan::try_new(
            commands(&[
                "settings put global mobile_data 0",
                "settings put global mobile_data 0",
            ]),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            plan.results("\n@@sl:run:0\nrc=0\n\n@@sl:run:1\nrc=255\n"),
            [
                result("settings put global mobile_data 0", Some(0), ""),
                result("settings put global mobile_data 0", Some(0), ""),
            ]
        );
    }
}
