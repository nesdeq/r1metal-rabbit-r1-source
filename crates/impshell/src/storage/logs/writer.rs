use super::crashes::{take_crashes, take_dump};
use super::folder::{Folder, last_record_number};
use super::snapshot::take_snapshot;
use super::{Shared, now, scrub, snapshot};
use crate::storage::Data;
use crate::sys::{self, klog, lock};
use crate::{clock, log};
use r1_funcore::base::handoff::Latch;
use r1_funcore::base::time::{Moment, NANOS_PER_MICRO};
use r1_funcore::fs::kedump;
use r1_funcore::logs::records;
use r1_funcore::logs::scrub::is_mark_spent;
use r1_funcore::logs::writer::{self, NO_PARTITION, Progress, Status, Switch, Turn};
use r1_funcore::logs::{BOOT, CRASH_LIST, DIRECTORY, Kind, LineFile, Lines, Logging, SCRUBBED, lines, to_changed_list};
use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

const REBOOT_RECORD: &str = "/proc/aed/reboot-reason";

#[derive(Default)]
struct Writer {
    root: Option<PathBuf>,
    folder: Option<Folder>,
    kmsg: Option<File>,
    progress: Progress,
    switch: Switch,
    records: Vec<u8>,
    ends: Vec<usize>,
    output: Vec<u8>,
    lines: Lines,
    status: Latch<Status>,
}

impl Writer {
    fn set_status(&mut self, shared: &Shared, status: Status) {
        if let Some(status) = self.status.change(status) {
            shared.board.lock().report = Some(status);
            shared.board.raise();
        }
    }

    fn wait(&self, shared: &Shared) {
        let deadline = writer::gather_timeout(self.kmsg.is_some()).map(|timeout| Instant::now() + timeout);
        let wake = sys::readable(shared.wake.fd());
        let result = match self.kmsg.as_ref().filter(|_| self.progress.is_debugging()) {
            Some(kmsg) => sys::poll(&mut [wake, sys::readable(kmsg.as_raw_fd())], deadline),
            None => sys::poll(&mut [wake], deadline),
        };
        if let Err(e) = result {
            log!("logs: waiting: {e}");
        }
        shared.wake.clear();
    }

    fn begin(&mut self, shared: &Shared, data: &Data, root: &Path, folder: &Folder) -> io::Result<()> {
        let boot = sys::boot_id().ok();
        let stored_boot = data.text(DIRECTORY, BOOT)?;
        if writer::continues_boot(boot.as_deref(), stored_boot.as_deref()) {
            self.progress.resume_after(last_record_number(root)?);
        } else {
            if let Some(boot) = &boot {
                data.put(DIRECTORY, BOOT, boot.as_bytes())?;
            }
            let mut note = String::new();
            lines::add_line(&mut note, now(), &lines::boot_note(boot.as_deref()));
            self.output.extend_from_slice(note.as_bytes());
        }
        let old_list = data.text(DIRECTORY, CRASH_LIST)?.unwrap_or_default();
        // While a scrub's mark is kept, the crash store may hold lines from before it: the copies are blanked.
        let scrubbed = data.text(DIRECTORY, SCRUBBED)?;
        self.progress.set_commit_bytes(fs::read_to_string(sys::CMDLINE).ok().as_deref());
        let crashes = take_crashes(Path::new(sys::CRASH_STORE), &folder.path, &old_list, scrubbed.is_some());
        match &crashes {
            Ok((count, list)) => {
                // A record seen and not copied, the console of a boot that went down in order, is kept in the list too.
                if let Some(list) = to_changed_list(list, &old_list) {
                    data.put(DIRECTORY, CRASH_LIST, list.as_bytes())?;
                }
                if let Some(line) = writer::to_copied_line(*count, &folder.day) {
                    log!("{line}");
                }
            }
            Err(e) => log!("logs: the crash store: {e}"),
        }
        let dump = sys::devnodes::partition_node(kedump::PARTITION)
            .and_then(|partition| take_dump(&partition, Path::new(REBOOT_RECORD), &folder.path, scrubbed.is_some()));
        match &dump {
            Ok(false) => {}
            Ok(true) => log!("{}", writer::to_dump_line(&folder.day)),
            Err(e) => log!("logs: {}: {e}", kedump::PARTITION),
        }
        if let Some(scrubbed) = &scrubbed
            && is_mark_spent(scrubbed, boot.as_deref(), crashes.is_ok() && dump.is_ok())
        {
            data.remove(DIRECTORY, SCRUBBED)?;
        }
        shared.pack_past(root, &folder.day);
        Ok(())
    }

