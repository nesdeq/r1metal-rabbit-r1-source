//! Host end of the console protocol, for a device that is running r1metal.

use super::{CDC_DATA_CLASS, Pipe, Transport, has_ids};
use crate::Result;
use nusb::DeviceInfo;
use r1_funcore::domain::console::command::{Command, ParseError};
use r1_funcore::domain::console::wire::{ReplyLine, classify};
use r1_funcore::hw::usb;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// What a console that gave no answer in the time it had is told as.
const SILENT: &str = "r1metal does not answer its console";
// How long a running r1metal is given to answer what its main loop answers within a frame or two: one that has not by then still runs a command or is stuck.
const ANSWERS_WITHIN: Duration = Duration::from_secs(2);

pub fn is_r1metal(device: &DeviceInfo) -> bool {
    has_ids(device, usb::VENDOR_ID, usb::PRODUCT_ID) && device.product_string() == Some(usb::PRODUCT_NAME)
}

/// The device's own refusal of a command, which says nothing against the link.
#[derive(Debug)]
pub struct Refusal(String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "device says: {}", self.0)
    }
}

impl std::error::Error for Refusal {}

impl Refusal {
    /// True for a refusal in those words of the device's.
    pub fn says(&self, reason: &str) -> bool {
        self.0 == reason
    }
}

// A word no other session's ping carries: this process's number and the moment, to the nanosecond.
fn session_word() -> String {
    let moment = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{}-{}", std::process::id(), moment.as_nanos())
}

pub struct Console<T = Pipe> {
    link: T,
    pending: Vec<u8>,
    // How many bytes of `pending` hold no line's end, so that no packet is searched twice.
    searched: usize,
    // When every answer has to have come by; none to wait as long as the device takes.
    until: Option<Instant>,
}

impl Console {
    // Claims the console of a device that runs r1metal, unsettled: only what reads no answer takes it so.
    fn claim(info: &DeviceInfo) -> Result<Self> {
        let link = Pipe::open(info, |interface| interface.class() == CDC_DATA_CLASS)?;
        Ok(Console { link, pending: Vec::new(), searched: 0, until: None })
    }

    /// Claims the console and sends a command whose answer is not read.
    pub fn tell(info: &DeviceInfo, command: &Command) -> Result<()> {
        Console::claim(info)?.send(command)
    }

    /// Claims the console and settles it; every answer is waited for as long as the device takes.
    pub fn open(info: &DeviceInfo) -> Result<Self> {
        Console::opened(info, None)
    }

    /// Runs a command the device answers in the pass that takes it, on a console whose every answer has to come within `ANSWERS_WITHIN` or it counts as silent; every line before the verdict goes to `show`.
    pub fn ask_at_once(info: &DeviceInfo, command: &Command, show: impl FnMut(ReplyLine<'_>)) -> Result<()> {
        debug_assert!(
            !command.needs_thread(),
            "{command:?} is answered once another thread of the device's has done it"
        );
        Console::opened(info, Some(Instant::now() + ANSWERS_WITHIN))?.run(command, show)
    }

    // Claims the console and settles it, every answer to come by `until` or the console counts as silent; with none, as long as the device takes.
    fn opened(info: &DeviceInfo, until: Option<Instant>) -> Result<Self> {
        let mut console = Console::claim(info)?;
        console.until = until;
        console.settle()?;
        Ok(console)
    }
}

impl<T: Transport> Console<T> {
    // Reads past what an earlier session left unread of the device's answers: up to the answer to a ping with a word of this session's own, or to the refusal of the word by a firmware that takes none.
    fn settle(&mut self) -> Result<()> {
        let word = session_word();
        self.send(&Command::Ping(Some(word.clone())))?;
        loop {
            match classify(&self.next_line()?) {
                ReplyLine::Data(said) if said == word => return self.verdict(|_| {}),
                ReplyLine::Err(why) if why == ParseError::UnexpectedArgument.message() => return Ok(()),
                _ => {}
            }
        }
    }

