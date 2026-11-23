//! Rabbit's MediaTek thermal policy and what r1metal writes of it: all but the second charger's settings, the steering in the kernel.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// `/vendor/etc/.tp/thermal.conf` of Rabbit OS v0.8.293, decoded and otherwise untouched.
pub const POLICY: &str = include_str!("../../../assets/thermal/thermal.conf");
const STEERING: &str = "/proc/driver/thermal/clctm";
const DAEMON_MODE: &str = "2 ";
const KERNEL_MODE: &str = "1 ";
const SECOND_CHARGER: [&str; 6] = [
    "/proc/mtkcooler/abcct_2nd_lcmoff",
    "/proc/mtkcooler/abcct_2nd",
    "/proc/driver/thermal/clabcct_2nd",
    "/proc/driver/thermal/clabcct_2nd_lcmoff",
    "/proc/driver/thermal/tzcharger2",
    "/proc/mtktz/mtktscharger2",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Setting<'a> {
    file: &'a str,
    text: &'a str,
}

/// The kernel's files the policy is written to.
pub trait Files {
    // False where the kernel has no such file or refuses it.
    fn write(&mut self, file: &str, text: &str) -> bool;
}

fn settings(policy: &str) -> Vec<Setting<'_>> {
    let mut lines = policy.lines();
    let mut settings = Vec::new();
    while let Some(line) = lines.next() {
        if line.starts_with('/') {
            settings.push(Setting { file: line, text: lines.next().unwrap_or_default() });
        }
    }
    settings
}

fn to_write(policy: &str) -> Vec<Setting<'_>> {
    settings(policy).into_iter().filter(|setting| !SECOND_CHARGER.contains(&setting.file)).collect()
}

fn adapt(setting: Setting<'_>) -> Cow<'_, str> {
    match setting.text.strip_prefix(DAEMON_MODE) {
        Some(rest) if setting.file == STEERING => Cow::Owned(format!("{KERNEL_MODE}{rest}")),
        _ => Cow::Borrowed(setting.text),
    }
}

/// Writes the policy to `files` in the file's order, all but the second charger's settings; returns the log line.
pub fn apply(policy: &str, files: &mut impl Files) -> String {
    let settings = to_write(policy);
    let is_written = |setting: &&Setting<'_>| files.write(setting.file, &format!("{}\n", adapt(**setting)));
    let count = settings.iter().filter(is_written).count();
    let skipped = self::settings(policy).len() - settings.len();
    format!(
        "thermal: {count} of {} settings of Rabbit's policy applied, the second charger's {skipped} left out",
        settings.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Sec_Chip_Name\nMT6765_Q\nSec_End\nSec_CD_TM\n/proc/mtkcooler/a\n\n/proc/mtkcooler/b\nEXIT 6000\nSec_End\nSec_RTP\n/proc/driver/thermal/tzcpu\n1 117000 0 x 200\nSec_End\nEOF\n";

    fn text_for(file: &str) -> String {
        let settings = settings(POLICY);
        let setting = settings.iter().find(|setting| setting.file == file).expect("the policy has this file");
        adapt(*setting).into_owned()
    }

    #[test]
    fn file_text_pairs() {
        let actual = settings(SAMPLE);
        assert_eq!(actual.len(), 3);
        assert_eq!(actual[0], Setting { file: "/proc/mtkcooler/a", text: "" });
        assert_eq!(actual[1], Setting { file: "/proc/mtkcooler/b", text: "EXIT 6000" });
        assert_eq!(actual[2], Setting { file: "/proc/driver/thermal/tzcpu", text: "1 117000 0 x 200" });
        assert_eq!(settings("/proc/x"), [Setting { file: "/proc/x", text: "" }], "a file on the last line");
    }

    #[test]
    fn second_charger_skipped() {
        let all = settings(POLICY);
        let writes = to_write(POLICY);
        assert_eq!(all.len(), 49, "the policy is Rabbit's whole file");
        assert_eq!(
            all.iter().filter(|setting| SECOND_CHARGER.contains(&setting.file)).count(),
            SECOND_CHARGER.len(),
            "each of its files once"
        );
        assert_eq!(writes.len(), all.len() - SECOND_CHARGER.len());
        assert!(writes.iter().all(|setting| !SECOND_CHARGER.contains(&setting.file)));
        assert_eq!(writes.first(), all.first(), "the order of the file");
        assert!(
            writes.iter().any(|setting| setting.file == "/proc/mtkcooler/mtktscharger2-rst"),
            "the second charger's reset cooler, which the kernel makes on every board, keeps its setting"
        );
    }

    #[test]
    fn steering_in_kernel() {
        assert_eq!(text_for(STEERING), "1 85000 55000 43000 46000 75000 51000 515000 10000 419000 8000 500 500 13500");
    }

    #[test]
    fn other_settings_verbatim() {
        for setting in settings(POLICY) {
            if setting.file != STEERING {
                assert_eq!(adapt(setting), setting.text);
            }
        }
        assert_eq!(text_for("/proc/driver/thermal/clbcct"), "0 550 350 150");
    }

    struct Refusing(Vec<(String, String)>);

    impl Files for Refusing {
        fn write(&mut self, file: &str, text: &str) -> bool {
            self.0.push((file.into(), text.into()));
            !file.ends_with('b')
        }
    }

    #[test]
    fn apply_counts_refused_writes() {
        let mut files = Refusing(Vec::new());
        assert_eq!(
            apply(SAMPLE, &mut files),
            "thermal: 2 of 3 settings of Rabbit's policy applied, the second charger's 0 left out"
        );
        assert_eq!(files.0[1], ("/proc/mtkcooler/b".into(), "EXIT 6000\n".into()));
    }
}
