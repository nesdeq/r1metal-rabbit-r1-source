//! The USB gadget: its identity to a host, its FunctionFS function, and the drive it shows.

mod drive;
mod function;

pub use drive::{HostChanges, Medium, MediumStep, OpenFile, inquiry, serial, to_snapshot_line};
pub use function::{EVENT_BYTES, MAX_EVENTS, Stall, descriptors, stall, strings};

use crate::base::text::has_words;
use crate::crypto::blake2s;
use alloc::format;
use alloc::string::{String, ToString};
use core::fmt::Display;

/// Linux Foundation, the customary vendor of configfs gadgets.
pub const VENDOR_ID: u16 = 0x1d6b;
/// "Multifunction Composite Gadget".
pub const PRODUCT_ID: u16 = 0x0104;
/// Product string by which the host tells an r1metal device from other gadgets with that identity.
pub const PRODUCT_NAME: &str = "r1metal console";
const VERSION: u16 = 0x0200;
/// The maker's string a host reads.
pub const MANUFACTURER: &str = "r1metal";
const DEVICE_CLASS: [u8; 3] = [0xEF, 0x02, 0x01];
const US_ENGLISH: u16 = 0x0409;
/// The string a host reads for the one configuration.
pub const CONFIGURATION_NAME: &str = "console, drive, frames and network";
const HARDWARE_FILES: [&str; 2] = ["dev_addr", "host_addr"];
// The bits of an address's first byte that tell a group's from a station's, and one a maker gave from one set locally.
const GROUP: u8 = 0x01;
const LOCAL: u8 = 0x02;
// Five unit loads, which a device that charges asks for whole.
const MAX_POWER_MA: u32 = 500;
/// The value that sets a switch of the gadget's files.
pub const SET: &str = "1";
/// What the drive's medium file takes to hold no medium.
pub const NO_MEDIUM: &str = "\n";
/// The error text of a function readied a second time.
pub const READIED_TWICE: &str = "the function was readied twice";
const DEVICE_CLASS_FIELDS: [&str; 3] = ["bDeviceClass", "bDeviceSubClass", "bDeviceProtocol"];

/// The name of the gadget's directories of strings in US English.
pub fn to_language_dir() -> String {
    format!("{US_ENGLISH:#x}")
}

/// The gadget's identity files and their values, in order.
pub fn to_identity_fields() -> [(&'static str, String); 6] {
    let [class, subclass, protocol] = DEVICE_CLASS_FIELDS;
    let [class_value, subclass_value, protocol_value] = DEVICE_CLASS.map(|value| format!("{value:#04x}"));
    [
        ("idVendor", format!("{VENDOR_ID:#06x}")),
        ("idProduct", format!("{PRODUCT_ID:#06x}")),
        ("bcdUSB", format!("{VERSION:#06x}")),
        (class, class_value),
        (subclass, subclass_value),
        (protocol, protocol_value),
    ]
}

/// The text of the gadget's MaxPower file.
pub fn to_power_text() -> String {
    MAX_POWER_MA.to_string()
}

/// True where the gadget's controller file names one.
pub fn is_bound(controller: &str) -> bool {
    has_words(controller)
}

/// A function the gadget works without: where the controller refuses the gadget with it, it goes out and the gadget is bound again.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Spared {
    Network,
    Frames,
}

impl Spared {
    /// In the order they go out: the newest first.
    pub const ALL: [Spared; 2] = [Spared::Network, Spared::Frames];

    /// The log line of the function taken out after a bind that failed with `error`.
    pub fn to_refused_line(self, error: &dyn Display) -> String {
        match self {
            Spared::Network => {
                format!("usb network: the controller refuses the gadget with it ({error}), binding it without")
            }
            Spared::Frames => {
                format!("usb frames: the controller refuses the gadget with them ({error}), binding it without")
            }
        }
    }
}

/// The network function's files of its two hardware addresses, the device's end and the host's, with their texts: each of the gadget's serial number, so that a host meets the same adapter at every start, a station's and set locally.
pub fn to_hardware_fields(serial: &str) -> [(&'static str, String); 2] {
    HARDWARE_FILES.map(|file| {
        let hash = blake2s::hash(&[serial.as_bytes(), file.as_bytes()]);
        let mut address: [u8; 6] = *hash.first_chunk().expect("a hash of thirty-two bytes");
        address[0] = address[0] & !GROUP | LOCAL;
        (file, address.map(|byte| format!("{byte:02x}")).join(":"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gadget_files() {
        assert_eq!(to_language_dir(), "0x409");
        assert_eq!(
            to_identity_fields().map(|(field, value)| alloc::format!("{field}={value}")),
            [
                "idVendor=0x1d6b",
                "idProduct=0x0104",
                "bcdUSB=0x0200",
                "bDeviceClass=0xef",
                "bDeviceSubClass=0x02",
                "bDeviceProtocol=0x01"
            ]
        );
        assert_eq!(to_power_text(), "500");
        assert!(is_bound("musb-hdrc\n") && !is_bound(" \n"));
    }

    #[test]
    fn network_hardware_addresses() {
        let [(device_file, device), (host_file, host)] = to_hardware_fields("R1A0000000001");
        assert_eq!((device_file, host_file), ("dev_addr", "host_addr"), "the function's files of its two ends");
        assert_eq!(to_hardware_fields("R1A0000000001"), [(device_file, device.clone()), (host_file, host.clone())]);
        assert_ne!(device, host, "each end its own");
        assert_ne!(to_hardware_fields("R1A0000000002")[0].1, device, "and each device its own");
        for address in [&device, &host] {
            let bytes: alloc::vec::Vec<u8> =
                address.split(':').map(|byte| u8::from_str_radix(byte, 16).expect("two hex digits")).collect();
            assert_eq!(
                (bytes.len(), address.len(), bytes[0] & 0x03),
                (6, 17, 0x02),
                "{address}: a station's, set locally"
            );
        }
    }

    #[test]
    fn spared_functions() {
        assert_eq!(Spared::ALL, [Spared::Network, Spared::Frames], "the newest goes out first");
        assert_eq!(
            Spared::ALL.map(|function| function.to_refused_line(&"no such device")),
            [
                "usb network: the controller refuses the gadget with it (no such device), binding it without",
                "usb frames: the controller refuses the gadget with them (no such device), binding it without"
            ]
        );
    }
}
