//! The setup of MediaTek's WMT driver before it powers the connectivity core: the link to the chip, the patch files, the factory's calibration.

use crate::base::bytes::be32;
use crate::wifi::station::ADDRESS_BYTES;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The link as the interface call takes it: BTIF in the low four bits, FM over that same link in the four above.
pub const INTERFACE: u64 = 0x23;
/// Where the boot image's ramdisk holds the files the drivers ask the kernel for, as a path from the root.
pub const FIRMWARE: &str = "vendor/firmware";

/// The programs of the chip's processor, Bluetooth half and WiFi half, copied into the shared memory at every power-on; calibration fails without the Bluetooth one.
pub const ROM_PATCHES: [&str; 3] =
    ["soc1_0_ram_mcu_1_1_hdr.bin", "soc1_0_ram_bt_1_1_hdr.bin", "soc1_0_ram_wifi_1_1_hdr.bin"];
/// The classic patch, which the driver sends down the link at every power-on.
pub const PATCH: &str = "soc1_0_patch_mcu_1_1_hdr.bin";
/// The count of classic patch files.
pub const PATCHES: u32 = 1;

/// The text that powers the core for WiFi through the adaptor's node.
pub const ON: &[u8] = b"1";
/// The text that takes the core's power away.
pub const OFF: &[u8] = b"0";
/// The answer that tells the driver its request is done; any other tells it failed.
pub const DONE: &[u8] = b"ok";
/// The longest request of the driver, in bytes.
pub const QUESTION_BYTES: usize = 255;
/// The NVRAM file of the factory's WiFi settings.
pub const WIFI_FILE: &str = "APCFG/APRDEB/WIFI";
pub(crate) const WIFI_AT: usize = 4;
const CALIBRATION_HEAD: &[u8] = b"WR-BUF:NVRAM";
const CALIBRATION_BYTES: usize = 8192;
const COUNTRY_BYTES: usize = 2;

pub const ROM_HEAD_BYTES: usize = 48;
/// The head of a classic patch file, the same as a ROM patch's up to the address.
pub const PATCH_HEAD_BYTES: usize = 28;
const ADDRESS_AT: usize = 24;
const KIND_AT: usize = 28;

/// The error of a power-on where the drivers could not be set up.
pub const UNUSABLE: &str = "the driver could not be set up, as logged before; it takes a restart";

/// The log line of the drivers started for that chip.
pub fn to_started_line(chip: i32) -> String {
    format!("radio: drivers started for chip {chip:#x}")
}

/// The error text of a calibration file of that size.
pub fn to_unfit_text(bytes: usize) -> String {
    format!("a file of {bytes} bytes, of which gen4m takes nothing")
}

/// The log line of a calibration handed to the driver.
pub fn to_handed_line(bytes: usize) -> String {
    format!("radio: the factory's calibration of {bytes} bytes handed to the driver")
}

/// The error text of a ROM patch file with no head.
pub fn to_no_head_text(name: &str) -> String {
    format!("{name}: no head of a ROM patch")
}

/// The log line of a request from the driver.
pub fn to_question_line(question: &[u8]) -> String {
    format!("radio: the driver asks {:?}", String::from_utf8_lossy(question))
}

/// The log line of a radio on after that many milliseconds.
pub fn to_on_line(ms: u128) -> String {
    format!("radio: on in {ms} ms")
}

/// The log line of a radio off after that many milliseconds.
pub fn to_off_line(ms: u128) -> String {
    format!("radio: off in {ms} ms")
}

/// A patch as the driver takes it: a ROM patch's part of the chip or a classic patch's number, and its address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Patch {
    pub number: u32,
    pub address: [u8; 4],
}

fn parse_address(head: &[u8]) -> Option<(u8, [u8; 4])> {
    let field = head.get(ADDRESS_AT..ADDRESS_AT + 4)?;
    Some((field[0], [0, field[1], field[2], field[3]]))
}

/// Reads the head of a ROM patch file.
pub fn parse_rom_patch(head: &[u8]) -> Option<Patch> {
    Some(Patch { number: be32(head, KIND_AT)?, address: parse_address(head)?.1 })
}

/// Why the head of the classic patch file cannot be used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PatchError {
    NoHead,
    // It names a number among more files than there are.
    Among { count: u32, number: u32 },
}

impl PatchError {
    fn message(self) -> String {
        match self {
            PatchError::NoHead => "no head of a patch".into(),
            PatchError::Among { count, number } => {
                format!("number {number} of {count} patches, and one file is all there is")
            }
        }
    }

    /// The error text with the file's name.
    pub fn to_file_text(self) -> String {
        format!("{PATCH}: {}", self.message())
    }
}

/// Reads the head of the classic patch file, which has to name the one file there is.
pub fn parse_patch(head: &[u8]) -> Result<Patch, PatchError> {
    let (counts, address) = parse_address(head).ok_or(PatchError::NoHead)?;
    let (count, number) = (u32::from(counts >> 4), u32::from(counts & 0xF));
    if count == PATCHES && number == PATCHES {
        Ok(Patch { number, address })
    } else {
        Err(PatchError::Among { count, number })
    }
}

