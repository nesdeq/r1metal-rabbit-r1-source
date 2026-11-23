//! The bring-up of the data partition: whether a filesystem is r1metal's, its steps, shared state and log lines.

use super::safe::{Order, Refusal, Request};
use super::{LABEL, MOUNT_POINT};
use crate::base::handoff::Ticket;
use crate::base::text::to_gib_text;
use crate::fs::f2fs::{self, Identity, UUID_SIZE};
use alloc::format;
use alloc::string::String;

pub const NAMELESS_NODE: &str = "the partition's node has no name";
pub const UNREADABLE_NODE: &str = "the partition's node is not UTF-8";
pub const LOST_LABEL: &str = "the mounted partition lost its label";
/// The log line of a filesystem that may encrypt.
pub const ENCRYPTING_LINE: &str = "data: the filesystem may encrypt a directory from now on";

/// The identity of the filesystem a superblock holds, where its label is r1metal's.
pub fn identify(block: &[u8]) -> Option<Identity> {
    f2fs::identify(block).filter(|identity| identity.label == LABEL)
}

/// The log line of a partition without an r1metal filesystem.
pub fn to_foreign_line(node: &str) -> String {
    format!("data: {node} holds no r1metal filesystem, formatting it")
}

/// The log line of a finished format.
pub fn to_formatted_line(bytes: u64, ms: u128, discard_ms: u128) -> String {
    format!("data: formatted {bytes} bytes in {ms} ms, {discard_ms} ms of it the discard")
}

/// A step of the partition's bring-up.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Progress {
    // Looking for the partition, or mounting it.
    Looking,
    // No partition has appeared yet.
    Waiting,
    Formatting,
    // Mounted with the private directory shut; `refused` is the last refused passphrase by its number.
    Shut { is_made: bool, refused: Option<(u32, Refusal)> },
    // Mounted with the safe open, while what an earlier firmware kept plain is moved under the safe's key.
    Encrypting,
    // Mounted, holding that many bytes.
    Mounted(u64),
    Failed(String),
}

impl Progress {
    /// A mounted step; no capacity counts as zero bytes.
    pub fn from_capacity(capacity: Option<u64>) -> Progress {
        Progress::Mounted(capacity.unwrap_or(0))
    }

    /// The log line of the partition mounted from a node.
    pub fn to_mounted_line(&self, node: &str) -> String {
        format!("data: {node} at {MOUNT_POINT}, {}", self.to_word())
    }

    /// True while the partition is being brought up right now.
    pub const fn is_underway(&self) -> bool {
        matches!(self, Progress::Looking | Progress::Formatting | Progress::Shut { .. } | Progress::Encrypting)
    }

    /// True while the partition's thread works on its own, with nobody to wait for: the device does not suspend then.
    pub const fn is_at_work(&self) -> bool {
        matches!(self, Progress::Formatting | Progress::Encrypting)
    }

    // True while it is neither mounted with its safe open nor given up.
    const fn is_undecided(&self) -> bool {
        self.is_underway() || matches!(self, Progress::Waiting)
    }

    /// What a waiter for the bring-up's end is answered, whether it is mounted; none while it is being brought up.
    pub const fn settled(&self) -> Option<bool> {
        if self.is_underway() { None } else { Some(self.is_mounted()) }
    }

    /// What a waiter for a partition however late is answered, whether it is mounted; none while that is undecided.
    pub const fn decided(&self) -> Option<bool> {
        if self.is_undecided() { None } else { Some(self.is_mounted()) }
    }

    pub const fn is_mounted(&self) -> bool {
        matches!(self, Progress::Mounted(_))
    }

    /// True once files kept plain began to move under the safe's key: what they leave is handed back to the medium.
    pub const fn is_encrypting(&self) -> bool {
        matches!(self, Progress::Encrypting)
    }

    /// The mount point, once it is mounted.
    pub const fn root(&self) -> Option<&'static str> {
        if self.is_mounted() { Some(MOUNT_POINT) } else { None }
    }

    /// The step's text on the debug screen and in the log.
    pub fn to_word(&self) -> String {
        match self {
            Progress::Looking => "looking for the partition".into(),
            Progress::Waiting => "waiting for the partition".into(),
            Progress::Formatting => "formatting".into(),
            Progress::Shut { is_made: true, .. } => "the safe is shut".into(),
            Progress::Shut { is_made: false, .. } => "the safe is not made".into(),
            Progress::Encrypting => "the safe takes in what was kept plain".into(),
            Progress::Mounted(bytes) => format!("f2fs {} GiB", to_gib_text(*bytes)),
            Progress::Failed(reason) => reason.clone(),
        }
    }
}

/// The partition thread's report: its step, and the outcome of the last passphrase change by its number.
#[derive(Default, PartialEq, Eq, Debug)]
pub struct Report {
    pub progress: Option<Progress>,
    pub change: Option<(u32, Result<(), Refusal>)>,
}

