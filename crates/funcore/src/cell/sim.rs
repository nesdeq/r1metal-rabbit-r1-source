//! The SIM's file, with its PIN.

use crate::base::lines::value;
use alloc::format;
use alloc::string::String;
use core::fmt;

/// The SIM's file in `cell::DIRECTORY`.
pub const FILE: &str = "sim";
const PIN_KEY: &str = "pin";
// A PIN is 4 to 8 digits (3GPP TS 22.030).
pub(crate) const PIN_DIGITS: core::ops::RangeInclusive<usize> = 4..=8;

/// A SIM's PIN; its `Debug` tells none of it.
#[derive(Clone, PartialEq, Eq)]
pub struct Pin(String);

impl Pin {
    // The PIN in a text, if it is 4 to 8 digits.
    pub(crate) fn parse(text: &str) -> Option<Pin> {
        let text = text.trim();
        (PIN_DIGITS.contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit())).then(|| Pin(text.into()))
    }

    // The digits, for the command that hands them to the SIM.
    pub(crate) fn digits(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Pin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Pin")
    }
}

/// What the SIM's file keeps.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Sim {
    pub(super) pin: Option<Pin>,
}

impl Sim {
    /// The file's content; a line that does not read is passed over.
    pub fn parse(text: &str) -> Sim {
        Sim { pin: value(text, PIN_KEY).and_then(Pin::parse) }
    }

    pub fn to_text(&self) -> String {
        self.pin.as_ref().map(|pin| format!("{PIN_KEY} {}\n", pin.0)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins() {
        assert!(Pin::parse("1234").is_some() && Pin::parse(" 12345678 ").is_some());
        for none in ["123", "123456789", "12a4", ""] {
            assert!(Pin::parse(none).is_none(), "{none:?}");
        }
        assert_eq!(format!("{:?}", Pin::parse("1234").unwrap()), "Pin", "no digit in the log");
    }

    #[test]
    fn sim_round_trip() {
        let file = "pin 1234\n";
        let sim = Sim::parse(file);
        assert_eq!(sim.pin.as_ref().map(Pin::digits), Some("1234"));
        assert_eq!(sim.to_text(), file);
        let shown = format!("{sim:?}");
        assert!(!shown.contains("1234"), "{shown}");
        assert_eq!(Sim::parse("").to_text(), "", "nothing kept, nothing written");
        assert_eq!(
            Sim::parse("pin 12\nother 1\n"),
            Sim::default(),
            "a PIN that is none, and a line of another key, are passed over"
        );
    }
}