    fn open(&mut self, shared: &Shared, data: &Data) -> io::Result<()> {
        if !data.wait_decided() {
            return Err(io::Error::other(NO_PARTITION));
        }
        let (root, day) = (data.make(DIRECTORY)?, Moment::from_seconds(now().0).to_compact_date());
        shared.open_day(&day);
        let folder = Folder::open(&root, day)?;
        let mut kmsg = klog::reader()?;
        if self.progress.has_begun() {
            kmsg.seek(SeekFrom::End(0))?;
        } else {
            self.begin(shared, data, &root, &folder)?;
        }
        self.progress.note_open();
        self.set_status(shared, Status::On(folder.day.clone()));
        (self.root, self.folder, self.kmsg) = (Some(root), Some(folder), Some(kmsg));
        shared.begin_period();
        snapshot("the logs are on");
        Ok(())
    }

    fn close(&mut self, shared: &Shared, status: Status) {
        shared.is_on.store(false, Ordering::Relaxed);
        (self.folder, self.kmsg) = (None, None);
        shared.board.lock().days.close();
        self.output.clear();
        self.lines.clear();
        shared.board.lock().queue.close();
        lock(&shared.overflow).clear();
        shared.board.notify();
        self.set_status(shared, status);
    }

    fn fail(&mut self, shared: &Shared, e: &io::Error) {
        log!("logs: {e}");
        self.switch.fail();
        self.close(shared, Status::Failed(e.to_string()));
    }

    fn roll_over(&mut self, shared: &Shared) -> io::Result<()> {
        let (Some(root), Some(folder)) = (&self.root, &mut self.folder) else {
            return Ok(());
        };
        let Some(today) = writer::to_new_day(now().0, &folder.day) else {
            return Ok(());
        };
        shared.open_day(&today);
        *folder = Folder::open(root, today)?;
        shared.pack_past(root, &folder.day);
        let status = Status::On(folder.day.clone());
        self.set_status(shared, status);
        Ok(())
    }

    fn write_mark(&mut self) {
        let start_us = sys::monotonic_ns() / NANOS_PER_MICRO;
        klog::write_debug(&records::clock_mark(start_us));
        self.progress.set_mark(start_us, sys::monotonic_ns() / NANOS_PER_MICRO);
    }

    fn pass(&mut self, shared: &Shared) -> io::Result<()> {
        self.roll_over(shared)?;
        if self.progress.is_mark_due(self.kmsg.is_some()) {
            self.write_mark();
        }
        let (Some(folder), Some(kmsg)) = (&mut self.folder, &mut self.kmsg) else {
            return Ok(());
        };
        self.progress.note_pass();
        self.records.clear();
        self.ends.clear();
        klog::records(kmsg, |record| {
            self.records.extend_from_slice(record);
            self.ends.push(self.records.len());
        })?;
        let now = (sys::realtime_ms(), sys::monotonic_ns() / NANOS_PER_MICRO);
        self.progress.add_records(&self.records, &self.ends, now, clock::offset(), &mut self.output);
        folder.append(Kind::Log, &self.output)?;
        self.progress.add_written(self.output.len());
        self.output.clear();
        let snapshots = {
            let mut state = shared.take_overflow();
            let queue = &mut state.queue;
            std::mem::swap(&mut queue.lines, &mut self.lines);
            std::mem::take(&mut queue.snapshots)
        };
        for file in LineFile::ALL {
            folder.append(file.to_kind(), self.lines.of(file).as_bytes())?;
        }
        self.lines.clear();
        for note in snapshots {
            folder.append(Kind::State, &take_snapshot(&note))?;
        }
        if self.progress.is_commit_due() {
            folder.commit()?;
            self.progress.note_commit();
        }
        Ok(())
    }

    fn scrub(&mut self, shared: &Shared, data: &Data) -> Option<String> {
        let root = match data.directory_path(DIRECTORY) {
            Ok(root) => root,
            Err(e) => return Some(e.to_string()),
        };
        // Marked before anything is taken out, so that a crash's copies made from here on are blanked.
        if let Err(e) = data.put(DIRECTORY, SCRUBBED, sys::boot_id().unwrap_or_default().as_bytes()) {
            return Some(e.to_string());
        }
        shared.board.lock().days.hold_packer(true);
        shared.board.wait(|state| state.days.to_idle());
        if let Some(folder) = &mut self.folder {
            folder.release();
        }
        let result = scrub::scrub(&root);
        shared.board.lock().days.hold_packer(false);
        if let Some(folder) = &self.folder {
            shared.pack_past(&root, &folder.day);
        }
        if let Err(e) = klog::clear() {
            log!("logs: the kernel's records a crash would carry: {e}");
        }
        result.err().map(|e| {
            log!("logs: scrubbing: {e}");
            e.to_string()
        })
    }
}

