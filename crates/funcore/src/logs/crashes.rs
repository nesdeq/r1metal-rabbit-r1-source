use crate::base::checksum::crc32;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const ORDERLY_ENDS: [&str; 3] = ["reboot: Restarting system", "reboot: System halted", "reboot: Power down"];

fn is_orderly_end(record: &[u8]) -> bool {
    let last = record.trim_ascii_end().rsplit(|byte| *byte == b'\n').next().unwrap_or_default();
    ORDERLY_ENDS.iter().any(|end| last.windows(end.len()).any(|at| at == end.as_bytes()))
}

/// The list to write where it differs from `previous`.
pub fn to_changed_list<'a>(list: &'a str, previous: &str) -> Option<&'a str> {
    (list != previous).then_some(list)
}

/// The crash store's records seen, each by name and mark, and how many to copy.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Crashes(Vec<(String, String)>, u32);

impl Crashes {
    /// The records of a list's text; a line without a name and a mark is skipped.
    pub fn parse(text: &str) -> Crashes {
        let lines = text.lines().filter_map(|line| line.split_once(' '));
        Crashes(lines.map(|(name, mark)| (name.into(), mark.into())).collect(), 0)
    }

    /// How many records `add_record` said to copy.
    pub const fn copy_count(&self) -> u32 {
        self.1
    }

    fn mark(record: &[u8]) -> String {
        format!("{} {:08x}", record.len(), crc32(record))
    }

    /// Lists a record; true if it is to be copied: not one listed with its mark, as the bootloader hands the last crash back at every start, nor one whose boot went down in order.
    pub fn add_record(&mut self, name: &str, record: &[u8]) -> bool {
        let mark = Crashes::mark(record);
        if self.0.iter().any(|(listed, listed_mark)| listed == name && *listed_mark == mark) {
            return false;
        }
        self.0.retain(|(listed, _)| listed != name);
        self.0.push((name.into(), mark));
        let is_copied = !is_orderly_end(record);
        self.1 += u32::from(is_copied);
        is_copied
    }

    /// The list's text, sorted by name.
    pub fn into_text(mut self) -> String {
        self.0.sort();
        self.0.iter().map(|(name, mark)| format!("{name} {mark}\n")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_copied_once() {
        let mut crashes = Crashes::default();
        let panic = b"[    9.1] Kernel panic - not syncing: Fatal exception\n";
        assert!(crashes.add_record("dmesg-ramoops-0", panic), "a boot that died is copied");
        assert!(!crashes.add_record("dmesg-ramoops-0", panic), "and handed back at the next start, passed over");
        let restart = b"[  206.871041]  (0)[307:init]reboot: Restarting system\n";
        assert!(!crashes.add_record("console-ramoops-0", restart), "the console of a warm restart is not copied");
        assert!(
            crashes.clone().into_text().contains(&format!("console-ramoops-0 {}", Crashes::mark(restart))),
            "but listed, so the next start passes it over"
        );
        assert!(
            crashes.add_record("dmesg-ramoops-0", b"a newer crash"),
            "a new record in the place of one listed is copied"
        );
    }

    #[test]
    fn copy_count_and_changes() {
        let mut crashes = Crashes::parse("");
        assert!(crashes.add_record("a", b"panic") && !crashes.add_record("a", b"panic"));
        assert_eq!(crashes.copy_count(), 1);
        assert_eq!((to_changed_list("a", "a"), to_changed_list("b", "a")), (None, Some("b")));
    }

    #[test]
    fn orderly_ends() {
        for end in
            ["reboot: Restarting system with command 'bootloader'", "reboot: System halted", "reboot: Power down"]
        {
            assert!(is_orderly_end(format!("[    1.0]  (0)[1:init]{end}\n").as_bytes()), "{end}");
        }
        let panic = b"[    9.1] Kernel panic - not syncing: Fatal exception\n[    9.1] Rebooting in 1 seconds..\n";
        let later = b"[    9.0] reboot: Restarting system\n[    9.2] Unable to handle kernel paging request\n";
        assert!(!is_orderly_end(panic) && !is_orderly_end(later) && !is_orderly_end(b""), "only the last line decides");
    }

    #[test]
    fn list_round_trip() {
        assert_eq!(Crashes::mark(b"last words"), "10 e9429783", "the checksum is zlib's");
        let mut crashes = Crashes::parse("dmesg-ramoops-0 5 9ea78192\nnoise\nconsole-ramoops-0 10 e9429783\n");
        assert!(
            !crashes.add_record("console-ramoops-0", b"last words"),
            "a record listed with its mark is passed over"
        );
        assert!(crashes.add_record("console-ramoops-0", b"later words"), "one with another mark is not");
        assert_eq!(
            crashes.clone().into_text(),
            "console-ramoops-0 11 f12ccbfe\ndmesg-ramoops-0 5 9ea78192\n",
            "a record listed anew in place of its old mark, the list in the order of the names"
        );
        assert_eq!(Crashes::parse(&crashes.clone().into_text()), Crashes::parse(&crashes.into_text()));
        assert_eq!(Crashes::parse("").into_text(), "");
    }
}
