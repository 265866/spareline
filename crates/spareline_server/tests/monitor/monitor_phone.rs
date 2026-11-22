//! A B30 stand-in for the monitor tests.
//!
//! It answers the policy read with the output recorded from the real B30 (`policy-read.txt`)
//! and applies the writes the monitor makes (`settings put`, `svc`, `cmd netpolicy`,
//! `pm uninstall`, the lock screen program) to that recording. Drift, a SIM insertion and data
//! use can't be produced on demand on the real phone, and turning a radio on to cause drift is
//! forbidden, so tests start from the recording and change one thing.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::mem;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use regex::{Captures, Regex};
use spareline_core::monitor::{SIM_LOG_COMMAND, SIM_STATE_COMMAND};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{oneshot, watch};

const POLICY_READ: &str = include_str!("../fixtures/nuu-s6702x/policy-read.txt");
const IDENTITY: &str = include_str!("../fixtures/nuu-s6702x/identity.txt");
const SUBSCRIBER: &str = include_str!("../fixtures/nuu-s6702x/subscriber.txt");
const HEALTH: &str = include_str!("../fixtures/nuu-s6702x/health.txt");

/// The log buffer command the B30 profile's `radioLogBufferMiB: 8` runs.
pub(crate) const LOG_BUFFER_COMMAND: &str = "logcat -b main,system,radio,crash -G 8M";

static APPLIED_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"@@sl:run:[0-9]+\\n'; (.*?) 2>&1; echo "rc=\$\?""#).expect("a valid pattern")
});
static SETTINGS_PUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^settings put (global|secure|system) (\S+) (\S+)$").expect("a valid pattern")
});
static NOTICE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^CLASSPATH=(\S+) app_process / spareline\.LockScreen (\S+) \S+$")
        .expect("a valid pattern")
});
static LOCK_WALLPAPER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(Lock wallpaper state:\n)(?: .*\n)*").expect("a valid pattern"));

/// How the stand-in adb server answers one shell call.
pub(crate) enum Answer {
    /// Standard output and the exit code.
    Output {
        /// Standard output.
        stdout: String,
        /// The exit code.
        exit_code: u8,
    },
    /// Lines streamed until the test drops the phone's stream senders.
    Stream(UnboundedReceiver<String>),
    /// The adb server refuses the service with this message, as for a phone that dropped off
    /// USB.
    Refuse(String),
}

/// A file the monitor pushed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pushed {
    /// Where it went.
    pub(crate) path: String,
    /// How many bytes it had.
    pub(crate) size: usize,
}

/// A read the test holds until it lets it go.
pub(crate) struct HeldRead {
    taken: Option<oneshot::Receiver<()>>,
    release: oneshot::Sender<()>,
}

impl HeldRead {
    /// Waits until the monitor's read has been taken, before it is answered.
    pub(crate) async fn taken(&mut self) {
        if let Some(taken) = self.taken.take() {
            taken.await.expect("the stand-in takes the held read");
        }
    }

    /// Lets the held read answer.
    pub(crate) fn release(self) {
        match self.release.send(()) {
            Ok(()) | Err(()) => {}
        }
    }
}

struct ReadGate {
    taken: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

struct PhoneState {
    order: Vec<String>,
    sections: BTreeMap<String, String>,
    read_script: Option<String>,
    sim_state: String,
    health: String,
    stuck: HashSet<String>,
    fail_apply: Option<String>,
    fail_push: Option<String>,
    files: HashSet<String>,
    applied: Vec<String>,
    reads: usize,
    pushes: Vec<Pushed>,
    calls: Vec<String>,
    streams: Vec<UnboundedSender<String>>,
    stream_opened: watch::Sender<usize>,
    streams_let_go: usize,
    hold: Option<watch::Receiver<bool>>,
    read_gate: Option<ReadGate>,
}

/// The B30 stand-in. Cloning is cheap; every clone is the same phone.
#[derive(Clone)]
pub(crate) struct MonitorPhone {
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "the test and each connection the stand-in adb server serves change the \
                      one phone, each from its own task"
        )
    )]
    state: Arc<Mutex<PhoneState>>,
}

