//! The USB link: finding the R1 in its guises and the chosen one among several, and moving bytes over a bulk endpoint pair for the protocols each guise speaks (on macOS, over the terminal of the preloader's serial line), and over the endpoint a running device's screen recording comes on.

pub mod console;
pub mod fastboot;
pub mod frames;
pub mod preloader;
#[cfg(target_os = "macos")]
mod serial;

use crate::Result;
use nusb::descriptors::TransferType;
use nusb::hotplug::HotplugEvent;
use nusb::transfer::{Buffer, Bulk, Direction, In, Out};
use nusb::{Device, DeviceInfo, Endpoint, Interface, InterfaceInfo, MaybeFuture};
use std::thread;
use std::time::{Duration, Instant};

// MediaTek's vendor id, which the preloader and the bootloader show.
const MEDIATEK: u16 = 0x0e8d;
// The class of the interface a serial line's bytes travel over.
const CDC_DATA_CLASS: u8 = 0x0a;
// Size of one queued transfer while sending a payload; far below usbfs's 16 MB memory limit.
const PAYLOAD_CHUNK: usize = 1 << 20;
// How long macOS is given to configure a device it has told of: it takes about 10 ms, and tells of none the user has not allowed to connect.
const CONFIGURED_WITHIN: Duration = Duration::from_secs(2);
// How often a device that lists no interfaces, or a terminal macOS has not made yet, is looked for again.
const RELIST_EVERY: Duration = Duration::from_millis(20);
// What macOS asks before it lets a USB device be used, and where it is told to ask no more, in its dialog's and System Settings' own words.
const MACOS_ASKS: &str = "macOS asks \"Allow accessory to connect?\" whenever the R1 comes onto USB, after every reboot: click Allow, and r1 goes on.
To be asked no more, which catching an R1 at power on needs:
  System Settings > Privacy & Security > Security > Allow accessories to connect > Automatically When Unlocked";
// What Linux needs before a user who is not root reaches the R1.
const LINUX_RULE: &str =
    "Linux reaches the R1 without root once udev/70-r1metal.rules is installed; scripts/setupcodebase.sh installs it.";
/// What this computer asks of the user before `r1` reaches an R1, where it asks anything.
pub const HOST_ASKS: Option<&str> = if cfg!(target_os = "macos") {
    Some(MACOS_ASKS)
} else if cfg!(target_os = "linux") {
    Some(LINUX_RULE)
} else {
    None
};
// What a device that cannot be opened is told with: on Linux, the rule that lets the user open it.
const DENIED_HINT: &str =
    if cfg!(target_os = "linux") { " (if access is denied, install udev/70-r1metal.rules)" } else { "" };

/// On macOS, tells the user what macOS will ask once the R1 comes onto USB, before it does.
pub fn tell_what_macos_asks() {
    if cfg!(target_os = "macos") {
        eprintln!("{MACOS_ASKS}");
    }
}

/// What an R1 shows itself as on USB.
pub enum Guise {
    Preloader,
    Fastboot,
    Running,
}

/// The guise a device shows, where it is an R1.
pub fn guise(device: &DeviceInfo) -> Option<Guise> {
    if fastboot::is_fastboot(device) {
        Some(Guise::Fastboot)
    } else if preloader::is_preloader(device) {
        Some(Guise::Preloader)
    } else if console::is_r1metal(device) {
        Some(Guise::Running)
    } else {
        None
    }
}

/// Lists every R1 that is plugged in: what it shows itself as, its port and its serial number.
pub fn devices() -> Result<()> {
    let mut found = 0;
    for device in nusb::list_devices().wait()? {
        if let Some(guise) = guise(&device) {
            let called = match guise {
                Guise::Preloader => "preloader",
                Guise::Fastboot => "fastboot",
                Guise::Running => "r1metal",
            };
            println!("{called:9} {}", describe(&device));
            found += 1;
        }
    }
    if found == 0 {
        eprintln!("no R1 is plugged in");
        tell_what_macos_asks();
    }
    Ok(())
}

/// The chosen R1 where it is plugged in, in a guise `counted` takes, and every other R1 that is so; several that could be the chosen one are refused.
pub fn plugged_in(choice: &Choice, counted: impl Fn(&Guise) -> bool) -> Result<(Option<DeviceInfo>, Vec<DeviceInfo>)> {
    let present = nusb::list_devices().wait()?.filter(|device| guise(device).as_ref().is_some_and(&counted));
    let (ours, others): (Vec<DeviceInfo>, Vec<DeviceInfo>) = present.partition(|device| choice.takes(device));
    if ours.len() > 1 {
        return Err(several(&ours));
    }
    Ok((ours.into_iter().next(), others))
}

