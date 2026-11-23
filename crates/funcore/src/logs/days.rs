use alloc::string::String;

/// The days the writer and the packer are at, so that neither touches a folder the other has.
pub struct Days<R> {
    writer_day: Option<String>,
    packer_day: Option<String>,
    pack_job: Option<(R, String)>,
    is_packer_held: bool,
}

impl<R> Default for Days<R> {
    fn default() -> Days<R> {
        Days { writer_day: None, packer_day: None, pack_job: None, is_packer_held: false }
    }
}

impl<R> Days<R> {
    fn is_packing_day(&self, day: &str) -> bool {
        self.packer_day.as_deref() == Some(day)
    }

    fn open(&mut self, day: &str) {
        self.writer_day = Some(day.into());
    }

    /// Opens that day's folder for the writer; none if the packer is at it.
    pub fn try_open(&mut self, day: &str) -> Option<()> {
        (!self.is_packing_day(day)).then(|| self.open(day))
    }

    /// Marks the writer's folder closed.
    pub fn close(&mut self) {
        self.writer_day = None;
    }

    /// Asks for every day under `root` but `today` to be packed, in place of the last such job.
    pub fn pack_past(&mut self, root: R, today: &str) {
        self.pack_job = Some((root, today.into()));
    }

    /// The root of the days to pack and the day that stays; none while none wait.
    pub const fn take_pack_job(&mut self) -> Option<(R, String)> {
        self.pack_job.take()
    }

    /// Takes a day for the packer, unless the writer has its folder open or holds the packer off: true if it is taken.
    pub fn begin_packing(&mut self, day: &str) -> bool {
        let is_free = !self.is_packer_held && self.writer_day.as_deref() != Some(day);
        if is_free {
            self.packer_day = Some(day.into());
        }
        is_free
    }

    /// As `begin_packing`, for a past day; `today` stays.
    pub fn begin_past(&mut self, day: &str, today: &str) -> bool {
        day != today && self.begin_packing(day)
    }

    /// Frees the packer's day.
    pub fn end_packing(&mut self) {
        self.packer_day = None;
    }

    const fn is_packing(&self) -> bool {
        self.packer_day.is_some()
    }

    /// `Some` once the packer is at no day, for a wait on that.
    pub const fn to_idle(&self) -> Option<()> {
        if self.is_packing() { None } else { Some(()) }
    }

    /// Holds the packer off while the writer scrubs the logs, or lets it go.
    pub const fn hold_packer(&mut self, is_held: bool) {
        self.is_packer_held = is_held;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_and_packer_days() {
        let mut days = Days::default();
        days.open("20261011");
        assert!(!days.begin_packing("20261011"), "the writer's day is not the packer's");
        assert!(days.begin_packing("20261009"));
        assert!(
            days.is_packing_day("20261009") && !days.is_packing_day("20261011"),
            "the writer waits for the day being packed alone"
        );
        assert_eq!(
            (days.try_open("20261009"), days.writer_day.as_deref()),
            (None, Some("20261011")),
            "which it does not open, and stays where it is"
        );
        days.end_packing();
        assert!(!days.is_packing_day("20261009"), "and no longer once it is packed");
        assert_eq!(
            (days.try_open("20261009"), days.writer_day.as_deref()),
            (Some(()), Some("20261009")),
            "then it opens it"
        );
        days.close();
        assert!(days.begin_packing("20261011") && days.is_packing(), "a folder the writer has closed is nobody's");
        days.end_packing();
        days.hold_packer(true);
        assert!(!days.is_packing() && !days.begin_packing("20261010"), "held off, the packer takes no day");
        days.hold_packer(false);
        assert!(!days.begin_past("20261010", "20261010") && days.to_idle().is_some(), "today stays");
        assert!(days.begin_packing("20261010") && days.to_idle().is_none());
        assert_eq!(days.take_pack_job(), None);
        days.pack_past("one", "20261010");
        days.pack_past("logs", "20261011");
        assert_eq!(
            (days.take_pack_job(), days.take_pack_job()),
            (Some(("logs", "20261011".into())), None),
            "the last asking is taken, once"
        );
    }
}
