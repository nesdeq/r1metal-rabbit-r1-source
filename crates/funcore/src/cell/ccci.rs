//! What the kernel's CCCI driver hands over and takes: monitor messages, status events, boot data, the CCB control area, the SBP code.

use crate::base::bytes::{le32, until_zero};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// A monitor message: `{0xFFFFFFFF, message, 0xF0000000, argument}` (`ccci_fsm_monitor.c`).
pub const MONITOR_BYTES: usize = 16;
/// A status event: a 64-bit `timeval`, the modem's id, the event, a reason of 32 bytes (`ccci_util_broadcast.c`).
pub const STATUS_BYTES: usize = 56;
/// The kernel's `struct ccci_ccb_config`.
pub const CCB_CONFIG_BYTES: usize = 24;
/// The three CCB users, counted from the first (`SMEM_USER_CCB_DHL`, `MD_MONITOR`, `META`).
pub const CCB_USERS: [u32; 3] = [0, 1, 2];
/// `SET_BOOT_DATA`'s words.
pub const BOOT_WORDS: usize = 16;
/// The flag of `DO_STOP_MD` for a stop after `AT+EPOF`.
pub const FLIGHT_STOP: u32 = 1;
/// The files of the SBP code in the `nvdata` partition, `CXP_SBP` and `MD_SBP`, as MediaTek's `ccci_mdinit` reads them.
pub const SBP_FILES: [&str; 2] = ["APCFG/APRDCL/CXP_SBP", "APCFG/APRDCL/MD_SBP"];
/// Why a start writes no CCB descriptors.
pub const CCB_OVERRUN: &str = "the kernel's CCB descriptors reach past the control area";

const MONITOR_MARK: u32 = 0xFFFF_FFFF;
const MONITOR_CHANNEL: u32 = 0xF000_0000;
const MESSAGE_BASE: u32 = 0xFAF5_0000;
const EVENT_AT: usize = 20;
const REASON_AT: usize = 24;
const HANDSHAKE: u32 = 9;
const READY: u32 = 10;
const EXCEPTION: u32 = 11;
const STOP: u32 = 12;
// A CCB descriptor in the control area.
const DESCRIPTOR_BYTES: usize = 64;
const DESCRIPTOR_MARK: u32 = 0xEEFF_0011;
const DL_PAGE_AT: usize = 0x14;
const DL_BUFFER_AT: usize = 0x18;
const DL_MARK_AT: usize = 0x1C;
const UL_PAGE_AT: usize = 0x34;
const UL_BUFFER_AT: usize = 0x38;
const UL_MARK_AT: usize = 0x3C;
// Dump flag that keeps the kernel's own (`MD_DBG_DUMP_INVALID`).
const KERNEL_DUMP: u32 = 0xFFFF_FFFF;
const SBP_WORD: usize = 1;
const DUMP_WORD: usize = 2;
const RAT_WORD: usize = 5;
const RAT_BYTES: usize = 24;
// The RAT string Rabbit OS hands its modem (`ro.vendor.mtk_ps1_rat`): every RAT of the image.
pub(super) const RAT: &str = "C/Lf/Lt/W/T/G";
const ENABLED_PREFIX: &str = "md1:";

// A message on the monitor node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Message {
    ForceStop,
    FlightStop,
    ForceStart,
    FlightStart,
    Reset,
    Exception,
    // The modem asks for the battery's voltage, which `SEND_BATTERY_INFO` hands it.
    Battery,
    StoreType,
    ConfigUpdate,
    RandomPattern,
    Other(u32),
}

impl Message {
    fn from_word(word: u32) -> Message {
        match word.wrapping_sub(MESSAGE_BASE) {
            1 => Message::ForceStop,
            2 => Message::FlightStop,
            3 => Message::ForceStart,
            4 => Message::FlightStart,
            5 => Message::Reset,
            6 => Message::Exception,
            7 => Message::Battery,
            8 => Message::StoreType,
            9 => Message::ConfigUpdate,
            10 => Message::RandomPattern,
            _ => Message::Other(word),
        }
    }
}

