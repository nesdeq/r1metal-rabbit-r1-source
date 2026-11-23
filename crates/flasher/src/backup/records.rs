//! The records of a backup, the payload of its age file: each file by its path from the partition's root with its bytes in parts, each file that went while the backup was taken, and the end. Of two records of one path the later counts.

use super::age;
use crate::Result;
use r1_funcore::domain::backup::is_backed_up;
use std::io::{Read, Write};

// What a backup's payload begins with: its name and the version of its records.
const HEAD: &[u8] = b"r1metal backup 1\n";
const END: u8 = 0;
const FILE: u8 = 1;
const GONE: u8 = 2;
// The most bytes of a part in this version of the records, whatever a read of the device's file brings.
const PART_BYTES: usize = 49152;
const NO_RECORDS: &str = "the file holds no backup's records";
const DAMAGED: &str = "the backup's records are damaged";

/// A backup's records as they are written into its age file.
pub struct Writer<W: Write>(age::Writer<W>);

impl<W: Write> Writer<W> {
    pub fn new(mut file: age::Writer<W>) -> Result<Writer<W>> {
        file.write(HEAD)?;
        Ok(Writer(file))
    }

    fn name(&mut self, kind: u8, path: &str) -> Result<()> {
        let length = u16::try_from(path.len()).map_err(|_| format!("{path}: a path longer than a backup names"))?;
        self.0.write(&[kind])?;
        self.0.write(&length.to_le_bytes())?;
        self.0.write(path.as_bytes())
    }

    // Begins a file's record: its parts follow, then its end.
    pub(super) fn begin(&mut self, path: &str) -> Result<()> {
        self.name(FILE, path)
    }

    // More of the bytes of the file begun.
    pub(super) fn part(&mut self, bytes: &[u8]) -> Result<()> {
        for part in bytes.chunks(PART_BYTES) {
            self.0.write(&(part.len() as u32).to_le_bytes())?;
            self.0.write(part)?;
        }
        Ok(())
    }

    // Ends the file begun.
    pub(super) fn end(&mut self) -> Result<()> {
        self.0.write(&0u32.to_le_bytes())
    }

    // Records that a file is on the device no more, whatever record of it came before.
    pub(super) fn gone(&mut self, path: &str) -> Result<()> {
        self.name(GONE, path)
    }

    // Ends the records and their file, and hands back what it was written to.
    pub(super) fn finish(mut self) -> Result<W> {
        self.0.write(&[END])?;
        self.0.finish()
    }
}

// What a backup's records tell, in their order.
#[cfg_attr(test, derive(PartialEq, Eq, Debug))]
pub(super) enum Record {
    File(String),
    Part(Vec<u8>),
    FileEnd,
    Gone(String),
    End,
}

// A backup's records as they are read out of its age file.
pub(super) struct Reader<R: Read> {
    file: age::Reader<R>,
    held: Vec<u8>,
    at: usize,
    is_in_file: bool,
    is_ended: bool,
}

impl<R: Read> Reader<R> {
    pub fn new(file: age::Reader<R>) -> Result<Reader<R>> {
        let mut reader = Reader { file, held: Vec::new(), at: 0, is_in_file: false, is_ended: false };
        if reader.take(HEAD.len()).ok().as_deref() != Some(HEAD) {
            return Err(NO_RECORDS.into());
        }
        Ok(reader)
    }

    fn take(&mut self, count: usize) -> Result<Vec<u8>> {
        while self.held.len() - self.at < count {
            let chunk = self.file.chunk()?.ok_or(DAMAGED)?;
            self.held.drain(..self.at);
            self.at = 0;
            self.held.extend(chunk);
        }
        let taken = self.held[self.at..self.at + count].to_vec();
        self.at += count;
        Ok(taken)
    }

