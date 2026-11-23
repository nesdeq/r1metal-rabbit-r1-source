//! Power: battery, charge, use, sleep, alarm, thermal policy and the ways down.

use core::time::Duration;

pub mod alarm;
pub mod battery;
mod charge;
pub mod doze;
pub mod thermal;
pub mod usage;

/// The longest a way down waits for the threads that save data, and a suspend for the logs; then the device goes down, or suspends, all the same.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// A way down the device takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Power {
    Reboot,
    // Restarts into the bootloader's fastboot.
    Bootloader,
    PowerOff,
}

impl Power {
    // The way down as the log names it.
    pub(crate) const fn to_word(self) -> &'static str {
        match self {
            Power::Reboot => "Reboot",
            Power::Bootloader => "Bootloader",
            Power::PowerOff => "PowerOff",
        }
    }
}
