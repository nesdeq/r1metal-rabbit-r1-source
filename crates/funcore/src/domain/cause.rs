//! Why something shown to the owner came to nothing.

use crate::openai::fault::{Ending, Fault};
use crate::storage::SaveFault;

/// A reason the screens tell in a word.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cause {
    NoKey,
    Limit,
    NoNetwork,
    NoAnswer,
    Refused,
    NotWritten,
    SessionFailed,
    NoStorage,
    StorageFull,
    NotDeleted,
    NotRecorded,
    NotPlayed,
    NotHeard,
    // No feed asked for a rundown gave a story.
    NoFeed,
    PixelsRefused,
    NoPixels,
    CodeExpired,
    TooOld,
    TooManyDevices,
}

impl Cause {
    pub(super) const fn from_fault(fault: &Fault, temporary: Cause) -> Cause {
        match fault {
            Fault::NoKey => Cause::NoKey,
            Fault::Limit => Cause::Limit,
            Fault::Offline(_) => Cause::NoNetwork,
            Fault::Refused(_) => Cause::Refused,
            Fault::Temporary(_) => temporary,
        }
    }

    pub(super) const fn from_save_fault(fault: SaveFault) -> Cause {
        match fault {
            SaveFault::NoStorage => Cause::NoStorage,
            SaveFault::Full => Cause::StorageFull,
            SaveFault::NotRecorded => Cause::NotRecorded,
        }
    }

    /// The cause a session ended for; none where it simply ended.
    pub const fn from_ending(ending: &Ending) -> Option<Cause> {
        match ending {
            Ending::Done => None,
            Ending::Failed(fault) => Some(Cause::from_fault(fault, Cause::SessionFailed)),
            Ending::Timeout => Some(Cause::NoAnswer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn fault_causes() {
        let table = [
            (Fault::NoKey, Cause::NoKey),
            (Fault::Limit, Cause::Limit),
            (Fault::Offline(String::new()), Cause::NoNetwork),
            (Fault::Refused(String::new()), Cause::Refused),
            (Fault::Temporary(String::new()), Cause::NotPlayed),
        ];
        for (fault, cause) in table {
            assert_eq!(Cause::from_fault(&fault, Cause::NotPlayed), cause);
        }
    }

    #[test]
    fn session_endings() {
        assert_eq!(Cause::from_ending(&Ending::Done), None);
        assert_eq!(Cause::from_ending(&Ending::Timeout), Some(Cause::NoAnswer));
        assert_eq!(Cause::from_ending(&Ending::Failed(Fault::Temporary(String::new()))), Some(Cause::SessionFailed));
        assert_eq!(Cause::from_ending(&Ending::Failed(Fault::NoKey)), Some(Cause::NoKey));
    }
}