/// Blocks until a device that `pick` accepts is connected; one already plugged in counts.
pub fn wait_for<T>(mut pick: impl FnMut(&DeviceInfo) -> Option<T>) -> Result<(DeviceInfo, T)> {
    let watch = nusb::watch_devices()?;
    let connected = futures_lite::stream::block_on(watch).filter_map(|event| match event {
        HotplugEvent::Connected(device) => Some(device),
        _ => None,
    });
    for device in nusb::list_devices().wait()?.chain(connected) {
        if let Some(kind) = pick(&device) {
            return Ok((configured(device), kind));
        }
    }
    Err("the USB hotplug watch ended".into())
}

// The device as listed once it lists its interfaces, which a configured device always has; as it was if it goes or they do not come within `CONFIGURED_WITHIN`. macOS tells of a device just before it sets its configuration; Linux tells of one configured, and one it could not configure is taken as it is.
fn configured(mut device: DeviceInfo) -> DeviceInfo {
    let until = Instant::now() + CONFIGURED_WITHIN;
    while cfg!(target_os = "macos") && device.interfaces().next().is_none() && Instant::now() < until {
        thread::sleep(RELIST_EVERY);
        let listed = nusb::list_devices().wait().ok().and_then(|mut all| all.find(|d| d.id() == device.id()));
        let Some(listed) = listed else { break };
        device = listed;
    }
    device
}

fn has_ids(device: &DeviceInfo, vendor: u16, product: u16) -> bool {
    device.vendor_id() == vendor && device.product_id() == product
}

/// The port a device is on, as Linux names it: the bus, then the ports from the root hub down (`5-4.2.3`).
pub fn port(device: &DeviceInfo) -> String {
    let chain: Vec<String> = device.port_chain().iter().map(u8::to_string).collect();
    format!("{}-{}", bus_number(device.bus_id()), chain.join("."))
}

// A bus's number in decimal, from nusb's bus id: decimal on Linux (`005`), hexadecimal on macOS (`00`, the top byte of the location ID).
fn bus_number(id: &str) -> String {
    let radix = if cfg!(target_os = "macos") { 16 } else { 10 };
    u8::from_str_radix(id, radix).map_or_else(|_| id.to_owned(), |bus| bus.to_string())
}

// True if a device is the one named, by its port or by the end of its serial number; with none named every device is.
fn named(name: Option<&str>, serial: Option<&str>, port: &str) -> bool {
    name.is_none_or(|name| port == name || serial.is_some_and(|serial| serial.ends_with(name)))
}

/// Which R1 a command is for: the one on the port named or whose USB serial number ends with the text named, or with none named the only one there is.
pub struct Choice(Option<String>);

impl Choice {
    pub fn new(name: Option<String>) -> Choice {
        Choice(name.filter(|name| !name.is_empty()))
    }

    /// True if `device` may be the R1 the command is for.
    pub fn takes(&self, device: &DeviceInfo) -> bool {
        named(self.0.as_deref(), device.serial_number(), &port(device))
    }

    /// True if a device that tells nothing of its serial number may be the R1 the command is for: one on the port named, or any where no port is named.
    pub fn may_be(&self, device: &DeviceInfo) -> bool {
        on_port(self.0.as_deref(), &port(device))
    }
}

// True for a name that is a port, as Linux names one (`5-4.2.3`).
fn is_port(name: &str) -> bool {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    name.split_once('-').is_some_and(|(bus, chain)| digits(bus) && chain.split('.').all(digits))
}

// True if a device on `port` may be the one named, where a port is all that can tell.
fn on_port(name: Option<&str>, port: &str) -> bool {
    name.is_none_or(|name| !is_port(name) || name == port)
}

// A device as a list of them names it: the port it is on, and its serial number.
fn describe(device: &DeviceInfo) -> String {
    format!("{:10} {}", port(device), device.serial_number().unwrap_or("without a serial number"))
}

// The refusal of a command that is for one R1 where several are there: each of them, and how one is named.
fn several(devices: &[DeviceInfo]) -> Box<dyn std::error::Error> {
    let listed: Vec<String> = devices.iter().map(|device| format!("  {}", describe(device))).collect();
    let how = "name one with --device or R1_DEVICE: its port, or the end of its serial number";
    format!("{} R1s are plugged in, and the command is for one:\n{}\n{how}", devices.len(), listed.join("\n")).into()
}

// The address of a claimed interface's first bulk endpoint that way.
fn bulk_address(interface: &Interface, direction: Direction) -> Option<u8> {
    let descriptor = interface.descriptor()?;
    let mut bulk = descriptor.endpoints().filter(|e| e.transfer_type() == TransferType::Bulk);
    bulk.find(|e| e.direction() == direction).map(|e| e.address())
}

/// What a protocol speaks over: whole messages out, packets in.
pub trait Transport {
    // Sends all of `data`, waiting as long as the device takes.
    fn send(&mut self, data: &[u8]) -> Result<()>;
    // Receives one packet, waiting `patience` at the longest.
    fn receive_within(&mut self, patience: Duration) -> Result<Vec<u8>>;
    // Receives one packet, waiting as long as the device takes.
    fn receive(&mut self) -> Result<Vec<u8>> {
        self.receive_within(Duration::MAX)
    }
}