    fn next_line(&mut self) -> Result<String> {
        loop {
            if let Some(end) = self.pending[self.searched..].iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=self.searched + end).collect();
                self.searched = 0;
                return Ok(String::from_utf8_lossy(&line).into_owned());
            }
            self.searched = self.pending.len();
            let patience = match self.until {
                Some(until) => until.checked_duration_since(Instant::now()).ok_or(SILENT)?,
                None => Duration::MAX,
            };
            let packet = self.link.receive_within(patience);
            let silent = self.until.is_some_and(|until| Instant::now() >= until);
            self.pending.extend(packet.map_err(|e| if silent { SILENT.into() } else { e })?);
        }
    }

    pub fn send(&mut self, command: &Command) -> Result<()> {
        self.link.send(command.to_line().as_bytes())
    }

    /// Sends `command`, passes every line before the verdict to `show`, and returns the verdict: a `Refusal` if the device said no.
    pub fn run(&mut self, command: &Command, show: impl FnMut(ReplyLine<'_>)) -> Result<()> {
        self.send(command)?;
        self.verdict(show)
    }

    // Passes every line before the verdict to `show`, and returns the verdict.
    fn verdict(&mut self, mut show: impl FnMut(ReplyLine<'_>)) -> Result<()> {
        loop {
            let line = self.next_line()?;
            match classify(&line) {
                ReplyLine::Ok => return Ok(()),
                ReplyLine::Err(why) => return Err(Refusal(why.to_string()).into()),
                other => show(other),
            }
        }
    }

    /// Passes lines to `show` until the device goes away.
    pub fn follow(&mut self, mut show: impl FnMut(ReplyLine<'_>)) -> Result<()> {
        loop {
            let line = self.next_line()?;
            show(classify(&line));
        }
    }
}

#[cfg(test)]
pub mod samples {
    use super::*;

    /// A console over a scripted device, taken as settled, its answers to come by `until`.
    pub fn console<T: Transport>(link: T, until: Option<Instant>) -> Console<T> {
        Console { link, pending: Vec::new(), searched: 0, until }
    }

    /// The scripted device a console is over.
    pub fn device<T: Transport>(console: &mut Console<T>) -> &mut T {
        &mut console.link
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::domain::console::answer::KEY_STORED;
    use r1_funcore::domain::console::wire::Reply;

    // Bytes a packet of the scripted device carries: few, so that lines cross packets.
    const PACKET: usize = 7;

    // How a scripted device answers: as a firmware that takes a ping's word, as one that takes none, or not at all.
    #[derive(Clone, Copy, Debug)]
    enum Firmware {
        Words,
        Wordless,
        Stuck,
    }

    // A device's console as the host meets it: what an earlier session left unread, then the answer to every line sent.
    struct Device {
        queued: Vec<u8>,
        firmware: Firmware,
    }

    impl Transport for Device {
        fn send(&mut self, data: &[u8]) -> Result<()> {
            let line = std::str::from_utf8(data)?.trim_end();
            let mut answer = Reply::default();
            match (Command::parse(line), self.firmware) {
                (_, Firmware::Stuck) => return Ok(()),
                (Ok(Command::Ping(Some(word))), Firmware::Words) => {
                    answer.data(&word);
                    answer.ok();
                }
                (Ok(Command::Ping(Some(_))), _) => {
                    answer.err(ParseError::UnexpectedArgument.message());
                }
                (Ok(Command::Key(None)), _) => {
                    answer.data(KEY_STORED);
                    answer.ok();
                }
                _ => return Err("a line the device is not scripted for".into()),
            }
            self.queued.extend(answer.bytes());
            Ok(())
        }

        fn receive_within(&mut self, _: Duration) -> Result<Vec<u8>> {
            if self.queued.is_empty() {
                return Err("the device sent nothing".into());
            }
            Ok(self.queued.drain(..self.queued.len().min(PACKET)).collect())
        }
    }

    fn console(left: &[u8], firmware: Firmware, until: Option<Instant>) -> Console<Device> {
        samples::console(Device { queued: left.to_vec(), firmware }, until)
    }

    #[test]
    fn settle_skips_stale_lines() {
        let left = b"> the rest of a reply\r\n# a log line\r\nerr no such file\r\n> pong\r\nok\r\n";
        for firmware in [Firmware::Words, Firmware::Wordless] {
            let mut console = console(left, firmware, None);
            console.settle().unwrap();
            let mut said = Vec::new();
            console.run(&Command::Key(None), |line| said.push(format!("{line:?}"))).unwrap();
            assert_eq!(said, [format!("{:?}", ReplyLine::Data(KEY_STORED))], "{firmware:?}");
            assert!(console.link.queued.is_empty() && console.pending.is_empty(), "nothing is left for the next");
            assert_eq!(console.searched, 0, "and a line taken leaves what follows it unsearched");
        }
    }

    #[test]
    fn silent_console_timeout() {
        let mut console = console(b"", Firmware::Stuck, Some(Instant::now()));
        assert_eq!(console.settle().unwrap_err().to_string(), SILENT);
    }
}
