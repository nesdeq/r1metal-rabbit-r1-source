//! What home tells the device holds.

/// What the device holds.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Tally {
    pub photos: usize,
    /// The photos taken today by the clock on screen.
    pub photos_today: usize,
    pub videos: usize,
    pub notes: usize,
    /// How long the newest note lasts.
    pub newest_note_s: Option<u64>,
    /// Today's edition of the news is there.
    pub has_edition: bool,
}

/// Whether home shows, the day, and how often home has come to show.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct HomeView {
    is_shown: bool,
    day: u64,
    view_count: u32,
}

impl HomeView {
    /// Takes whether home shows and the day; true if either changed.
    pub fn set(&mut self, is_shown: bool, day: u64) -> bool {
        let is_new = (self.is_shown, self.day) != (is_shown, day);
        self.view_count = self.view_count.wrapping_add(u32::from(is_shown && !self.is_shown));
        self.is_shown = is_shown;
        self.day = day;
        is_new
    }

    pub(super) fn count_key(&self) -> Option<(u32, u64)> {
        self.is_shown.then_some((self.view_count, self.day))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_recounted() {
        let mut view = HomeView::default();
        assert_eq!(view.count_key(), None);
        assert!(view.set(true, 3));
        let first = view.count_key().map(|key| key.0);
        assert_eq!(view.count_key().map(|key| key.1), Some(3));
        assert!(!view.set(true, 3));
        assert!(view.set(true, 4));
        assert_eq!(view.count_key(), first.map(|view_count| (view_count, 4)));
        assert!(view.set(false, 4) && view.count_key().is_none());
        assert!(view.set(true, 4));
        assert!(view.count_key().is_some_and(|key| Some(key.0) != first));
    }
}
