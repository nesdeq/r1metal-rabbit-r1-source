//! The fastboot protocol as LK speaks it: text commands, four letter status replies.

use super::{MEDIATEK, Pipe, Transport, has_ids};
use crate::Result;
use nusb::DeviceInfo;

pub(super) const PRODUCT_ID: u16 = 0x201c;
const INTERFACE_CLASS: u8 = 0xff;
const INTERFACE_SUBCLASS: u8 = 0x42;
const INTERFACE_PROTOCOL: u8 = 0x03;
const STATUS_LEN: usize = 4;
// What the bootloader's answer to `flashing get_unlock_ability` begins with, before its number.
const UNLOCK_ABILITY: &str = "unlock_ability = ";

/// Fastboot over the USB link to an R1.
pub type Bootloader = Fastboot<Pipe>;

pub(super) fn is_fastboot(device: &DeviceInfo) -> bool {
    has_ids(device, MEDIATEK, PRODUCT_ID)
}

pub fn open(info: &DeviceInfo) -> Result<Bootloader> {
    let pipe = Pipe::open(info, |interface| {
        (interface.class(), interface.subclass(), interface.protocol())
            == (INTERFACE_CLASS, INTERFACE_SUBCLASS, INTERFACE_PROTOCOL)
    })?;
    Ok(Fastboot { link: pipe, notes: Vec::new(), limit: None })
}

enum Reply {
    Okay(String),
    Data(usize),
}

pub struct Fastboot<T> {
    link: T,
    notes: Vec<String>,
    // The most bytes the bootloader takes in one download, once it has said.
    limit: Option<usize>,
}

impl<T: Transport> Fastboot<T> {
    /// INFO and TEXT lines the bootloader sent since the last call.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    fn reply(&mut self, command: &str) -> Result<Reply> {
        loop {
            let packet = self.link.receive()?;
            let (status, rest) = packet.split_at_checked(STATUS_LEN).ok_or("fastboot reply shorter than a status")?;
            let text = String::from_utf8_lossy(rest).into_owned();
            match status {
                b"OKAY" => return Ok(Reply::Okay(text)),
                b"DATA" => {
                    let size = usize::from_str_radix(&text, 16).map_err(|_| format!("bad DATA size {text:?}"))?;
                    return Ok(Reply::Data(size));
                }
                b"INFO" | b"TEXT" => self.notes.push(text),
                b"FAIL" => return Err(format!("bootloader refused {command:?}: {text}").into()),
                _ => return Err(format!("unknown fastboot status in reply to {command:?}").into()),
            }
        }
    }

    fn command(&mut self, command: &str) -> Result<String> {
        self.link.send(command.as_bytes())?;
        match self.reply(command)? {
            Reply::Okay(text) => Ok(text),
            Reply::Data(_) => Err(format!("bootloader wants data after {command:?}").into()),
        }
    }

    pub fn getvar(&mut self, name: &str) -> Result<String> {
        self.command(&format!("getvar:{name}"))
    }

    /// The most bytes the bootloader takes in one download, asked once.
    pub fn limit(&mut self) -> Result<usize> {
        if let Some(limit) = self.limit {
            return Ok(limit);
        }
        let said = self.getvar("max-download-size")?;
        let limit = usize::from_str_radix(said.trim_start_matches("0x"), 16)
            .map_err(|_| format!("bad max-download-size {said:?}"))?;
        Ok(*self.limit.insert(limit))
    }

    fn download(&mut self, image: &[u8]) -> Result<()> {
        let limit = self.limit()?;
        if image.len() > limit {
            return Err(format!("image is {} bytes, bootloader accepts {limit}", image.len()).into());
        }
        let command = format!("download:{:08x}", image.len());
        self.link.send(command.as_bytes())?;
        match self.reply(&command)? {
            Reply::Data(size) if size == image.len() => {}
            Reply::Data(size) => return Err(format!("bootloader expects {size} bytes, not {}", image.len()).into()),
            Reply::Okay(_) => return Err("bootloader skipped the data phase".into()),
        }
        self.link.send(image)?;
        match self.reply(&command)? {
            Reply::Okay(_) => Ok(()),
            Reply::Data(_) => Err("bootloader asked for data twice".into()),
        }
    }

    pub fn flash(&mut self, partition: &str, image: &[u8]) -> Result<()> {
        self.download(image)?;
        self.command(&format!("flash:{partition}")).map(drop)
    }

    /// Whether the bootloader would carry out an unlock; none if its answer names no number.
    pub fn unlock_ability(&mut self) -> Result<Option<bool>> {
        self.command("flashing get_unlock_ability")?;
        let number = |note: &String| note.trim().strip_prefix(UNLOCK_ABILITY)?.parse::<u32>().ok();
        Ok(self.take_notes().iter().find_map(number).map(|ability| ability != 0))
    }

    pub fn unlock(&mut self) -> Result<()> {
        self.command("flashing unlock").map(drop)
    }

    pub fn erase(&mut self, partition: &str) -> Result<()> {
        self.command(&format!("erase:{partition}")).map(drop)
    }

    /// Makes `slot` the one the bootloader tries first, with a fresh boot attempt counter.
    pub fn set_active(&mut self, slot: &str) -> Result<()> {
        self.command(&format!("set_active:{slot}")).map(drop)
    }

    /// Runs `fastboot oem <args>` and returns the INFO lines it printed.
    pub fn oem(&mut self, args: &str) -> Result<Vec<String>> {
        self.command(&format!("oem {args}"))?;
        Ok(self.take_notes())
    }

    /// Every variable the bootloader publishes, one `name: value` line each.
    pub fn variables(&mut self) -> Result<Vec<String>> {
        self.getvar("all")?;
        Ok(self.take_notes())
    }

    pub fn reboot(&mut self) -> Result<()> {
        self.command("reboot").map(drop)
    }
}

