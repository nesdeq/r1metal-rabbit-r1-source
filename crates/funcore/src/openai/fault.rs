//! The failures of requests to a service, and the end of a session.

use super::key;
use super::refusal::{Refusal, is_temporary};
use alloc::string::{String, ToString};
use core::fmt;

/// Why a request to a service failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fault {
    NoKey,
    // The month's cost has reached the spending limit.
    Limit,
    // With the radio's reason.
    Offline(String),
    // Asking again would come to the same.
    Refused(String),
    // Asking again may work.
    Temporary(String),
}

impl<E: core::error::Error> From<E> for Fault {
    fn from(error: E) -> Fault {
        Fault::Temporary(error.to_string())
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::NoKey => write!(formatter, "no key to the services in {}/{}", key::DIRECTORY, key::FILE),
            Fault::Limit => formatter.write_str("the month's spending limit is reached"),
            Fault::Offline(reason) => write!(formatter, "not in a network: {reason}"),
            Fault::Refused(reason) => write!(formatter, "refused: {reason}"),
            Fault::Temporary(reason) => formatter.write_str(reason),
        }
    }
}

impl Fault {
    /// A service's refusal: temporary where the status says asking later may come to something else.
    pub fn from_refusal(status: Option<u16>, refusal: &Refusal) -> Fault {
        let reason = refusal.to_log_line(status);
        match status {
            Some(status) if is_temporary(status) => Fault::Temporary(reason),
            _ => Fault::Refused(reason),
        }
    }

    // No more of such work is tried until a screen of it opens again.
    pub(crate) const fn is_stalling(&self) -> bool {
        matches!(self, Fault::NoKey | Fault::Limit | Fault::Offline(_))
    }

    pub(crate) const fn is_permanent(&self) -> bool {
        matches!(self, Fault::NoKey | Fault::Limit | Fault::Refused(_))
    }
}

/// The end of a session with a service.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Ending {
    // The screen ended it, or it came to its end.
    Done,
    Failed(Fault),
    // The service sent nothing of what it owes within the limit.
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fault_kinds() {
        let table = [
            (Fault::NoKey, (true, true), "no key to the services in .r1metal/openai/key"),
            (Fault::Limit, (true, true), "the month's spending limit is reached"),
            (Fault::Offline("asleep".into()), (true, false), "not in a network: asleep"),
            (Fault::Refused("no".into()), (false, true), "refused: no"),
            (Fault::Temporary("gone".into()), (false, false), "gone"),
        ];
        for (fault, (is_stalling, is_permanent), line) in table {
            assert_eq!(
                (fault.is_stalling(), fault.is_permanent(), fault.to_string().as_str()),
                (is_stalling, is_permanent, line)
            );
        }
        assert_eq!(Fault::from(fmt::Error), Fault::Temporary(fmt::Error.to_string()));
    }

    #[test]
    fn temporary_refusals() {
        let refusal = Refusal { code: "invalid_value".into(), message: "No.".into() };
        let is_permanent = |status| Fault::from_refusal(status, &refusal).is_permanent();
        assert_eq!(
            [None, Some(400), Some(401), Some(404), Some(429), Some(500), Some(503)].map(is_permanent),
            [true, true, true, true, false, false, false]
        );
        assert_eq!(Fault::from_refusal(Some(503), &refusal), Fault::Temporary("status 503, invalid_value: No.".into()));
    }
}