/// A claimed interface's bulk IN and bulk OUT endpoints.
pub struct Pipe {
    // The device, for requests to an interface left unclaimed: the preloader's control interface, which macOS's driver keeps.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    device: Device,
    input: Endpoint<Bulk, In>,
    output: Endpoint<Bulk, Out>,
}

// Opens `info` and claims its first interface accepted by `pick`, detaching any kernel driver; the device's ids, for what goes wrong with it.
fn claim(info: &DeviceInfo, pick: impl Fn(&InterfaceInfo) -> bool) -> Result<(Device, Interface, String)> {
    let id = format!("{:04x}:{:04x}", info.vendor_id(), info.product_id());
    let number = info
        .interfaces()
        .find(|interface| pick(interface))
        .map(InterfaceInfo::interface_number)
        .ok_or_else(|| format!("USB device {id} has no interface of the expected class"))?;
    let device = info.open().wait().map_err(|e| format!("cannot open USB device {id}: {e}{DENIED_HINT}"))?;
    let interface = device.detach_and_claim_interface(number).wait()?;
    Ok((device, interface, id))
}

impl Pipe {
    // Claims the first interface of `info` accepted by `pick` and its bulk endpoints.
    fn open(info: &DeviceInfo, pick: impl Fn(&InterfaceInfo) -> bool) -> Result<Pipe> {
        let (device, interface, id) = claim(info, pick)?;
        let number = interface.interface_number();
        let missing = || format!("USB device {id} interface {number} lacks a bulk endpoint pair");
        let input = interface.endpoint::<Bulk, In>(bulk_address(&interface, Direction::In).ok_or_else(missing)?)?;
        let output = interface.endpoint::<Bulk, Out>(bulk_address(&interface, Direction::Out).ok_or_else(missing)?)?;
        Ok(Pipe { device, input, output })
    }
}

impl Transport for Pipe {
    fn send(&mut self, data: &[u8]) -> Result<()> {
        for chunk in data.chunks(PAYLOAD_CHUNK) {
            self.output.transfer_blocking(chunk.to_vec().into(), Duration::MAX).status?;
        }
        Ok(())
    }

    fn receive_within(&mut self, patience: Duration) -> Result<Vec<u8>> {
        let packet = self.input.max_packet_size();
        let done = self.input.transfer_blocking(Buffer::new(packet), patience);
        done.status?;
        Ok(done.buffer[..].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names() {
        let (serial, port) = (Some("ABCDEF0123456789"), "5-4.2.3");
        assert!(named(None, serial, port) && named(None, None, port), "nothing named takes every device");
        assert!(named(Some("5-4.2.3"), serial, port), "its port");
        assert!(!named(Some("5-4.2.2"), serial, port) && !named(Some("5-4.2"), serial, port), "and no other");
        assert!(named(Some("6789"), serial, port) && named(Some("ABCDEF0123456789"), serial, port), "its serial's end");
        assert!(!named(Some("ABCD"), serial, port), "its serial's beginning is no name: two R1s can share it");
        assert!(named(Some("5-4.2.3"), None, port) && !named(Some("6789"), None, port), "one without a serial number");
        assert_eq!(Choice::new(Some(String::new())).0, None, "an empty name names nothing");
        assert!(
            on_port(Some("5-4.2.3"), port) && !on_port(Some("5-4.2.2"), port) && !on_port(Some("6-1"), port),
            "a device that tells no serial number is the one named by its port alone"
        );
        assert!(on_port(Some("6789"), port) && on_port(None, port), "and may be any where no port is named");
        assert!(is_port("5-4.2.3") && is_port("1-1") && !is_port("5-") && !is_port("ABCD-12") && !is_port("5-4..2"));
    }

    #[test]
    fn bus_numbers() {
        if cfg!(target_os = "macos") {
            assert_eq!((bus_number("00"), bus_number("02"), bus_number("1a")), ("0".into(), "2".into(), "26".into()));
        } else {
            assert_eq!((bus_number("005"), bus_number("012")), ("5".into(), "12".into()));
        }
        assert!(is_port(&format!("{}-1", bus_number("00"))), "bus 0 is a port's name too");
    }

    #[test]
    fn udev_rule_ids() {
        use r1_funcore::hw::usb::{PRODUCT_ID, PRODUCT_NAME, VENDOR_ID};
        let rule = include_str!("../../../../udev/70-r1metal.rules");
        let guises = [(MEDIATEK, preloader::PRODUCT_ID), (MEDIATEK, fastboot::PRODUCT_ID), (VENDOR_ID, PRODUCT_ID)];
        for (vendor, product) in guises {
            let ids = format!("ATTR{{idVendor}}==\"{vendor:04x}\", ATTR{{idProduct}}==\"{product:04x}\"");
            assert!(rule.lines().any(|line| line.contains(&ids) && line.contains("TAG+=\"uaccess\"")), "{ids}");
        }
        assert!(rule.contains(&format!("ATTR{{product}}==\"{PRODUCT_NAME}\"")), "the console by its name");
    }
}
