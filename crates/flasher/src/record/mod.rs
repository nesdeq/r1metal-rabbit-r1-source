//! `r1 record`: a running device's screen recorded over USB into an H.264 MP4 at the rate its panel refreshes, until Ctrl-C.

mod ffmpeg;
mod video;

use crate::link::console::Console;
use crate::link::{Choice, frames};
use crate::{Result, interrupt, remote};
use ffmpeg::Ffmpeg;
use nusb::DeviceInfo;
use nusb::transfer::TransferError;
use r1_funcore::domain::console::answer::parse_record_answer;
use r1_funcore::domain::console::command::Command;
use r1_funcore::domain::console::wire::ReplyLine;
use r1_funcore::domain::recording::StartBody;
use std::path::Path;
use std::sync::mpsc;
use video::Video;

// What the main thread hears of: a piece of the stream or how its reading ended, and a Ctrl-C.
enum Heard {
    Read(std::result::Result<Vec<u8>, TransferError>),
    Interrupt,
}

// Tells the device to end its recording, in the time a running r1metal has to answer.
fn end(info: &DeviceInfo) -> Result<()> {
    Console::ask_at_once(info, &Command::Record(false), |_| {})
}

/// Records the chosen device's screen into `out` until Ctrl-C, and once more Ctrl-C if the device's last frames are not to be waited for.
pub fn record(choice: &Choice, out: &Path) -> Result<()> {
    let info = remote::running_device(choice)?;
    if !frames::offered(&info) {
        return Err("the firmware this R1 runs has no frames function: flash one that has".into());
    }
    let (tell, heard) = mpsc::channel();
    let reading = tell.clone();
    frames::read(&info, move |piece| reading.send(Heard::Read(piece)).is_ok())?;
    let mut begins_us = None;
    // The console is let go once the recording runs, so that whatever steers the device meanwhile can have it.
    Console::open(&info)?.run(&Command::Record(true), |line| {
        if let ReplyLine::Data(at) = line {
            begins_us = parse_record_answer(at);
        }
    })?;
    let begins_us = begins_us.ok_or("the device did not say when its recording begins")?;
    let mut video = Video::new(begins_us, |started: &StartBody| Ffmpeg::open(started, out));
    let taken = interrupt::take(move || {
        let _ = tell.send(Heard::Interrupt);
    })?;
    eprintln!("recording into {}: Ctrl-C ends it", out.display());
    let mut asked_to_end = false;
    let mut follow = || -> Result<bool> {
        loop {
            match heard.recv()? {
                Heard::Read(Ok(bytes)) => {
                    if video.take(&bytes)? {
                        return Ok(true);
                    }
                }
                Heard::Read(Err(e)) => {
                    eprintln!("r1: the device's frames stopped: {e}");
                    return Ok(false);
                }
                Heard::Interrupt if asked_to_end => return Ok(false),
                Heard::Interrupt => {
                    if let Err(e) = end(&info) {
                        eprintln!("r1: the device is not told to end its recording: {e}");
                        return Ok(false);
                    }
                    asked_to_end = true;
                    eprintln!("ending once the device has sent its last frames; Ctrl-C again ends at once");
                }
            }
        }
    };
    let followed = follow();
    // From here on a Ctrl-C ends `r1` at once; ffmpeg, which it does not end, writes what came all the same.
    let given_back = taken.give_back();
    // However it ended, the device is told to stop recording, and ffmpeg writes what came.
    if !asked_to_end {
        let _ = end(&info);
    }
    if !matches!(followed, Ok(true)) {
        video.cut()?;
    }
    let written = video.pictures.take().map(Ffmpeg::finish).transpose()?;
    followed?;
    given_back?;
    written.ok_or("no frame came from the device, and no video was written")?;
    eprintln!("{}", video.told(out));
    Ok(())
}