pub(super) fn work(shared: &Shared, data: &Data) -> ! {
    let mut writer = Writer::default();
    loop {
        writer.wait(shared);
        let (logging, wait) = {
            let state = shared.board.lock();
            (state.logging, state.queue.last_wait())
        };
        writer.progress.set_logging(logging);
        match writer.switch.turn(logging.map(Logging::is_on), writer.folder.is_some()) {
            Turn::Open => {
                if let Err(e) = writer.open(shared, data) {
                    writer.fail(shared, &e);
                }
            }
            Turn::Close => writer.close(shared, Status::Off),
            Turn::Report(status) => writer.set_status(shared, status),
            Turn::Stay => {}
        }
        if let Err(e) = writer.pass(shared) {
            writer.fail(shared, &e);
        }
        if let Some(number) = writer::to_scrub_turn(shared.scrub_due(), logging) {
            let failure = writer.scrub(shared, data);
            shared.end_scrub(number, failure);
        }
        shared.board.lock().queue.set_written(wait);
        shared.board.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Logs, add_line, is_on, settle};
    use super::*;
    use std::sync::Arc;

    #[test]
    fn logs_off_take_nothing() {
        let shared = Arc::new(Shared::new().unwrap());
        let handle = Logs::new(Arc::clone(&shared));
        settle(Instant::now());
        add_line(LineFile::Frames, "a frame nobody takes");
        snapshot("nothing");
        let frames = |queue: &r1_funcore::logs::Queue| queue.lines.of(LineFile::Frames).is_empty();
        assert!(
            !is_on() && frames(&shared.board.lock().queue) && shared.board.lock().queue.snapshots.is_empty(),
            "without a writer that is on, nothing is taken and nothing waited for"
        );
        let mut writer = Writer::default();
        writer.set_status(&shared, Status::Undecided);
        writer.set_status(&shared, Status::Undecided);
        assert_eq!((handle.latest(), handle.latest()), (Some(Status::Undecided), None), "a state is told once");
        shared.board.lock().queue.begin(0);
        shared.board.lock().queue.lines.of_mut(LineFile::Frames).push_str("a frame\n");
        lock(&shared.overflow).lines.of_mut(LineFile::Frames).push_str("a frame parked\n");
        writer.lines.of_mut(LineFile::Frames).push_str("a frame taken over and not written\n");
        writer.close(&shared, Status::Off);
        assert!(
            frames(&shared.board.lock().queue)
                && frames(&lock(&shared.overflow))
                && shared.board.lock().queue.is_settled(u64::MAX)
                && handle.latest() == Some(Status::Off),
            "what was queued or in the overflow for logs that went off is dropped"
        );
        assert!(
            writer.lines.of(LineFile::Frames).is_empty(),
            "what a failed pass had taken over is not written into a later folder"
        );
    }

    #[test]
    fn scrub_open_day() {
        let base = crate::storage::scratch("writer-scrub");
        let data = Data::with_root(&base).within(&[DIRECTORY]);
        let root = base.join(DIRECTORY);
        let shared = Shared::new().unwrap();
        let mut writer = Writer { folder: Some(Folder::open(&root, "20261011".into()).unwrap()), ..Writer::default() };
        let folder = writer.folder.as_mut().unwrap();
        folder.append(Kind::Log, b"10:00:00.000 14,1,5,-; (2)[412:signal]r1metal: signal: unlinked\n").unwrap();
        folder.append(Kind::Log, b"10:00:01.000 6,2,9,-;usb: up\n").unwrap();
        assert_eq!(writer.scrub(&shared, &data), None);
        assert_eq!(
            data.text(DIRECTORY, SCRUBBED).unwrap(),
            sys::boot_id().ok(),
            "the scrub's boot is marked, for the crash's copies a later first open makes"
        );
        writer.folder.as_mut().unwrap().append(Kind::Log, b"10:00:02.000 6,3,12,-;after\n").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("20261011").join(r1_funcore::logs::LOG)).unwrap(),
            "10:00:00.000 14,1,5,-;\n10:00:01.000 6,2,9,-;usb: up\n10:00:02.000 6,3,12,-;after\n",
            "what came after is written into the file in the place of the one scrubbed"
        );
        assert!(shared.board.lock().days.begin_packing("20261010"), "the packer is let go");
        assert_eq!(Writer::default().scrub(&shared, &Data::unusable()).map(|_| ()), Some(()), "no partition, no scrub");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn debug_record_wakes_thread() {
        use std::io::Write;
        use std::os::fd::FromRawFd;
        let shared = Shared::new().unwrap();
        let mut ends = [0; 2];
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        let (read_end, mut write_end) = unsafe { (File::from_raw_fd(ends[0]), File::from_raw_fd(ends[1])) };
        let mut writer = Writer { kmsg: Some(read_end), ..Writer::default() };
        writer.progress.set_logging(Some(Logging::Debug));
        write_end.write_all(b"7,1,0,-;a debug record\n").unwrap();
        let start = Instant::now();
        writer.wait(&shared);
        assert!(start.elapsed() < writer::GATHER_PERIOD, "the record woke it before the second was out");
        writer.progress.set_logging(Some(Logging::On));
        let start = Instant::now();
        writer.wait(&shared);
        assert!(start.elapsed() >= writer::GATHER_PERIOD, "on, it waits the second whatever waits");
    }
}
