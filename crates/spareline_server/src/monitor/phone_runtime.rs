//! What the monitor keeps for one phone between its steps.

use std::collections::BTreeMap;
use std::sync::Arc;

use spareline_core::monitor::{
    Busy, CheckReport, DataOffFailure, PhonePolicy, PolicyAlertSource, ProvisionReport,
};
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

/// One phone's monitor state, kept from the first time it came online this run.
#[derive(Debug)]
pub(crate) struct PhoneRuntime {
    /// The phone's serial.
    pub(crate) serial: String,
    /// Held for the length of one step, so the phone's steps run one at a time, in the order
    /// they were queued: tokio's mutex hands itself over first come, first served.
    #[cfg_attr(
        dylint_lib = "trellis_lints",
        expect(
            shared_ownership,
            reason = "each queued step holds the phone's queue while it waits, from a task of \
                      its own that outlives the monitor's state lock"
        )
    )]
    pub(crate) queue: Arc<Mutex<()>>,
    /// What the running step is.
    pub(crate) busy: Option<Busy>,
    /// How many steps wait for their turn in the queue.
    pub(crate) waiting: usize,
    /// The last drift check.
    pub(crate) check: Option<CheckReport>,
    /// The last Set up phone run, from this run or, after a restart, from its event.
    pub(crate) provision: Option<ProvisionReport>,
    /// Whether the last Set up phone run has been looked up in the database.
    pub(crate) provision_loaded: bool,
    /// Which sub is in each slot, as last seen; `None` until the first read after the phone
    /// came online.
    pub(crate) slots: Option<BTreeMap<u32, i64>>,
    /// The SIM watch's tasks while the phone is online: the radio log stream and the SIM state
    /// poll.
    pub(crate) watch: Vec<AbortHandle>,
    /// The debounce timer of the last SIM signal.
    pub(crate) sim_timer: Option<AbortHandle>,
    /// The subs over the data threshold at the last check.
    pub(crate) alarmed: Vec<i64>,
    /// A data-off on a SIM signal runs, outside the queue.
    pub(crate) data_off_running: bool,
    /// A SIM signal arrived while that data-off ran, so another follows it.
    pub(crate) data_off_again: bool,
    /// Turning data off failed. Only a check that started after the failure and finds mobile
    /// data off clears it.
    pub(crate) data_off_failure: Option<SequencedFailure>,
}

/// A data-off failure, numbered on the monitor's sequence so a check can tell whether it
/// started after it.
#[derive(Clone, Debug)]
pub(crate) struct SequencedFailure {
    /// The failure.
    pub(crate) failure: DataOffFailure,
    /// Its number.
    pub(crate) sequence: u64,
}

impl PhoneRuntime {
    /// The state of a phone seen for the first time.
    pub(crate) fn new(serial: &str) -> Self {
        Self {
            serial: serial.to_owned(),
            queue: Arc::default(),
            busy: None,
            waiting: 0,
            check: None,
            provision: None,
            provision_loaded: false,
            slots: None,
            watch: Vec::new(),
            sim_timer: None,
            alarmed: Vec::new(),
            data_off_running: false,
            data_off_again: false,
            data_off_failure: None,
        }
    }

    /// The phone's policy state, as `GET /api/v1/status` serves it.
    pub(crate) fn view(&self) -> PhonePolicy {
        PhonePolicy {
            serial: self.serial.clone(),
            busy: self.busy,
            check: self.check.clone(),
            provision: self.provision.clone(),
            data_alarm: !self.alarmed.is_empty(),
        }
    }

    /// The phone's state as the policy alerts read it.
    pub(crate) fn alert_source(&self, online: bool) -> PolicyAlertSource<'_> {
        PolicyAlertSource {
            serial: &self.serial,
            online,
            data_off_failure: self
                .data_off_failure
                .as_ref()
                .map(|sequenced| &sequenced.failure),
            check: self.check.as_ref(),
        }
    }

    /// Stops the SIM watch and its pending debounce.
    pub(crate) fn stop_watch(&mut self) {
        for task in self.watch.drain(..) {
            task.abort();
        }
        if let Some(timer) = self.sim_timer.take() {
            timer.abort();
        }
    }
}
