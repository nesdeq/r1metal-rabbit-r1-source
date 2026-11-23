//! The kinds of entry the audio, news and memory screens list.

use crate::openai::ledger::SummaryKind;

/// A kind of entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shelf {
    Notes,
    Talks,
    News,
    // The robot's facts of the user.
    Facts,
    // The days of the robot's conversation.
    Days,
}

impl Shelf {
    // The kind the scribe sums an entry of this shelf up as.
    pub(crate) const fn summary_kind(self) -> Option<SummaryKind> {
        match self {
            Shelf::Notes => Some(SummaryKind::Note),
            Shelf::Talks => Some(SummaryKind::Talk),
            Shelf::News | Shelf::Facts | Shelf::Days => None,
        }
    }

    // True for the robot's shelves, which the conversation keeps and nothing reads aloud.
    pub(crate) const fn is_robots(self) -> bool {
        matches!(self, Shelf::Facts | Shelf::Days)
    }
}