/// The state the partition's thread, the parts that keep files and the main loop share.
pub struct State {
    pub has_new_device: bool,
    pub order: Option<(u32, Order)>,
    pub report: Report,
    progress: Progress,
    id: Option<[u8; UUID_SIZE]>,
    ticket: Ticket,
}

impl Default for State {
    fn default() -> State {
        State {
            has_new_device: false,
            order: None,
            report: Report::default(),
            progress: Progress::Looking,
            id: None,
            ticket: Ticket::default(),
        }
    }
}

impl State {
    pub const fn progress(&self) -> &Progress {
        &self.progress
    }

    /// The id of the filesystem, once it is known.
    pub const fn id(&self) -> Option<[u8; UUID_SIZE]> {
        self.id
    }

    pub const fn set_id(&mut self, id: [u8; UUID_SIZE]) {
        self.id = Some(id);
    }

    /// Hands the safe a request once per number; true if it was new.
    pub fn ask_safe(&mut self, request: Option<(u32, Request)>) -> bool {
        let Some((number, request)) = request.filter(|(number, _)| self.ticket.want(Some(*number))) else {
            return false;
        };
        self.order = Some((number, Order::from(request)));
        true
    }

    /// Sets a step without a report to the main loop.
    pub fn enter(&mut self, progress: Progress) {
        self.progress = progress;
    }

    /// Sets a step and reports it to the main loop.
    pub fn report_progress(&mut self, progress: Progress) {
        self.report.progress = Some(progress.clone());
        self.progress = progress;
    }

    /// Reports `Encrypting` as the first plain root begins to move; true if that is new.
    pub fn begin_encrypting(&mut self) -> bool {
        let is_new = !self.progress.is_encrypting();
        if is_new {
            self.report_progress(Progress::Encrypting);
        }
        is_new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_by_label() {
        let request = f2fs::Request {
            bytes: 1 << 30,
            start_sector: 0,
            label: LABEL,
            uuid: [7; 16],
            checkpoint: 1,
            kernel: "",
            time: 0,
            is_trimmed: true,
        };
        let (image, foreign) =
            (f2fs::format(&request).unwrap(), f2fs::format(&f2fs::Request { label: "someone", ..request }).unwrap());
        assert_eq!(identify(&image.superblock).map(|identity| identity.uuid), Some([7; 16]));
        assert_eq!((identify(&foreign.superblock), identify(&[0; f2fs::BLOCK_SIZE])), (None, None));
    }

    #[test]
    fn safe_asked_once() {
        let mut state = State::default();
        let open = |number| Some((number, Request::Open("opening")));
        assert!(state.ask_safe(open(1)) && !state.ask_safe(open(1)), "a request is handed over once by its number");
        assert!(!state.ask_safe(None) && !state.ask_safe(open(1)), "nor again after a pass without one");
        assert!(state.ask_safe(Some((2, Request::Change { current: "opening", new: "another" }))));
        let Some((2, order)) = state.order.take() else { panic!("the order of the second number") };
        assert_eq!(order.into_change(), Some(("opening".into(), "another".into())), "with passphrases of its own");
    }

    #[test]
    fn encrypting_reported_once() {
        let mut state = State::default();
        assert!(!state.progress().is_encrypting(), "no root has begun to move");
        assert!(state.begin_encrypting() && state.progress().is_encrypting());
        assert_eq!(core::mem::take(&mut state.report).progress, Some(Progress::Encrypting));
        assert!(!state.begin_encrypting(), "a second root is no news");
        assert_eq!(state.report, Report::default(), "and reports nothing");
    }

    #[test]
    fn step_queries() {
        let table = [
            (Progress::Looking, (true, true, false, None), false),
            (Progress::Waiting, (false, true, false, None), false),
            (Progress::Formatting, (true, true, false, None), true),
            (Progress::Shut { is_made: true, refused: Some((3, Refusal::Wrong)) }, (true, true, false, None), false),
            (Progress::Shut { is_made: false, refused: None }, (true, true, false, None), false),
            (Progress::Encrypting, (true, true, false, None), true),
            (Progress::Mounted(1), (false, false, true, Some(MOUNT_POINT)), false),
            (Progress::Failed("gone".into()), (false, false, false, None), false),
        ];
        for (step, expected, is_at_work) in table {
            assert_eq!((step.is_underway(), step.is_undecided(), step.is_mounted(), step.root()), expected, "{step:?}");
            assert_eq!(
                step.is_at_work(),
                is_at_work,
                "{step:?}: a safe that waits for its passphrase lets the device suspend"
            );
            let answer = Some(step.is_mounted());
            assert_eq!(
                (step.settled(), step.decided()),
                (answer.filter(|_| !step.is_underway()), answer.filter(|_| !step.is_undecided())),
                "{step:?}: a waiter is answered once its wait's end has come"
            );
        }
        assert_eq!(
            (Progress::Waiting.settled(), Progress::Waiting.decided()),
            (Some(false), None),
            "a partition that has not appeared ends the wait of who would not wait for it, and no other"
        );
    }
}