// The message in a record of the monitor node; none for one not of the monitor's channel.
fn parse_message(record: &[u8]) -> Option<Message> {
    let (mark, message, channel) = (le32(record, 0)?, le32(record, 4)?, le32(record, 8)?);
    (record.len() == MONITOR_BYTES && mark == MONITOR_MARK && channel == MONITOR_CHANNEL)
        .then(|| Message::from_word(message))
}

// The log line of a monitor message the firmware answers with nothing.
fn to_message_line(message: Message) -> String {
    format!("cell: the kernel's monitor says {message:?}, which nothing here asks for")
}

/// What the monitor's thread does with a record of the monitor node.
#[derive(PartialEq, Eq, Debug)]
pub enum Answer {
    // Hands the modem the battery's voltage it asked for (`SEND_BATTERY_INFO`).
    Battery,
    // Answers nothing and logs the line.
    Log(String),
}

/// The answer to a record of the monitor node; none for one not of the monitor's channel.
pub fn answer_message(record: &[u8]) -> Option<Answer> {
    parse_message(record).map(|message| match message {
        Message::Battery => Answer::Battery,
        message => Answer::Log(to_message_line(message)),
    })
}

/// An event of the modem's state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    // Handshake 1: the modem runs and asked for its runtime data.
    Handshake,
    Ready,
    Exception,
    Stop,
    // A request someone made, before it is carried out.
    Other(u32),
}

/// The event in a record of the status node, with its reason.
pub fn parse_event(record: &[u8]) -> Option<(Event, String)> {
    let kind = le32(record, EVENT_AT)?;
    let reason = String::from_utf8_lossy(until_zero(record.get(REASON_AT..STATUS_BYTES)?)).into_owned();
    let event = match kind {
        HANDSHAKE => Event::Handshake,
        READY => Event::Ready,
        EXCEPTION => Event::Exception,
        STOP => Event::Stop,
        other => Event::Other(other),
    };
    Some((event, reason))
}

/// True if the kernel's boot file names a modem the bootloader loaded; a boot a cable started has none.
pub fn is_enabled(boot_file: &str) -> bool {
    boot_file.trim_start().starts_with(ENABLED_PREFIX)
}

// The SBP code as MediaTek's `ccci_mdinit` takes it: `CXP_SBP`'s number where the file is, else `MD_SBP`'s first word, else 0.
pub(super) fn to_sbp(cxp: Option<&[u8]>, md: Option<&[u8]>) -> u32 {
    if let Some(code) = cxp.and_then(|text| u32::try_from(to_c_number(text)).ok()) {
        return code;
    }
    md.and_then(|record| le32(record, 0)).filter(|&code| code as i32 > 0).unwrap_or(0)
}

// C's `atoi`: spaces, a sign, then digits up to the first that is none.
fn to_c_number(text: &[u8]) -> i64 {
    let text = text.trim_ascii_start();
    let (sign, digits) = match text.first() {
        Some(b'-') => (-1, &text[1..]),
        Some(b'+') => (1, &text[1..]),
        _ => (1, text),
    };
    let number = digits
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .fold(0i64, |number, digit| number.saturating_mul(10).saturating_add(i64::from(digit - b'0')));
    sign * number
}

// `SET_BOOT_DATA`'s words: idle logging, the SBP code, the kernel's dump flag, the RAT string (`MD_CFG_*` in `ccci_modem.h`).
pub(super) fn boot_data(sbp: u32) -> [u32; BOOT_WORDS] {
    let mut words = [0u32; BOOT_WORDS];
    words[SBP_WORD] = sbp;
    words[DUMP_WORD] = KERNEL_DUMP;
    let mut rat = [0u8; RAT_BYTES];
    rat[..RAT.len()].copy_from_slice(RAT.as_bytes());
    for (word, bytes) in words[RAT_WORD..].iter_mut().zip(rat.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
    words
}

/// One entry of the kernel's CCB table: its user and the sizes of its downlink and uplink pages and buffers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CcbConfig {
    user: u32,
    dl_page: u32,
    ul_page: u32,
    dl_buffer: u32,
    ul_buffer: u32,
}

