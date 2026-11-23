use super::{VoiceRecording, record_voice};
use crate::sys;
use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

const FILE_NAME: &str = "said.m4a";

/// A voice being recorded into memory.
pub struct Memo {
    recording: VoiceRecording,
    file: File,
}

/// A file in memory for a recording: the reading and the writing handle.
pub struct Files {
    file: File,
    writer: File,
}

impl Memo {
    /// Makes a file in memory, `name` in the kernel's lists.
    pub fn files(name: &CStr) -> io::Result<Files> {
        let file = sys::memory_file(name)?;
        let writer = file.try_clone()?;
        Ok(Files { file, writer })
    }

    /// Begins recording a voice into the file `files` made.
    pub fn record(Files { file, writer }: Files) -> io::Result<Memo> {
        let recording = record_voice(move || Ok((writer, String::from(FILE_NAME))))?;
        Ok(Memo { recording, file })
    }

    /// Ends the recording and drops the file.
    pub fn discard(self) -> io::Result<()> {
        self.recording.end().map(drop)
    }

    /// Ends the recording and reads the file back, with the seconds `seconds_of` makes of its packets; none if too little was heard.
    pub fn finish(mut self, seconds_of: impl FnOnce(usize) -> Option<u64>) -> io::Result<Option<(Vec<u8>, u64)>> {
        let Some(seconds) = self.recording.end()?.and_then(|summary| seconds_of(summary.packets)) else {
            return Ok(None);
        };
        let mut sound = Vec::new();
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_to_end(&mut sound)?;
        Ok(Some((sound, seconds)))
    }
}
