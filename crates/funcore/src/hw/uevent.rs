//! Kernel device messages: uevents, uevent files, `/sys/dev` names and the gadget's state.

use alloc::format;
use alloc::string::{String, ToString};

/// The key of a uevent file that names a device's node.
pub const NAME_KEY: &str = "DEVNAME=";
const PARTITION_KEY: &str = "PARTNAME=";
const BOOT_DEVICE: &str = "bootdevice";
const CONFIGURED: &str = "CONFIGURED";
const DISCONNECTED: &str = "DISCONNECTED";
const ADDED: &str = "add";
const BLOCK: &str = "block";
const INPUT: &str = "input";
const LEDS: &str = "leds";

/// Nodes a coldplug made and failed to make.
#[derive(Default)]
pub struct Outcome {
    created: usize,
    failed: usize,
}

impl Outcome {
    /// Counts an entry: a node made, none needed, or a failure.
    pub fn add<E>(&mut self, result: Result<bool, E>) {
        match result {
            Ok(true) => self.created += 1,
            Ok(false) => {}
            Err(_) => self.failed += 1,
        }
    }

    /// The log line of a coldplug at the start of process 1.
    pub fn to_line(&self) -> String {
        format!("device nodes: {} created, {} failed", self.created, self.failed)
    }

    /// The log line after the gadget's start, none where nothing failed.
    pub fn to_gadget_line(&self) -> Option<String> {
        let Outcome { created, failed } = self;
        (*failed > 0)
            .then(|| format!("device nodes after gadget start: Outcome {{ created: {created}, failed: {failed} }}"))
    }
}

/// What the gadget's state word tells of a host.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HostState {
    // A host has taken the device up.
    Configured,
    // The host is gone.
    Disconnected,
    // A word that says neither, as `CONNECTED` of a host that has not taken the device up.
    Other,
}

impl HostState {
    /// The state a word of the gadget's state file or of its uevent tells, whatever blanks stand about it.
    pub fn parse(word: &str) -> HostState {
        match word.trim() {
            CONFIGURED => HostState::Configured,
            DISCONNECTED => HostState::Disconnected,
            _ => HostState::Other,
        }
    }

    /// Whether a host has the device now; a state that says neither keeps `previous`.
    pub const fn next_hosted(self, previous: bool) -> bool {
        match self {
            HostState::Configured => true,
            HostState::Disconnected => false,
            HostState::Other => previous,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Uevent {
    action: String,
    subsystem: String,
    devname: Option<String>,
    major: Option<u32>,
    minor: Option<u32>,
    /// The gadget's state, where the message tells it.
    pub host: Option<HostState>,
}

/// The node of a device the kernel added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node<'a> {
    pub name: &'a str,
    pub is_block: bool,
    pub is_input: bool,
    pub major: u32,
    pub minor: u32,
}

impl Uevent {
    fn is_add(&self) -> bool {
        self.action == ADDED
    }

    /// The node of an added device.
    pub fn node(&self) -> Option<Node<'_>> {
        if !self.is_add() {
            return None;
        }
        let (is_block, is_input) = (self.subsystem == BLOCK, self.subsystem == INPUT);
        Some(Node { name: self.devname.as_deref()?, is_block, is_input, major: self.major?, minor: self.minor? })
    }

    /// True where the kernel added a disk or a partition.
    pub fn is_block_add(&self) -> bool {
        self.is_add() && self.subsystem == BLOCK
    }

    /// True where the kernel added a light.
    pub fn is_light_add(&self) -> bool {
        self.is_add() && self.subsystem == LEDS
    }
}

/// Parses a kernel message: `action@devpath`, then NUL separated `KEY=VALUE` pairs.
pub fn parse(message: &[u8]) -> Option<Uevent> {
    let mut fields = message.split(|&byte| byte == 0).filter(|field| !field.is_empty()).map(String::from_utf8_lossy);
    if !fields.next()?.contains('@') {
        return None;
    }
    let mut event = Uevent::default();
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key {
            "ACTION" => event.action = value.to_string(),
            "SUBSYSTEM" => event.subsystem = value.to_string(),
            "DEVNAME" => event.devname = Some(value.to_string()),
            "MAJOR" => event.major = value.parse().ok(),
            "MINOR" => event.minor = value.parse().ok(),
            "USB_STATE" => event.host = Some(HostState::parse(value)),
            _ => {}
        }
    }
    (!event.action.is_empty()).then_some(event)
}

