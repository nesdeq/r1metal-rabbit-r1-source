//! The OpenAI key on this computer: asked for, pasted or typed without being shown, checked with OpenAI before it goes anywhere, then given to a flash, handed to a running R1, or shown as the QR code the R1's key window scans.

mod check;
mod terminal;

use crate::link::Choice;
use crate::link::console::{self, Console};
use crate::prompt::{self, Attempt};
use crate::{Result, remote};
use nusb::DeviceInfo;
use r1_funcore::domain::console::answer::KEY_STORED;
use r1_funcore::domain::console::command::Command;
use r1_funcore::domain::console::wire::ReplyLine;

// The questions asked before a flash and by `r1 key`.
const FOR_FLASH: &str = "OpenAI key for the R1, pasted or typed, not shown (Enter flashes without one): ";
const FOR_KEY: &str = "OpenAI key, pasted or typed, not shown: ";

// A key asked for until one that OpenAI takes is given, or none is: an empty line, or the input's end. Where the input is no terminal its first line alone is taken, and one that does not work is an error.
fn asked(question: &str) -> Result<Option<String>> {
    prompt::ask(question, |given| {
        let Some(given) = given.filter(|given| !given.trim().is_empty()) else {
            return Ok(Attempt::Taken(None));
        };
        if let Some(why) = check::refused(&given) {
            return Ok(Attempt::Refused(why.into()));
        }
        let key = given.trim().to_string();
        eprintln!("OpenAI takes the key, {} characters", key.chars().count());
        Ok(Attempt::Taken(Some(key)))
    })
}

// True if the R1 runs r1metal and says in time that it keeps a key.
pub(crate) fn kept_on(device: &DeviceInfo) -> bool {
    if !console::is_r1metal(device) {
        return false;
    }
    let mut kept = false;
    let asked = Console::ask_at_once(device, &Command::Key(None), |line| kept |= line == ReplyLine::Data(KEY_STORED));
    asked.is_ok() && kept
}

// The key a flash gives the R1, asked for; none where none is given.
pub(crate) fn for_flash() -> Result<Option<String>> {
    asked(FOR_FLASH)
}

/// `r1 key`: a key asked for and checked, then handed to the chosen R1, which keeps it in place of any before; with `qr` shown as a code instead, for the R1's key window to scan.
pub fn set(choice: &Choice, qr: bool) -> Result<()> {
    let key = asked(FOR_KEY)?.ok_or("no key given")?;
    if qr {
        let (code, side) = terminal::code(&key).ok_or("the key is too long for a QR code")?;
        terminal::room_for(side)?;
        print!("{code}");
        eprintln!("on the R1: OPENAI KEY, SCAN QR CODE, and its camera to the code");
        return Ok(());
    }
    remote::running_console(choice)?.run(&Command::Key(Some(key)), remote::show)?;
    eprintln!("the R1 keeps the key");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn scan_command_shows_code() {
        let parsed = crate::tests::parsed(r1_funcore::openai::key::SCAN_COMMAND);
        assert!(matches!(parsed, Some((_, crate::Task::Key { qr: true }))));
    }
}
