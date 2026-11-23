//! What process 1 decides: when it starts an app that ended again, and whether it restarts a device the bootloader started to charge.

use crate::base::bytes::le32;
use crate::domain::device::cmdline_value;
use alloc::format;
use alloc::string::String;
use core::time::Duration;

const RESTART_SPACING: Duration = Duration::from_secs(1);
const BOOT_REASON_KEY: &str = "androidboot.bootreason=";
// A cable switched the device on; a restart's reason is `reboot`, so the restart is made once.
const CABLE_REASON: &str = "usb";
// `atag,boot` holds its size, tag, boot mode and boot type, each a little-endian u32 (`struct tag_bootmode`).
const MODE_AT: usize = 8;
// KERNEL_POWER_OFF_CHARGING_BOOT and LOW_POWER_OFF_CHARGING_BOOT, whose kernel powers off when the cable is pulled.
const CHARGING_MODES: [u32; 2] = [8, 9];

/// How long the next start waits after one that ran for `run_time`.
pub const fn restart_pause(run_time: Duration) -> Duration {
    RESTART_SPACING.saturating_sub(run_time)
}

/// What the binary tells whoever started it as another process than the first; none for process 1.
pub const fn to_not_init_line(pid: u32) -> Option<&'static str> {
    if pid == 1 { None } else { Some("this binary is an init and must run as process 1") }
}

/// The first log line.
pub fn to_start_line() -> String {
    format!("r1metal {} starting", crate::VERSION)
}

/// The log line of an app that ended.
pub fn to_ended_line(status: i32) -> String {
    format!("app ended with wait status {status:#06x}, restarting it")
}

/// The log line of the restart before the app's first start where a cable had the bootloader boot to charge; none otherwise.
pub fn to_charging_restart_line(boot_tag: &[u8], cmdline: &str) -> Option<String> {
    let mode = le32(boot_tag, MODE_AT)?;
    let is_charging = CHARGING_MODES.contains(&mode) && cmdline_value(cmdline, BOOT_REASON_KEY) == Some(CABLE_REASON);
    is_charging.then(|| {
        format!("charging boot (boot mode {mode}): restarting, as its kernel powers off when the cable is pulled")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CABLE: &str = "console=tty0 androidboot.bootreason=usb gpt=1";

    fn boot_tag(mode: u8) -> [u8; 16] {
        [3, 0, 0, 0, 2, 8, 0, 0x41, mode, 0, 0, 0, 1, 0, 0, 0]
    }

    #[test]
    fn restart_spacing() {
        let table = [(Duration::from_millis(20), 980), (RESTART_SPACING, 0), (Duration::from_secs(3600), 0)];
        for (run_time, pause_ms) in table {
            assert_eq!(restart_pause(run_time), Duration::from_millis(pause_ms));
        }
    }

    #[test]
    fn init_alone_runs() {
        assert_eq!(
            [to_not_init_line(1), to_not_init_line(4321)],
            [None, Some("this binary is an init and must run as process 1")]
        );
    }

    #[test]
    fn ended_line() {
        assert_eq!(to_ended_line(0x8b), "app ended with wait status 0x008b, restarting it");
    }

    #[test]
    fn charging_boot_restarts() {
        for mode in [8, 9] {
            assert_eq!(
                to_charging_restart_line(&boot_tag(mode), CABLE),
                Some(format!(
                    "charging boot (boot mode {mode}): restarting, as its kernel powers off when the cable is pulled"
                )),
                "mode {mode} after a cable switched the device on"
            );
        }
    }

    #[test]
    fn other_boots_start_the_app() {
        let cases = [
            (&boot_tag(0)[..], CABLE, "a normal boot on a cable"),
            (&boot_tag(8)[..], "androidboot.bootreason=reboot", "the restart's own boot"),
            (&boot_tag(8)[..], "androidboot.bootreason=PowerKey", "a boot by the button"),
            (&boot_tag(8)[..], "console=tty0", "a boot without a reason"),
            (&boot_tag(8)[..MODE_AT + 3], CABLE, "a tag cut inside the mode"),
            (&[][..], CABLE, "no tag"),
        ];
        for (tag, cmdline, case) in cases {
            assert_eq!(to_charging_restart_line(tag, cmdline), None, "{case}");
        }
    }
}
