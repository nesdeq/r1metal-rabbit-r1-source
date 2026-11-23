use crate::base::handoff::Latch;
use crate::hw::uevent::HostState;
use alloc::format;
use alloc::string::String;

const VENDOR: &str = "r1metal";
const MODEL: &str = "R1";
const MIB_SHIFT: u32 = 20;

/// The gadget's host changes since the drive's thread last looked, and whether the owner has the drive shown to a host.
#[derive(Default)]
pub struct HostChanges {
    is_changed: bool,
    has_left: bool,
    is_shown: bool,
}

impl HostChanges {
    /// Takes the gadget's state.
    pub fn note(&mut self, state: HostState) {
        (self.is_changed, self.has_left) = (true, self.has_left || state == HostState::Disconnected);
    }

    /// Takes whether the owner has the drive shown; true where that is a change the thread is to look at.
    pub const fn set_shown(&mut self, is_shown: bool) -> bool {
        let is_new = self.is_shown != is_shown;
        (self.is_shown, self.is_changed) = (is_shown, self.is_changed || is_new);
        is_new
    }

    /// Forgets what the host did, and keeps whether the owner has the drive shown.
    pub const fn reset(&mut self) {
        (self.is_changed, self.has_left) = (false, false);
    }

    /// True where a medium may be in: the owner has the drive shown, and a host has the device.
    pub const fn is_offered(&self, is_configured: bool) -> bool {
        self.is_shown && is_configured
    }

    /// Whether the host went away in between; none where nothing changed.
    pub const fn take(&mut self) -> Option<bool> {
        if !self.is_changed {
            return None;
        }
        let has_left = self.has_left;
        (self.is_changed, self.has_left) = (false, false);
        Some(has_left)
    }
}

/// The drive's thread's next step with its medium.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediumStep {
    Eject,
    // A snapshot of the partition as it is now goes in.
    Load,
    Wait,
}

#[derive(Clone, Copy, Default)]
enum Stage {
    Eject(bool, bool),
    Load(bool),
    #[default]
    Rest,
}

/// The drive's medium: whether one is in, and the pass over it.
#[derive(Default)]
pub struct Medium {
    is_loaded: bool,
    stage: Stage,
}

impl Medium {
    const fn should_eject(&self, is_configured: bool, has_left: bool) -> bool {
        self.is_loaded && (has_left || !is_configured)
    }

    const fn should_load(&self, is_configured: bool) -> bool {
        is_configured && !self.is_loaded
    }

    /// Takes whether a medium is in now.
    pub const fn set_loaded(&mut self, is_loaded: bool) {
        self.is_loaded = is_loaded;
    }

    /// Takes whether a host has the device, and whether it went away since the last look.
    pub const fn begin_pass(&mut self, is_configured: bool, has_left: bool) {
        self.stage = Stage::Eject(is_configured, has_left);
    }

    /// The next step of the pass.
    pub const fn next_step(&mut self) -> MediumStep {
        if let Stage::Eject(is_configured, has_left) = self.stage {
            self.stage = Stage::Load(is_configured);
            if self.should_eject(is_configured, has_left) {
                return MediumStep::Eject;
            }
        }
        if let Stage::Load(is_configured) = self.stage {
            self.stage = Stage::Rest;
            if self.should_load(is_configured) {
                return MediumStep::Load;
            }
        }
        MediumStep::Wait
    }
}

/// The file a host's reads last went to, by its number in the snapshot, and the file whose failure was last told.
pub struct OpenFile<F> {
    open: Option<(usize, F)>,
    failed: Latch<usize>,
}

impl<F> Default for OpenFile<F> {
    fn default() -> OpenFile<F> {
        OpenFile { open: None, failed: Latch::default() }
    }
}

impl<F> OpenFile<F> {
    /// True where the file of that number is open.
    pub fn is_open(&self, id: usize) -> bool {
        self.open.as_ref().is_some_and(|(open_id, _)| *open_id == id)
    }

    /// Takes the file of that number, none where it did not open; one that opened ends the failures told.
    pub fn set(&mut self, id: usize, file: Option<F>) {
        if file.is_some() {
            self.failed.forget();
        }
        self.open = file.map(|file| (id, file));
    }

    /// The file that is open.
    pub fn file(&self) -> Option<&F> {
        self.open.as_ref().map(|(_, file)| file)
    }

    /// True the first time in a row the file of that number fails to open or to be read: the time to tell it.
    pub fn is_failure_new(&mut self, id: usize) -> bool {
        self.failed.change(id).is_some()
    }
}

/// The log line of a snapshot of the partition.
pub fn to_snapshot_line(files: usize, used_bytes: u64, skips: usize) -> String {
    format!("drive: {files} files, {} MiB in use, {skips} entries a host cannot be shown", used_bytes >> MIB_SHIFT)
}

/// The volume's serial number: the start of the filesystem's id, so a host sees the same drive at every start.
pub const fn serial(id: [u8; 16]) -> u32 {
    u32::from_le_bytes([id[0], id[1], id[2], id[3]])
}