/// The device number of a `/sys/dev` entry named `major:minor`.
pub fn parse_device_number(entry: &str) -> Option<(u32, u32)> {
    let (major, minor) = entry.split_once(':')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The value of a key in a uevent file.
pub fn value<'a>(uevent: &'a str, key: &str) -> Option<&'a str> {
    uevent.lines().find_map(|line| line.strip_prefix(key))
}

fn is_on_boot_device(path: &str) -> bool {
    path.split('/').any(|step| step == BOOT_DEVICE)
}

/// The `/dev` name and number of the boot device's partition `name`, where a `/sys/dev` entry is it.
pub fn partition_node<'a>(name: &str, entry: &str, uevent: &'a str, link: &str) -> Option<(&'a str, u32, u32)> {
    if value(uevent, PARTITION_KEY) != Some(name) || !is_on_boot_device(link) {
        return None;
    }
    let (major, minor) = parse_device_number(entry)?;
    Some((value(uevent, NAME_KEY)?, major, minor))
}

/// The error text of a partition without a node.
pub fn to_no_partition_text(name: &str) -> String {
    format!("no partition named {name} on the boot device")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_message() {
        let message = b"add@/devices/virtual/input/input3/event3\0ACTION=add\0DEVPATH=/devices/virtual/input/input3/event3\0SUBSYSTEM=input\0MAJOR=13\0MINOR=67\0DEVNAME=input/event3\0SEQNUM=2161\0";
        let event = parse(message).unwrap();
        assert_eq!(event.action, "add");
        assert_eq!(event.subsystem, "input");
        assert_eq!(event.devname.as_deref(), Some("input/event3"));
        assert_eq!((event.major, event.minor), (Some(13), Some(67)));
        let node = Node { name: "input/event3", is_block: false, is_input: true, major: 13, minor: 67 };
        assert_eq!((event.node(), event.is_block_add(), event.is_light_add()), (Some(node), false, false));
    }

    #[test]
    fn added_device_kinds() {
        let disk =
            parse(b"add@/devices/x\0ACTION=add\0SUBSYSTEM=block\0DEVNAME=mmcblk0p9\0MAJOR=179\0MINOR=9\0").unwrap();
        assert_eq!(
            disk.node(),
            Some(Node { name: "mmcblk0p9", is_block: true, is_input: false, major: 179, minor: 9 })
        );
        assert!(disk.is_block_add() && !disk.is_light_add());
        let light = parse(b"add@/devices/x\0ACTION=add\0SUBSYSTEM=leds\0").unwrap();
        assert_eq!(
            (light.node(), light.is_block_add(), light.is_light_add()),
            (None, false, true),
            "a light has no node"
        );
        let removal =
            parse(b"remove@/devices/x\0ACTION=remove\0SUBSYSTEM=block\0DEVNAME=sda\0MAJOR=8\0MINOR=0\0").unwrap();
        assert_eq!((removal.node(), removal.is_block_add()), (None, false), "a device taken away is added nowhere");
    }

    #[test]
    fn change_without_node() {
        let message = b"change@/devices/platform/battery/power_supply/battery\0ACTION=change\0SUBSYSTEM=power_supply\0POWER_SUPPLY_CAPACITY=-1\0";
        let event = parse(message).unwrap();
        assert_eq!((event.action.as_str(), event.subsystem.as_str()), ("change", "power_supply"));
        assert_eq!((event.devname.as_deref(), event.major, event.node()), (None, None, None));
    }

    #[test]
    fn gadget_state() {
        let message = b"change@/devices/virtual/android_usb/android0\0ACTION=change\0DEVPATH=/devices/virtual/android_usb/android0\0SUBSYSTEM=android_usb\0USB_STATE=CONFIGURED\0SEQNUM=2291\0";
        let event = parse(message).unwrap();
        assert_eq!((event.subsystem.as_str(), event.host), ("android_usb", Some(HostState::Configured)));
        assert_eq!(parse(b"add@/devices/x\0ACTION=add\0SUBSYSTEM=block\0").unwrap().host, None);
        assert_eq!(
            ["CONFIGURED", "DISCONNECTED\n", "CONNECTED", ""].map(HostState::parse),
            [HostState::Configured, HostState::Disconnected, HostState::Other, HostState::Other],
            "a host that is only connected has not taken the device up, nor has it gone; the state file ends its word with a line end"
        );
        assert_eq!(
            [("CONFIGURED", false), ("DISCONNECTED", true), ("CONNECTED", true), ("CONNECTED", false)]
                .map(|(state, previous)| HostState::parse(state).next_hosted(previous)),
            [true, false, true, false],
            "a word that says neither leaves the host as it was"
        );
    }

    #[test]
    fn foreign_messages_ignored() {
        assert_eq!(parse(b"libudev\0\xfe\xed\xca\xfe"), None);
        assert_eq!(parse(b"add@/devices/x\0SUBSYSTEM=input\0"), None);
        assert_eq!(parse(b""), None);
    }

    #[test]
    fn coldplug_counts() {
        let mut outcome = Outcome::default();
        outcome.add::<()>(Ok(true));
        outcome.add::<()>(Ok(false));
        assert_eq!((outcome.to_line().as_str(), outcome.to_gadget_line()), ("device nodes: 1 created, 0 failed", None));
        outcome.add(Err(()));
        assert_eq!(
            outcome.to_gadget_line().as_deref(),
            Some("device nodes after gadget start: Outcome { created: 1, failed: 1 }")
        );
    }

    #[test]
    fn device_numbers() {
        assert_eq!(parse_device_number("29:0"), Some((29, 0)));
        assert_eq!(parse_device_number("179:23"), Some((179, 23)));
        assert_eq!(parse_device_number("garbage"), None);
        assert_eq!(parse_device_number("1:x"), None);
    }

    #[test]
    fn uevent_values() {
        let uevent = "MAJOR=179\nMINOR=2\nDEVNAME=mmcblk0p2\nDEVTYPE=partition\nPARTN=2\nPARTNAME=para\n";
        assert_eq!(value(uevent, PARTITION_KEY), Some("para"));
        assert_eq!(value(uevent, NAME_KEY), Some("mmcblk0p2"));
        assert_eq!((value("MAJOR=1\n", PARTITION_KEY), value("MAJOR=1\n", NAME_KEY)), (None, None));
    }

    #[test]
    fn boot_device_paths() {
        assert!(is_on_boot_device("../../devices/platform/bootdevice/mmc_host/mmc0/mmc0:0001/block/mmcblk0/mmcblk0p5"));
        assert!(
            !is_on_boot_device("../../devices/platform/externdevice/mmc_host/mmc1/mmc1:aaaa/block/mmcblk1/mmcblk1p1"),
            "a card's"
        );
        assert!(!is_on_boot_device("../../devices/virtual/block/bootdevice0"), "a name that only begins as the host's");
    }

    #[test]
    fn partition_by_name_and_path() {
        const UEVENT: &str = "DEVNAME=mmcblk0p2\nPARTNAME=para\n";
        const BOOT: &str = "../../devices/bootdevice/block/mmcblk0/mmcblk0p2";
        assert_eq!(partition_node("para", "179:2", UEVENT, BOOT), Some(("mmcblk0p2", 179, 2)));
        assert_eq!(partition_node("other", "179:2", UEVENT, BOOT), None, "another name");
        assert_eq!(partition_node("para", "179:2", UEVENT, "../../devices/card"), None, "a card's");
        assert_eq!(partition_node("para", "garbage", UEVENT, BOOT), None);
        assert_eq!(partition_node("para", "179:2", "PARTNAME=para\n", BOOT), None, "no node's name");
        assert_eq!(to_no_partition_text("para"), "no partition named para on the boot device");
    }
}