#[cfg(test)]
pub mod samples {
    use super::*;
    use std::collections::VecDeque;

    /// A bootloader that answers by a script and keeps what it was sent.
    #[derive(Default)]
    pub struct Script {
        replies: VecDeque<&'static [u8]>,
        sent: Vec<Vec<u8>>,
    }

    impl Transport for Script {
        fn send(&mut self, data: &[u8]) -> Result<()> {
            self.sent.push(data.to_vec());
            Ok(())
        }

        fn receive_within(&mut self, _: std::time::Duration) -> Result<Vec<u8>> {
            Ok(self.replies.pop_front().ok_or("script ran out of replies")?.to_vec())
        }
    }

    /// Fastboot over a bootloader that gives `replies` in their order.
    pub fn fastboot(replies: &[&'static [u8]]) -> Fastboot<Script> {
        let link = Script { replies: replies.iter().copied().collect(), sent: Vec::new() };
        Fastboot { link, notes: Vec::new(), limit: None }
    }

    /// What was sent, each piece as text.
    pub fn sent(fb: &Fastboot<Script>) -> Vec<String> {
        fb.link.sent.iter().map(|piece| String::from_utf8_lossy(piece).into_owned()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::samples::{fastboot, sent};

    #[test]
    fn commands_and_notes() {
        let mut fb = fastboot(&[b"OKAYyes", b"OKAY", b"OKAY", b"OKAY", b"OKAY"]);
        assert_eq!(fb.getvar("unlocked").unwrap(), "yes");
        fb.erase("userdata").unwrap();
        fb.set_active("a").unwrap();
        fb.unlock().unwrap();
        fb.reboot().unwrap();
        assert_eq!(sent(&fb), ["getvar:unlocked", "erase:userdata", "set_active:a", "flashing unlock", "reboot"]);
        let mut fb =
            fastboot(&[b"INFOline one", b"INFOline two", b"OKAY", b"INFOslot: a", b"OKAY", b"INFOok", b"OKAY"]);
        assert_eq!(fb.oem("dump_pllk_log").unwrap(), ["line one", "line two"]);
        assert_eq!(fb.variables().unwrap(), ["slot: a"]);
        fb.reboot().unwrap();
        assert_eq!(fb.take_notes(), ["ok"], "INFO lines are kept as notes until they are taken");
        assert!(fb.take_notes().is_empty());
        assert_eq!(sent(&fb), ["oem dump_pllk_log", "getvar:all", "reboot"]);
    }

    #[test]
    fn refusal_and_bad_status() {
        let error = fastboot(&[b"FAILnot allowed in locked state"]).unlock().unwrap_err().to_string();
        assert!(error.contains("not allowed in locked state"), "{error}");
        assert!(fastboot(&[b"OK"]).reboot().is_err());
        assert!(fastboot(&[b"WHAT"]).reboot().is_err());
    }

    #[test]
    fn download_then_flash() {
        let mut fb =
            fastboot(&[b"OKAY0x8000000", b"DATA00000005", b"OKAY", b"OKAY", b"DATA00000002", b"OKAY", b"OKAY"]);
        fb.flash("boot_a", b"hello").unwrap();
        fb.flash("dtbo_a", b"hi").unwrap();
        let first = ["getvar:max-download-size", "download:00000005", "hello", "flash:boot_a"];
        let second = ["download:00000002", "hi", "flash:dtbo_a"];
        assert_eq!(sent(&fb), [&first[..], &second].concat(), "the second download goes without asking again");
    }

    #[test]
    fn oversized_image_not_sent() {
        let mut oversized = fastboot(&[b"OKAY0x4"]);
        assert!(oversized.flash("boot_a", b"hello").is_err());
        assert_eq!(sent(&oversized), ["getvar:max-download-size"]);
        let mut disagreeing = fastboot(&[b"OKAY0x8000000", b"DATA00000004"]);
        assert!(disagreeing.flash("boot_a", b"hello").is_err());
        assert_eq!(sent(&disagreeing), ["getvar:max-download-size", "download:00000005"]);
    }

    #[test]
    fn unlock_ability_number() {
        let mut refusing = fastboot(&[b"INFOunlock_ability = 0", b"OKAY"]);
        assert_eq!(refusing.unlock_ability().unwrap(), Some(false));
        assert_eq!(sent(&refusing), ["flashing get_unlock_ability"]);
        assert_eq!(fastboot(&[b"INFOunlock_ability = 1", b"OKAY"]).unlock_ability().unwrap(), Some(true));
        assert_eq!(fastboot(&[b"OKAY"]).unlock_ability().unwrap(), None, "an answer without the number");
        assert!(fastboot(&[b"FAILunknown command"]).unlock_ability().is_err());
    }
}