impl MonitorPhone {
    /// The recorded B30.
    pub(crate) fn recorded() -> Self {
        let parsed = spareline_core::split_sections(POLICY_READ);
        Self {
            state: Arc::new(Mutex::new(PhoneState {
                order: parsed
                    .sections()
                    .iter()
                    .map(|section| section.name.clone())
                    .collect(),
                sections: parsed
                    .sections()
                    .iter()
                    .map(|section| (section.name.clone(), section.text.clone()))
                    .collect(),
                read_script: None,
                sim_state: "LOADED,ABSENT".to_owned(),
                health: HEALTH.to_owned(),
                stuck: HashSet::new(),
                fail_apply: None,
                fail_push: None,
                files: HashSet::new(),
                applied: Vec::new(),
                reads: 0,
                pushes: Vec::new(),
                calls: Vec::new(),
                streams: Vec::new(),
                stream_opened: watch::channel(0).0,
                streams_let_go: 0,
                hold: None,
                read_gate: None,
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, PhoneState> {
        self.state
            .lock()
            .expect("no test task panicked while holding the phone")
    }

    /// Answers the policy read `script` with the recording.
    pub(crate) fn set_read_script(&self, script: &str) {
        self.lock().read_script = Some(script.to_owned());
    }

    /// Replaces a section of the recorded read.
    pub(crate) fn set_section(&self, name: &str, body: &str) {
        self.lock().set_section(name, body);
    }

    /// Changes one setting the way a person tapping in Settings would.
    pub(crate) fn set(&self, namespace: &str, key: &str, value: &str) {
        self.lock().set(namespace, key, value);
    }

    /// A setting's value, or `None` when it's unset.
    pub(crate) fn get(&self, namespace: &str, key: &str) -> Option<String> {
        self.lock().get(namespace, key)
    }

    /// Makes a setting, such as `global:bluetooth_on`, or `logd` for the log buffer, ignore the
    /// monitor's writes, like a radio that won't turn off.
    pub(crate) fn stick(&self, what: &str) {
        self.lock().stuck.insert(what.to_owned());
    }

    /// Lets a stuck setting take writes again.
    pub(crate) fn unstick(&self, what: &str) {
        self.lock().stuck.remove(what);
    }

    /// Makes apply scripts fail, with the adb server refusing them with `message`, like a phone
    /// that drops off USB; `None` lets them run again.
    pub(crate) fn fail_apply(&self, message: Option<&str>) {
        self.lock().fail_apply = message.map(str::to_owned);
    }

    /// Makes pushes fail with `message`, like a full `/data/local/tmp`.
    pub(crate) fn fail_push(&self, message: Option<&str>) {
        self.lock().fail_push = message.map(str::to_owned);
    }

    /// Leaves a file on the phone, as a run killed between its push and its start does.
    pub(crate) fn leave_file(&self, path: &str) {
        self.lock().files.insert(path.to_owned());
    }

    /// The health probe's answer from now on.
    pub(crate) fn set_health(&self, text: &str) {
        text.clone_into(&mut self.lock().health);
    }

    /// The radio buffer the `log_buffer` rule accepts.
    pub(crate) fn big_radio_buffer(&self) {
        self.lock().big_radio_buffer();
    }

    /// The lock screen's own wallpaper named `name`, or with `None` the home screen's, as
    /// `dumpsys wallpaper` prints them.
    pub(crate) fn set_lock_wallpaper(&self, name: Option<&str>) {
        self.lock().set_lock_wallpaper(name);
    }

    /// The recorded netstats plus one app's traffic on the metered internet APN of sub 1, in
    /// the per-uid section's recorded format: mobile data stays off on the real phone, so
    /// internet traffic can't be recorded.
    pub(crate) fn add_metered_traffic(&self, bytes: u64, uid: i64) {
        let mut state = self.lock();
        let now: i64 = state
            .section("clock")
            .parse()
            .expect("the recording has the phone's clock");
        let rows = format!(
            "  ident=[{{type=0, ratType=13, subscriberId=310260..., metered=true, \
             defaultNetwork=true, oemManaged=OEM_NONE, subId=1}}] uid={uid} set=DEFAULT tag=0x0\n    \
             NetworkStatsHistory: bucketDuration=7200\n      st={} rb={bytes} rp=900 tb=0 tp=0 \
             op=0\nInterfaceMapUpdater:",
            now - 3600
        );
        let netstats = state
            .section("netstats")
            .replacen("InterfaceMapUpdater:", &rows, 1);
        state.set_section("netstats", &netstats);
    }

    /// A SIM appearing in slot 1 as `sub_id`, in the recorded isub line format. A SIM can't be
    /// inserted on demand, so the stand-in changes isub and the SIM state, and the test feeds
    /// the log line the RIL would print.
    pub(crate) fn insert_sim(&self, sub_id: i64) {
        let mut state = self.lock();
        let isub = state.section("isub");
        let line = isub
            .split('\n')
            .find(|line| line.contains("SubscriptionInfoInternal: id=1 "))
            .expect("the recording has sub 1");
        let second = line
            .replacen("id=1 ", &format!("id={sub_id} "), 1)
            .replacen("simSlotIndex=0", "simSlotIndex=1", 1)
            .replacen(
                "enabledMobileDataPolicies=2",
                "enabledMobileDataPolicies=",
                1,
            );
        let isub = isub.replacen(
            "Active subscriptions:\n",
            &format!("Active subscriptions:\n{second}\n"),
            1,
        );
        state.set_section("isub", &isub);
        "LOADED,LOADED".clone_into(&mut state.sim_state);
    }

    /// The SIM in slot 0 taken out.
    pub(crate) fn remove_sim(&self) {
        let mut state = self.lock();
        let isub: Vec<String> = state
            .section("isub")
            .split('\n')
            .filter(|line| {
                !(line.contains("SubscriptionInfoInternal: id=1 ")
                    && line.contains("simSlotIndex=0"))
            })
            .map(str::to_owned)
            .collect();
        state.set_section("isub", &isub.join("\n"));
        "ABSENT,ABSENT".clone_into(&mut state.sim_state);
    }

    /// Sends `line` on every open radio log stream, as the RIL would print it.
    pub(crate) fn log_line(&self, line: &str) {
        for stream in &self.lock().streams {
            match stream.send(format!("{line}\n")) {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Ends every open radio log stream, as logcat exiting would.
    pub(crate) fn end_log_streams(&self) {
        self.lock().streams.clear();
    }

    /// How many ended radio log streams the monitor has closed its end of.
    pub(crate) fn log_streams_let_go(&self) -> usize {
        self.lock().streams_let_go
    }

    /// Counts an ended radio log stream whose reader closed its end.
    pub(crate) fn log_stream_let_go(&self) {
        self.lock().streams_let_go += 1;
    }

    /// How many radio log streams were opened.
    pub(crate) fn log_streams_opened(&self) -> watch::Receiver<usize> {
        self.lock().stream_opened.subscribe()
    }

    /// Makes every shell call wait until the returned sender sends `false`, like a phone that
    /// is slow to answer.
    pub(crate) fn hold(&self) -> watch::Sender<bool> {
        let (sender, receiver) = watch::channel(true);
        self.lock().hold = Some(receiver);
        sender
    }

    /// Lets calls made from now on answer at once, while the calls already held keep waiting.
    pub(crate) fn let_new_calls_through(&self) {
        self.lock().hold = None;
    }

    /// Holds the next policy read after it is taken and before it is answered, to order
    /// something between the two.
    pub(crate) fn hold_next_read(&self) -> HeldRead {
        let (taken_sender, taken) = oneshot::channel();
        let (release, release_receiver) = oneshot::channel();
        self.lock().read_gate = Some(ReadGate {
            taken: taken_sender,
            release: release_receiver,
        });
        HeldRead {
            taken: Some(taken),
            release,
        }
    }

    /// Every command an apply script ran, in order, taken out of the record.
    pub(crate) fn take_applied(&self) -> Vec<String> {
        mem::take(&mut self.lock().applied)
    }

    /// Every command an apply script ran, in order.
    pub(crate) fn applied(&self) -> Vec<String> {
        self.lock().applied.clone()
    }

    /// How many policy reads were answered, taken out of the record.
    pub(crate) fn take_reads(&self) -> usize {
        mem::take(&mut self.lock().reads)
    }

    /// Every file pushed.
    pub(crate) fn pushes(&self) -> Vec<Pushed> {
        self.lock().pushes.clone()
    }

    /// Every shell command, in order.
    pub(crate) fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    /// How many shell calls ran `command`.
    pub(crate) fn count(&self, command: &str) -> usize {
        self.lock()
            .calls
            .iter()
            .filter(|call| call.as_str() == command)
            .count()
    }

    /// Answers one shell call.
    pub(crate) async fn answer(&self, _serial: &str, command: &str) -> Answer {
        let hold = {
            let mut state = self.lock();
            state.calls.push(command.to_owned());
            state.hold.clone()
        };
        if let Some(mut hold) = hold {
            match hold.wait_for(|held| !held).await {
                Ok(_) | Err(_) => {}
            }
        }
        let read = {
            let mut state = self.lock();
            if state.read_script.as_deref() == Some(command) {
                state.reads += 1;
                Some((state.read(), state.read_gate.take()))
            } else {
                None
            }
        };
        if let Some((stdout, gate)) = read {
            if let Some(gate) = gate {
                match gate.taken.send(()) {
                    Ok(()) | Err(()) => {}
                }
                match gate.release.await {
                    Ok(()) | Err(_) => {}
                }
            }
            return Answer::Output {
                stdout,
                exit_code: 0,
            };
        }
        let mut state = self.lock();
        let output = |stdout: String| Answer::Output {
            stdout,
            exit_code: 0,
        };
        if command == "dumpsys isub" {
            return output(state.section("isub"));
        }
        if command == SIM_STATE_COMMAND {
            return output(format!("{}\n", state.sim_state));
        }
        if command == SIM_LOG_COMMAND {
            let (sender, receiver) = mpsc::unbounded_channel();
            state.streams.push(sender);
            state.stream_opened.send_modify(|opened| *opened += 1);
            return Answer::Stream(receiver);
        }
        if command.contains("@@sl:run:") {
            if let Some(message) = &state.fail_apply {
                return Answer::Refuse(message.clone());
            }
            return output(state.run(command));
        }
        if command == spareline_core::identity_command() {
            return output(IDENTITY.to_owned());
        }
        if command.starts_with("printf '\\n@@sl:imei:0\\n'") {
            return output(SUBSCRIBER.to_owned());
        }
        if command == spareline_core::health_command() {
            return output(state.health.clone());
        }
        Answer::Output {
            stdout: String::new(),
            exit_code: 127,
        }
    }

    /// Takes a pushed file.
    ///
    /// # Errors
    ///
    /// The message a full phone would give, while [`Self::fail_push`] is set.
    pub(crate) fn receive(&self, path: &str, data: &[u8]) -> Result<(), String> {
        let mut state = self.lock();
        if let Some(message) = &state.fail_push {
            return Err(message.clone());
        }
        state.pushes.push(Pushed {
            path: path.to_owned(),
            size: data.len(),
        });
        state.files.insert(path.to_owned());
        Ok(())
    }
}

impl PhoneState {
    fn section(&self, name: &str) -> String {
        self.sections.get(name).cloned().unwrap_or_default()
    }

    fn set_section(&mut self, name: &str, body: &str) {
        self.sections.insert(name.to_owned(), body.to_owned());
    }

    fn set(&mut self, namespace: &str, key: &str, value: &str) {
        let name = format!("settings:{namespace}");
        let prefix = format!("{key}=");
        let mut lines: Vec<String> = self
            .section(&name)
            .split('\n')
            .filter(|line| !line.is_empty() && !line.starts_with(&prefix))
            .map(str::to_owned)
            .collect();
        lines.push(format!("{key}={value}"));
        self.set_section(&name, &lines.join("\n"));
    }

    fn get(&self, namespace: &str, key: &str) -> Option<String> {
        let prefix = format!("{key}=");
        self.section(&format!("settings:{namespace}"))
            .split('\n')
            .find_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
    }

    fn write(&mut self, namespace: &str, key: &str, value: &str) {
        if !self.stuck.contains(&format!("{namespace}:{key}")) {
            self.set(namespace, key, value);
        }
    }

    fn big_radio_buffer(&mut self) {
        self.set_section(
            "radiobuf",
            "radio: ring buffer is 8 MiB (252 KiB consumed, 1 MiB readable), max entry is 5120 \
             B, max payload is 4068 B",
        );
    }

    fn set_lock_wallpaper(&mut self, name: Option<&str>) {
        let entry = name.map_or_else(String::new, |name| {
            format!(
                " User 0: id=10: mWhich=2: mSystemWasBoth=true\n  mName={name}\n  \
                 mWallpaperComponent=null\n"
            )
        });
        let wallpaper = format!("{}\n", self.section("wallpaper"));
        let replaced = LOCK_WALLPAPER.replace(&wallpaper, |captures: &Captures<'_>| {
            format!("{}{entry}", &captures[1])
        });
        let replaced = replaced.trim_end().to_owned();
        self.set_section("wallpaper", &replaced);
    }

    fn read(&self) -> String {
        let mut output = String::new();
        for name in &self.order {
            write!(output, "\n@@sl:{name}\n{}\n", self.section(name))
                .expect("writing to a String never fails");
        }
        output
    }

    fn run(&mut self, script: &str) -> String {
        let commands: Vec<String> = APPLIED_COMMAND
            .captures_iter(script)
            .map(|captures| captures[1].to_owned())
            .collect();
        let mut out = String::new();
        for (index, command) in commands.iter().enumerate() {
            self.applied.push(command.clone());
            let rc = self.effect(command);
            write!(out, "\n@@sl:run:{index}\nrc={rc}\n").expect("writing to a String never fails");
        }
        out
    }

    fn effect(&mut self, command: &str) -> i32 {
        if let Some(put) = SETTINGS_PUT.captures(command) {
            self.write(&put[1], &put[2], &put[3]);
            return 0;
        }
        match command {
            "svc data disable" => {
                self.write("global", "mobile_data", "0");
                if let Some(sub) =
                    spareline_core::parse_isub(&self.section("isub")).default_data_sub_id
                    && sub >= 0
                {
                    self.write("global", &format!("mobile_data{sub}"), "0");
                }
                return 0;
            }
            "svc wifi disable" => {
                self.write("global", "wifi_on", "0");
                return 0;
            }
            "cmd bluetooth_manager disable" => {
                self.write("global", "bluetooth_on", "0");
                return 0;
            }
            "cmd bluetooth_manager wait-for-state:STATE_OFF" => return 0,
            "cmd netpolicy set restrict-background true" => {
                self.set_section("datasaver", "Restrict background status: enabled");
                return 0;
            }
            LOG_BUFFER_COMMAND => {
                if !self.stuck.contains("logd") {
                    self.big_radio_buffer();
                }
                return 0;
            }
            _ => {}
        }
        if let Some(app) = command.strip_prefix("pm uninstall --user 0 ") {
            let remaining: Vec<String> = self
                .section("bloat")
                .split('\n')
                .filter(|line| *line != format!("package:{app}"))
                .map(str::to_owned)
                .collect();
            self.set_section("bloat", &remaining.join("\n"));
            return 0;
        }
        if command.starts_with("cmd role add-role-holder ") {
            return 0;
        }
        if let Some(notice) = NOTICE.captures(command) {
            if !self.files.remove(&notice[1]) {
                return 134;
            }
            self.set_lock_wallpaper(Some(&notice[2]));
            return 0;
        }
        panic!("the stand-in has no effect recorded for {command:?}");
    }
}