/// The bytes for the adaptor's node that hand gen4m the factory's file: address and country code zeroed, so that the driver keeps its own address and names no country; none for a file too short to hold them or longer than gen4m keeps.
pub fn to_calibration(file: &[u8]) -> Option<Vec<u8>> {
    let zeroed_end = WIFI_AT + ADDRESS_BYTES + COUNTRY_BYTES;
    if file.len() < zeroed_end || file.len() > CALIBRATION_BYTES {
        return None;
    }
    let mut message = [CALIBRATION_HEAD, file].concat();
    message[CALIBRATION_HEAD.len() + WIFI_AT..CALIBRATION_HEAD.len() + zeroed_end].fill(0);
    Some(message)
}

/// The drivers' setup since the device started; neither start nor configuration can be redone before a restart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setup {
    Unconfigured,
    Configured,
    Failed,
}

impl Setup {
    /// True before the drivers' first start, which draws on the random generator.
    pub const fn is_unconfigured(self) -> bool {
        matches!(self, Setup::Unconfigured)
    }

    /// True where the drivers cannot be used before a restart.
    pub const fn is_failed(self) -> bool {
        matches!(self, Setup::Failed)
    }

    /// The setup after a power-on: a node that does not open for configured drivers is tried again at the next request.
    pub const fn next(self, is_prepared: bool) -> Setup {
        match self {
            Setup::Unconfigured if is_prepared => Setup::Configured,
            Setup::Unconfigured => Setup::Failed,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(tail: &[u8]) -> alloc::vec::Vec<u8> {
        [b"230616120501000\0ALPS\x8a\x00\x8a\x00".as_slice(), tail].concat()
    }

    #[test]
    fn patch_heads() {
        assert_eq!(
            [[0x11, 0, 0, 0xf0, 0, 0, 0, 4], [0x11, 0, 0x08, 0xf0, 0, 0, 0, 0], [0x11, 0, 0x14, 0xf0, 0, 0, 0, 3]]
                .map(|tail| parse_rom_patch(&head(&[&tail[..], &[0; 4]].concat()))),
            [
                Some(Patch { number: 4, address: [0, 0, 0, 0xf0] }),
                Some(Patch { number: 0, address: [0, 0, 0x08, 0xf0] }),
                Some(Patch { number: 3, address: [0, 0, 0x14, 0xf0] })
            ],
            "the processor's at the start of the shared memory, Bluetooth's, WiFi's"
        );
        assert_eq!(
            [&[0x11, 0xc0, 0x01, 0x00][..], &[0x32, 0xc0, 0x01, 0x00], &[0x12, 0, 0, 0], &[0x11]]
                .map(|tail| parse_patch(&head(tail))),
            [
                Ok(Patch { number: 1, address: [0, 0xc0, 0x01, 0x00] }),
                Err(PatchError::Among { count: 3, number: 2 }),
                Err(PatchError::Among { count: 1, number: 2 }),
                Err(PatchError::NoHead)
            ],
            "the one file there is is told of, and a head that names others is not"
        );
        assert_eq!(
            [PatchError::NoHead.message(), PatchError::Among { count: 3, number: 2 }.message()],
            ["no head of a patch", "number 2 of 3 patches, and one file is all there is"]
        );
        assert_eq!(head(&[0; 24]).len(), ROM_HEAD_BYTES);
        assert_eq!(head(&[0; 4]).len(), PATCH_HEAD_BYTES);
        assert_eq!(parse_rom_patch(&head(&[0x11, 0, 0, 0xf0])), None, "a head that ends too soon says nothing");
    }

    #[test]
    fn log_text() {
        assert_eq!(to_started_line(0x6765), "radio: drivers started for chip 0x6765");
        assert_eq!(to_unfit_text(9000), "a file of 9000 bytes, of which gen4m takes nothing");
        assert_eq!(to_handed_line(1024), "radio: the factory's calibration of 1024 bytes handed to the driver");
        assert_eq!(to_no_head_text(ROM_PATCHES[0]), "soc1_0_ram_mcu_1_1_hdr.bin: no head of a ROM patch");
        assert_eq!(to_question_line(b"patch\n"), "radio: the driver asks \"patch\\n\"", "as text, its signs escaped");
        assert_eq!((to_on_line(412), to_off_line(38)), ("radio: on in 412 ms".into(), "radio: off in 38 ms".into()));
        assert_eq!(PatchError::NoHead.to_file_text(), "soc1_0_patch_mcu_1_1_hdr.bin: no head of a patch");
    }

    #[test]
    fn setup_once() {
        assert_eq!(
            [Setup::Unconfigured.next(true), Setup::Unconfigured.next(false)],
            [Setup::Configured, Setup::Failed],
            "the first power-on starts and tells them"
        );
        assert_eq!(
            [Setup::Configured.next(false), Setup::Failed.next(true)],
            [Setup::Configured, Setup::Failed],
            "a node that does not open later changes nothing, and nothing makes them usable again"
        );
    }

    #[test]
    fn calibration_bytes() {
        let file: alloc::vec::Vec<u8> = (1..=40).collect();
        let message = to_calibration(&file).unwrap();
        assert_eq!((&message[..12], message.len()), (&b"WR-BUF:NVRAM"[..], 12 + file.len()), "the adaptor's head");
        assert_eq!(message[12..16], file[..4], "the versions stay");
        assert_eq!(message[16..24], [0; 8], "the address and the country code are zeroed");
        assert_eq!(message[24..], file[12..], "the rest is the factory's");
        assert_eq!(to_calibration(&file[..11]), None, "too short to hold the country code");
        assert!(to_calibration(&[0; CALIBRATION_BYTES]).is_some());
        assert_eq!(to_calibration(&[0; CALIBRATION_BYTES + 1]), None, "longer than gen4m keeps");
    }
}
