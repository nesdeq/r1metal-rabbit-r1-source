//! The sensor hub's accelerometer: the batch command and its records.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const RATE: &str = "/sys/class/sensor/m_acc_misc/accbatch";
const SWITCH: &str = "/sys/class/sensor/m_acc_misc/accactive";
const ON: &str = "1";
const OFF: &str = "0";
pub(super) const PERIOD_NS: u64 = 20_000_000;
/// struct sensor_event, packed: a time, four bytes of which the second says what the record is, six words.
pub const RECORD_BYTES: usize = 36;
const ACTION_OFFSET: usize = 9;
const WORDS_OFFSET: usize = 12;
const WORD_BYTES: usize = 4;
pub(super) const DATA_ACTION: u8 = 0;
/// Records taken from the queue at once.
pub const RECORDS_PER_READ: usize = 16;

pub(super) fn parse_reading(record: &[u8; RECORD_BYTES]) -> Option<[i32; 3]> {
    if record[ACTION_OFFSET] != DATA_ACTION {
        return None;
    }
    let word = |index: usize| {
        let at = WORDS_OFFSET + index * WORD_BYTES;
        i32::from_le_bytes(record[at..at + WORD_BYTES].try_into().expect("four bytes of a record"))
    };
    Some([word(0), word(1), word(2)])
}

// Handle, flag, the period and the latency, in nanoseconds.
fn batch_command() -> String {
    format!("0,0,{PERIOD_NS},0")
}

/// The files that switch the accelerometer, each with its text, in their order.
pub fn to_switch_writes(is_on: bool) -> Vec<(&'static str, String)> {
    if is_on { alloc::vec![(RATE, batch_command()), (SWITCH, ON.into())] } else { alloc::vec![(SWITCH, OFF.into())] }
}

#[cfg(test)]
pub(super) fn record(action: u8, words: [i32; 3]) -> [u8; RECORD_BYTES] {
    let mut record = [0u8; RECORD_BYTES];
    record[..8].copy_from_slice(&1_329_169_000_000i64.to_le_bytes());
    record[ACTION_OFFSET] = action;
    for (index, word) in words.into_iter().enumerate() {
        record[WORDS_OFFSET + index * WORD_BYTES..][..WORD_BYTES].copy_from_slice(&word.to_le_bytes());
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_records() {
        assert_eq!(
            parse_reading(&record(DATA_ACTION, [-136, 9811, 50])),
            Some([-136, 9811, 50]),
            "as the device sent it standing upright"
        );
        assert_eq!(parse_reading(&record(1, [0; 3])), None, "the end of a flush");
        assert_eq!(parse_reading(&record(2, [12, -7, 3])), None, "a bias");
        assert_eq!(WORDS_OFFSET + 6 * WORD_BYTES, RECORD_BYTES);
    }

    #[test]
    fn batch_rate() {
        assert_eq!(batch_command(), "0,0,20000000,0");
        const { assert!(PERIOD_NS >= crate::gfx::refresh::REFRESH_NS) };
    }
}
