//! What a flash chose: the file it writes on the boot image's ramdisk, and the stamp of the last flash taken.

use crate::base::lines::last_value;
use crate::base::lines::values;
use crate::domain::settings::KEY_LOGGING;
use crate::domain::settings::Settings;
use crate::logs::Logging;
use alloc::format;
use alloc::string::String;
use core::fmt::Write;

/// The file a flash writes at the root of the boot image's ramdisk.
pub const FILE: &str = "flashed";
/// The file in the settings' directory holding the stamp of the last flash taken.
pub const STAMP_FILE: &str = "flash";
const KEY_STAMP: &str = "stamp";
const KEY_OPENAI: &str = "key";

/// What a flash chose: its unique stamp, the logging, and the OpenAI key checked on the computer.
#[derive(Clone, PartialEq, Eq)]
pub struct Flash {
    pub stamp: u64,
    pub logging: Logging,
    pub key: Option<String>,
}

impl core::fmt::Debug for Flash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let key = self.key.as_ref().map(|_| "given");
        f.debug_struct("Flashed")
            .field("stamp", &self.stamp)
            .field("logging", &self.logging)
            .field("key", &key)
            .finish()
    }
}

impl Flash {
    /// Parses a flash's file by the last line of a key: none without a stamp, a key only where it looks like OpenAI's, logging on by default.
    pub fn parse(text: &str) -> Option<Flash> {
        Some(Flash {
            stamp: values(text, KEY_STAMP).last()?.trim().parse().ok()?,
            logging: last_value(text, KEY_LOGGING, Logging::parse).unwrap_or(Logging::On),
            key: values(text, KEY_OPENAI).last().and_then(crate::openai::key::parse_key).map(String::from),
        })
    }

    /// The text of a flash's file.
    pub fn to_text(&self) -> String {
        let mut text = format!("{KEY_STAMP} {}\n{KEY_LOGGING} {}\n", self.stamp, self.logging.to_word());
        if let Some(key) = &self.key {
            let _ = writeln!(text, "{KEY_OPENAI} {key}");
        }
        text
    }

    /// True where nothing is logged from the first line on.
    pub const fn is_silent(&self) -> bool {
        !self.logging.is_on()
    }

    fn to_stamp_text(&self) -> String {
        format!("{}\n", self.stamp)
    }

    /// The stamp to keep now and the one to keep once the flash's key is stored, so that a flash whose key could not be stored is taken anew at the next start.
    pub fn stamps_to_keep(&self) -> (Option<String>, Option<String>) {
        let stamp = self.to_stamp_text();
        if self.key.is_some() { (None, Some(stamp)) } else { (Some(stamp), None) }
    }

    /// True where the stored stamp is another flash's, or none.
    pub fn is_new(&self, stored: Option<&str>) -> bool {
        stored.and_then(|stored| stored.trim().parse::<u64>().ok()) != Some(self.stamp)
    }

    /// The settings with this flash's choice of logging.
    pub const fn apply_to(&self, settings: Settings) -> Settings {
        Settings { logging: self.logging, ..settings }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_file_round_trip() {
        let key = Some(String::from("sk-proj-Fake0Key_-9"));
        for flash in [
            Flash { stamp: 1_792_000_000, logging: Logging::On, key: None },
            Flash { stamp: 7, logging: Logging::Off, key: key.clone() },
            Flash { stamp: 8, logging: Logging::Debug, key: None },
        ] {
            assert_eq!(Flash::parse(&flash.to_text()), Some(flash));
        }
        assert_eq!(Flash { stamp: 7, logging: Logging::Off, key: None }.to_text(), "stamp 7\nlogging off\n");
        assert_eq!(Flash { stamp: 7, logging: Logging::Debug, key: None }.to_text(), "stamp 7\nlogging debug\n");
        assert_eq!(
            Flash { stamp: 7, logging: Logging::On, key: key.clone() }.to_text(),
            "stamp 7\nlogging on\nkey sk-proj-Fake0Key_-9\n"
        );
        assert_eq!(
            Flash::parse("stamp 9\n"),
            Some(Flash { stamp: 9, logging: Logging::On, key: None }),
            "logging on unless it says"
        );
        assert_eq!(Flash::parse("stamp 9\nkey not a key\n").and_then(|flash| flash.key), None);
        assert_eq!(Flash::parse("logging off\n"), None, "no flash without a stamp");
        assert_eq!(Flash::parse("stamp soon\nlogging off\n"), None);
        assert_eq!(
            Flash::parse("stamp 8\nlogging off\nkey sk-proj-Fake0Key_-9\nstamp 9\nlogging loud\nkey not a key\n"),
            Some(Flash { stamp: 9, logging: Logging::Off, key: None }),
            "of a key written twice the last line counts, and of the logging the last that names one"
        );
        assert_eq!(Flash::parse(""), None);
        let debug = alloc::format!("{:?}", Flash { stamp: 7, logging: Logging::On, key });
        assert!(debug.contains("given") && !debug.contains("Fake0Key"), "a flash is told without its key: {debug}");
    }

    #[test]
    fn choice_taken_once() {
        let flash = Flash { stamp: 1_792_000_000, logging: Logging::Off, key: None };
        assert!(flash.is_new(None), "nothing taken yet");
        assert!(flash.is_new(Some("1791999999\n")), "another flash's was taken");
        assert!(flash.is_new(Some("garbled")));
        assert!(!flash.is_new(Some(&flash.to_stamp_text())), "this one's was taken");
        assert!(!flash.is_new(Some(" 1792000000 ")));
        assert_eq!(
            flash.stamps_to_keep(),
            (Some(flash.to_stamp_text()), None),
            "a flash without a key keeps its stamp at once"
        );
        let with_key = Flash { key: Some("sk-proj-Fake0Key_-9".into()), ..flash.clone() };
        assert_eq!(
            with_key.stamps_to_keep(),
            (None, Some(with_key.to_stamp_text())),
            "one that gave a key owes it until the key is kept"
        );
        let set = Settings { brightness: 80, ..Settings::default() };
        assert_eq!(
            flash.apply_to(set),
            Settings { logging: Logging::Off, ..set },
            "its choice of logging over the settings, all else as it is"
        );
    }
}