/// The entry `GET_CCB_CONFIG` answered.
pub fn parse_ccb_config(record: &[u8]) -> Option<CcbConfig> {
    Some(CcbConfig {
        user: le32(record, 0)?,
        dl_page: le32(record, 8)?,
        ul_page: le32(record, 12)?,
        dl_buffer: le32(record, 16)?,
        ul_buffer: le32(record, 20)?,
    })
}

/// The words of a CCB user's descriptors, each with its place in a control area of `area` bytes, from the user's descriptor `first` on, as Rabbit OS's `ccci_ccb_init_users` writes them; none where they reach past the area.
pub fn ccb_words(configs: &[CcbConfig], user: u32, first: u32, area: u32) -> Option<Vec<(usize, u32)>> {
    let mut descriptors = Vec::new();
    for config in configs.iter().filter(|config| config.user == user) {
        let mut descriptor = [0u8; DESCRIPTOR_BYTES];
        for (at, value) in [
            (DL_PAGE_AT, config.dl_page),
            (DL_BUFFER_AT, config.dl_buffer),
            (DL_MARK_AT, DESCRIPTOR_MARK),
            (UL_PAGE_AT, config.ul_page),
            (UL_BUFFER_AT, config.ul_buffer),
            (UL_MARK_AT, DESCRIPTOR_MARK),
        ] {
            descriptor[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        descriptors.extend_from_slice(&descriptor);
    }
    let start = first as usize * DESCRIPTOR_BYTES;
    let words = descriptors.as_chunks::<4>().0.iter().enumerate();
    (start + descriptors.len() <= area as usize)
        .then(|| words.map(|(index, word)| (start + index * 4, u32::from_le_bytes(*word))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn monitor(message: u32, channel: u32) -> Vec<u8> {
        [MONITOR_MARK, message, channel, 0].iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    #[test]
    fn monitor_messages() {
        assert_eq!(parse_message(&monitor(0xFAF5_0007, MONITOR_CHANNEL)), Some(Message::Battery));
        assert_eq!(parse_message(&monitor(0xFAF5_0001, MONITOR_CHANNEL)), Some(Message::ForceStop));
        assert_eq!(parse_message(&monitor(0xFAF5_000A, MONITOR_CHANNEL)), Some(Message::RandomPattern));
        assert_eq!(parse_message(&monitor(0x1234, MONITOR_CHANNEL)), Some(Message::Other(0x1234)));
        assert_eq!(parse_message(&monitor(0xFAF5_0007, 2)), None, "another channel's");
        assert_eq!(parse_message(&monitor(0xFAF5_0007, MONITOR_CHANNEL)[..12]), None, "cut short");
    }

    #[test]
    fn monitor_answers() {
        assert_eq!(
            answer_message(&monitor(0xFAF5_0007, MONITOR_CHANNEL)),
            Some(Answer::Battery),
            "the voltage asked for"
        );
        assert_eq!(
            answer_message(&monitor(0xFAF5_0001, MONITOR_CHANNEL)),
            Some(Answer::Log("cell: the kernel's monitor says ForceStop, which nothing here asks for".into())),
            "any other message is only told"
        );
        assert_eq!(answer_message(&monitor(0xFAF5_0007, 2)), None, "and another channel's record not even that");
    }

    fn status(kind: u32, reason: &str) -> Vec<u8> {
        let mut record = vec![0u8; STATUS_BYTES];
        record[EVENT_AT..EVENT_AT + 4].copy_from_slice(&kind.to_le_bytes());
        record[REASON_AT..REASON_AT + reason.len()].copy_from_slice(reason.as_bytes());
        record
    }

    #[test]
    fn status_events() {
        assert_eq!(parse_event(&status(10, "md1")), Some((Event::Ready, "md1".into())));
        assert_eq!(parse_event(&status(11, "")).map(|event| event.0), Some(Event::Exception));
        assert_eq!(parse_event(&status(12, "")).map(|event| event.0), Some(Event::Stop));
        assert_eq!(parse_event(&status(9, "")).map(|event| event.0), Some(Event::Handshake));
        assert_eq!(parse_event(&status(4, "")).map(|event| event.0), Some(Event::Other(4)));
        assert_eq!(parse_event(&status(10, "")[..40]), None);
    }

    #[test]
    fn enabled_modem() {
        assert!(is_enabled("md1:0\n") && is_enabled("md1:4"));
        assert!(!is_enabled("") && !is_enabled("\n"), "the kernel writes nothing for a modem it did not enable");
    }

    #[test]
    fn sbp_order() {
        let md = 9u32.to_le_bytes();
        assert_eq!(to_sbp(Some(b"12\n"), Some(&md)), 12, "the carrier file first");
        assert_eq!(to_sbp(Some(b"0"), Some(&md)), 0, "even at 0");
        assert_eq!(to_sbp(Some(b"-1"), Some(&md)), 9, "a negative number counts as no file");
        assert_eq!(to_sbp(None, Some(&md)), 9);
        assert_eq!(to_sbp(None, Some(&0xFFFF_FFFFu32.to_le_bytes())), 0, "a word read as a negative number is none");
        assert_eq!((to_sbp(None, Some(&[1, 2])), to_sbp(None, None)), (0, 0));
        assert_eq!((to_c_number(b"  +7x"), to_c_number(b"x7"), to_c_number(b"")), (7, 0, 0));
    }

    #[test]
    fn boot_words() {
        let words = boot_data(5);
        assert_eq!((words[0], words[1], words[2], words[3], words[4], words[11]), (0, 5, 0xFFFF_FFFF, 0, 0, 0));
        let rat: Vec<u8> = words[RAT_WORD..RAT_WORD + 6].iter().flat_map(|word| word.to_le_bytes()).collect();
        assert_eq!(until_zero(&rat), b"C/Lf/Lt/W/T/G");
        assert!(words[12..].iter().all(|&word| word == 0));
    }

    #[test]
    fn ccb_area() {
        let mut record = vec![0u8; CCB_CONFIG_BYTES];
        for (at, value) in [(0, 1u32), (8, 512), (12, 1024), (16, 16384), (20, 16384)] {
            record[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        record[4] = 7;
        let config = parse_ccb_config(&record).unwrap();
        assert_eq!(config, CcbConfig { user: 1, dl_page: 512, ul_page: 1024, dl_buffer: 16384, ul_buffer: 16384 });
        let other = CcbConfig { user: 0, ..config };
        let configs = [other, config, config];
        let (start, end) = (3 * DESCRIPTOR_BYTES, 5 * DESCRIPTOR_BYTES);
        let words = ccb_words(&configs, 1, 3, end as u32).unwrap();
        assert_eq!(
            words.iter().map(|word| word.0).collect::<Vec<_>>(),
            (start..end).step_by(4).collect::<Vec<_>>(),
            "one descriptor per entry of the user, word by word from the user's first on"
        );
        let word = |at: usize| words[at / 4].1;
        assert_eq!(
            [word(0x14), word(0x18), word(0x1C), word(0x34), word(0x38), word(0x3C)],
            [512, 16384, DESCRIPTOR_MARK, 1024, 16384, DESCRIPTOR_MARK]
        );
        let marked = [0x14, 0x18, 0x1C, 0x34, 0x38, 0x3C];
        assert!(
            (0..DESCRIPTOR_BYTES).step_by(4).filter(|at| !marked.contains(at)).all(|at| word(at) == 0),
            "every other byte is 0"
        );
        assert_eq!(ccb_words(&configs, 1, 3, end as u32 - 1), None, "descriptors past the area are not written");
        assert_eq!(ccb_words(&configs, 2, 5, end as u32), Some(Vec::new()), "a user with no entry writes nothing");
        assert!(parse_ccb_config(&record[..20]).is_none());
    }
}
