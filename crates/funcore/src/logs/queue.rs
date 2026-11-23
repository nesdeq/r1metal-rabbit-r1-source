use super::LineFile;
use alloc::string::String;
use alloc::vec::Vec;

/// The lines of each file of lines, in one piece.
#[derive(Default)]
pub struct Lines([String; LineFile::ALL.len()]);

impl Lines {
    /// The lines of a file.
    pub fn of(&self, file: LineFile) -> &str {
        &self.0[file as usize]
    }

    /// Where a file's next line is written.
    pub fn of_mut(&mut self, file: LineFile) -> &mut String {
        &mut self.0[file as usize]
    }

    /// Drops the lines, keeping their capacity.
    pub fn clear(&mut self) {
        self.0.iter_mut().for_each(String::clear);
    }

    fn append(&mut self, source: &Lines) {
        for (lines, more) in self.0.iter_mut().zip(&source.0) {
            lines.push_str(more);
        }
    }
}

/// The lines of each file in one piece, the snapshots asked for, and the waits for them to be written.
#[derive(Default)]
pub struct Queue {
    pub lines: Lines,
    /// The snapshots asked for: the stamp of the asking and what it was for.
    pub snapshots: Vec<String>,
    last_wait: u64,
    written_wait: u64,
    is_open: bool,
    period: u64,
}

impl Queue {
    /// Begins an on-period with the day's folder open: lines go, and waits wait for the writer from now on.
    pub fn begin(&mut self, period: u64) {
        self.clear();
        (self.period, self.is_open) = (period, true);
    }

    /// Closes the folder: lines go, and nothing is waited for.
    pub fn close(&mut self) {
        self.clear();
        self.is_open = false;
    }

    /// Begins a wait for everything made up to now to be written, and gives its number.
    pub fn begin_wait(&mut self) -> u64 {
        self.last_wait += 1;
        self.last_wait
    }

    /// The number of the last wait begun: a pass that begins now writes what it waits for.
    pub const fn last_wait(&self) -> u64 {
        self.last_wait
    }

    /// Marks what the waits up to that number wait for as written.
    pub const fn set_written(&mut self, wait: u64) {
        self.written_wait = wait;
    }

    /// True if the wait of that number is over: all is written, or no folder is open to write it to.
    pub const fn is_settled(&self, wait: u64) -> bool {
        !self.is_open || self.written_wait >= wait
    }

    /// Takes in a thread's queue in the order it was made, unless it is of an on-period that is over.
    pub fn merge(&mut self, source: &mut Queue) {
        if source.period == self.period {
            self.lines.append(&source.lines);
            self.snapshots.append(&mut source.snapshots);
        }
        source.clear();
    }

    /// Drops the lines and snapshots, keeping their capacity.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.snapshots.clear();
    }

    /// Has what is made from here on belong to that on-period; lines of an earlier one go.
    pub fn set_period(&mut self, period: u64) {
        if self.period != period {
            self.clear();
            self.period = period;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_by_period() {
        let (mut queue, mut source) = (Queue::default(), Queue::default());
        source.lines.of_mut(LineFile::Frames).push_str("one\n");
        source.snapshots.push("asked".into());
        queue.merge(&mut source);
        assert_eq!((queue.lines.of(LineFile::Frames), queue.snapshots.len()), ("one\n", 1));
        assert!(
            source.lines.of(LineFile::Frames).is_empty() && source.snapshots.is_empty(),
            "what was taken in is no longer held"
        );
        source.set_period(1);
        source.lines.of_mut(LineFile::Power).push_str("new\n");
        queue.merge(&mut source);
        assert_eq!(queue.lines.of(LineFile::Power), "", "made in another on-period, it is dropped");
        let power = source.lines.of_mut(LineFile::Power);
        assert!(power.is_empty() && power.capacity() > 0, "and its room is kept");
        queue.set_period(1);
        assert!(
            queue.lines.of(LineFile::Frames).is_empty() && queue.period == 1,
            "a queue of a new on-period starts empty"
        );
        source.lines.of_mut(LineFile::Input).push_str("touch\n");
        source.set_period(1);
        queue.merge(&mut source);
        assert_eq!(
            LineFile::ALL.map(|file| queue.lines.of(file)),
            ["", "touch\n", ""],
            "within its own on-period nothing is dropped, and each file keeps its own lines"
        );
    }

    #[test]
    fn wait_settles() {
        let mut queue = Queue::default();
        let unopened = queue.begin_wait();
        assert!(queue.is_settled(unopened), "without an open folder nothing is waited for");
        queue.lines.of_mut(LineFile::Frames).push_str("old\n");
        queue.begin(1);
        assert!(queue.lines.of(LineFile::Frames).is_empty() && queue.period == 1, "an on-period begins empty");
        let first = queue.begin_wait();
        assert!(!queue.is_settled(first), "with the folder open, a wait waits for the writer");
        let passing = queue.last_wait();
        let second = queue.begin_wait();
        queue.set_written(passing);
        assert!(queue.is_settled(first), "a pass writes everything asked for before it began");
        assert!(!queue.is_settled(second), "but not what was asked for while it ran");
        queue.lines.of_mut(LineFile::Power).push_str("unwritten\n");
        queue.close();
        assert!(queue.is_settled(second), "a closed folder has nothing to wait for");
        assert!(queue.lines.of(LineFile::Power).is_empty(), "and what was not written is dropped");
    }
}