/// The SCSI INQUIRY string the mass storage function takes: vendor in 8 characters, model in 16, revision in 4.
pub fn inquiry() -> String {
    let revision = concat!(env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"));
    format!("{VENDOR:<8.8}{MODEL:<16.16}{revision:<4.4}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_changes() {
        let mut changes = HostChanges::default();
        assert_eq!(changes.take(), None, "nothing said");
        for state in [HostState::Other, HostState::Disconnected, HostState::Configured] {
            changes.note(state);
        }
        assert_eq!(
            (changes.take(), changes.take()),
            (Some(true), None),
            "a host that went away in between is remembered"
        );
        changes.note(HostState::Configured);
        assert_eq!(changes.take(), Some(false));
        assert!(!changes.is_offered(true), "a drive the owner has not switched on offers no host a medium");
        assert!(!changes.set_shown(false) && changes.take().is_none(), "as it was is no change");
        assert!(changes.set_shown(true) && changes.is_offered(true) && !changes.is_offered(false));
        assert_eq!(changes.take(), Some(false), "switched on, the thread looks, and no host went away for it");
        assert!(changes.set_shown(false) && !changes.is_offered(true), "switched off, the medium goes");
    }

    #[test]
    fn reset_keeps_owner_switch() {
        let mut changes = HostChanges::default();
        changes.note(HostState::Disconnected);
        changes.reset();
        assert_eq!(changes.take(), None, "what the host did before the drive began is forgotten");
        changes.note(HostState::Disconnected);
        changes.reset();
        changes.note(HostState::Configured);
        assert_eq!(changes.take(), Some(false), "and counts for nothing in what it does after");
        changes.set_shown(true);
        changes.reset();
        assert!(changes.take().is_none() && changes.is_offered(true), "what the owner switched stays through a reset");
    }

    #[test]
    fn medium_rules() {
        let mut medium = Medium::default();
        assert!(!medium.should_load(false) && medium.should_load(true), "in once a host has the device");
        medium.set_loaded(true);
        assert!(!medium.should_load(true) && !medium.should_eject(true, false), "and stays while it does");
        assert!(medium.should_eject(false, false), "out once it is gone");
        assert!(
            medium.should_eject(true, true),
            "and out to go in anew where it went away and came back since the last look"
        );
        medium.set_loaded(false);
        assert!(!medium.should_eject(false, true) && medium.should_load(true));
        let id = [0x6f, 0x1d, 0x8a, 0x52, 0x3c, 0x4b, 0x4e, 0x7a, 0x9b, 0x21, 0x0f, 0x5e, 0x6d, 0x7c, 0x8a, 0x90];
        assert_eq!(serial(id), 0x528a_1d6f, "the first four bytes of the filesystem's id, the low one first");
    }

    #[test]
    fn eject_then_load() {
        let mut medium = Medium::default();
        assert_eq!(medium.next_step(), MediumStep::Wait, "nothing told");
        medium.begin_pass(true, false);
        assert_eq!((medium.next_step(), medium.next_step()), (MediumStep::Load, MediumStep::Wait));
        medium.set_loaded(true);
        medium.begin_pass(true, true);
        assert_eq!(medium.next_step(), MediumStep::Eject, "it went away and came back");
        medium.set_loaded(false);
        assert_eq!((medium.next_step(), medium.next_step()), (MediumStep::Load, MediumStep::Wait));
        medium.set_loaded(true);
        medium.begin_pass(true, true);
        assert_eq!((medium.next_step(), medium.next_step()), (MediumStep::Eject, MediumStep::Wait), "it holds on");
    }

    #[test]
    fn open_file_by_number() {
        let mut open = OpenFile::default();
        assert!(!open.is_open(1) && open.file().is_none());
        open.set(1, Some("file"));
        assert_eq!((open.is_open(1), open.is_open(2), open.file()), (true, false, Some(&"file")));
        open.set(2, None);
        assert!(!open.is_open(2) && open.file().is_none(), "one that did not open is tried again");
        assert!(open.is_failure_new(2) && !open.is_failure_new(2), "and its failure told once for all its blocks");
        assert!(open.is_failure_new(3) && open.is_failure_new(2), "another file's failure is another");
        open.set(1, Some("file"));
        assert!(open.is_failure_new(2), "and once a file has opened since, the same one's is told again");
    }

    #[test]
    fn snapshot_line() {
        assert_eq!(to_snapshot_line(3, 5 << 20, 2), "drive: 3 files, 5 MiB in use, 2 entries a host cannot be shown");
    }

    #[test]
    fn inquiry_fields() {
        let answer = inquiry();
        let version = concat!(env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"));
        assert_eq!(
            (answer.len(), &answer[..8], &answer[8..24], answer[24..].trim_end()),
            (28, "r1metal ", "R1              ", version),
            "maker, model and revision, each filled up with blanks to its field"
        );
    }
}