    fn number<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| DAMAGED.into())
    }

    fn path(&mut self) -> Result<String> {
        let length = u16::from_le_bytes(self.number()?);
        let path = String::from_utf8(self.take(length.into())?).map_err(|_| DAMAGED)?;
        if !is_backed_up(&path) {
            return Err(format!("{path:?}: no file a backup holds").into());
        }
        Ok(path)
    }

    /// The next record; after the end, which nothing may follow, the end again.
    pub fn next(&mut self) -> Result<Record> {
        if self.is_ended {
            return Ok(Record::End);
        }
        if self.is_in_file {
            let length = u32::from_le_bytes(self.number()?) as usize;
            if length > PART_BYTES {
                return Err(DAMAGED.into());
            }
            self.is_in_file = length > 0;
            return if self.is_in_file { Ok(Record::Part(self.take(length)?)) } else { Ok(Record::FileEnd) };
        }
        match self.take(1)?[0] {
            END => {
                if self.at < self.held.len() || self.file.chunk()?.is_some() {
                    return Err(DAMAGED.into());
                }
                self.is_ended = true;
                Ok(Record::End)
            }
            FILE => {
                let path = self.path()?;
                self.is_in_file = true;
                Ok(Record::File(path))
            }
            GONE => Ok(Record::Gone(self.path()?)),
            _ => Err(DAMAGED.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::age::samples;
    use super::*;

    fn records(file: &[u8]) -> Result<Vec<Record>> {
        let mut reader = Reader::new(samples::reader(file)?)?;
        let mut records = Vec::new();
        loop {
            let record = reader.next()?;
            let is_end = record == Record::End;
            records.push(record);
            if is_end {
                return Ok(records);
            }
        }
    }

    fn file(path: &str) -> Record {
        Record::File(path.into())
    }

    #[test]
    fn records_round_trip() {
        let long: Vec<u8> = (0..PART_BYTES + 7).map(|at| (at % 253) as u8).collect();
        let mut writer = Writer::new(samples::writer()).unwrap();
        writer.begin("camera/pictures/a.jpg").unwrap();
        writer.part(&long).unwrap();
        writer.part(&[]).unwrap();
        writer.part(b"tail").unwrap();
        writer.end().unwrap();
        writer.begin(".r1metal/signal/account").unwrap();
        writer.end().unwrap();
        writer.gone("chat/core.md").unwrap();
        let written = writer.finish().unwrap();
        assert_eq!(
            records(&written).unwrap(),
            [
                file("camera/pictures/a.jpg"),
                Record::Part(long[..PART_BYTES].to_vec()),
                Record::Part(long[PART_BYTES..].to_vec()),
                Record::Part(b"tail".to_vec()),
                Record::FileEnd,
                file(".r1metal/signal/account"),
                Record::FileEnd,
                Record::Gone("chat/core.md".into()),
                Record::End,
            ],
            "no part longer than the records take, none empty, and an empty file with no part"
        );
        let empty = Writer::new(samples::writer()).unwrap().finish().unwrap();
        assert_eq!(records(&empty).unwrap(), [Record::End], "a backup of nothing");
    }

    #[test]
    fn foreign_records_refused() {
        let refused = |write: &dyn Fn(&mut age::Writer<Vec<u8>>)| {
            let mut file = samples::writer();
            write(&mut file);
            records(&file.finish().unwrap()).unwrap_err().to_string()
        };
        assert_eq!(refused(&|file| file.write(b"another payload\n").unwrap()), NO_RECORDS);
        assert_eq!(refused(&|_| {}), NO_RECORDS, "an age file of nothing");
        assert_eq!(refused(&|file| file.write(HEAD).unwrap()), DAMAGED, "records without their end");
        assert_eq!(
            refused(&|file| file.write(&[HEAD, &[END, END]].concat()).unwrap()),
            DAMAGED,
            "bytes behind the end"
        );
        assert_eq!(refused(&|file| file.write(&[HEAD, &[9]].concat()).unwrap()), DAMAGED, "a record of no kind");
        let named = |path: &str| [HEAD, &[FILE], &(path.len() as u16).to_le_bytes(), path.as_bytes()].concat();
        for path in ["../etc/passwd", "/data/camera/a.jpg", ".r1metal/battery/gauge", ".restore/camera/a.jpg", "boot"] {
            let told = refused(&|file| file.write(&named(path)).unwrap());
            assert!(told.ends_with("no file a backup holds"), "{path}: {told}");
        }
        let cut = named("camera/a.jpg");
        assert_eq!(refused(&|file| file.write(&cut).unwrap()), DAMAGED, "a file without its end");
        let long = [&cut[..], &(PART_BYTES as u32 + 1).to_le_bytes()].concat();
        assert_eq!(refused(&|file| file.write(&long).unwrap()), DAMAGED, "a part longer than any that is written");
    }
}
