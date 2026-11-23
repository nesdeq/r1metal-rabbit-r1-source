//! State shared by the Pokédex's threads and the main loop.

use super::Language;
use super::entry::Entry;
use crate::domain::cause::Cause;
use crate::domain::voice::{Job, Keep, Phase, Query, Voice};
use crate::gfx::Picture;
use crate::gfx::palette::Roles;
use crate::openai::fault::Fault;
use core::convert::Infallible;

/// A lookup's result: the entry, and its artwork where that was drawn.
#[derive(Clone, Default)]
pub struct Article {
    pub entry: Entry,
    pub picture: Option<Picture>,
}

/// The Pokédex's report for the main loop.
#[derive(Clone)]
pub struct Report {
    pub phase: Phase,
    pub article: Option<Article>,
}

/// A lookup's result the main loop has not taken.
#[derive(Default)]
pub struct Lookup {
    article: Option<Article>,
    /// The colours of an artwork's ground.
    pub roles: Roles,
    /// The language names are said in and entries written in.
    pub language: Language,
}

impl Job for Lookup {
    type Input = Query;
    type Chore = Infallible;
    type Report = Report;
    const KEEP: Keep = Keep::Newest;

    fn take_query(&mut self, query: Query) -> Option<Query> {
        Some(query)
    }

    fn report(&mut self, phase: Phase) -> Report {
        Report { phase, article: self.article.take() }
    }
}

type State = Voice<Lookup>;

impl State {
    /// Takes a lookup's outcome, none meaning nothing was heard; true if the screen has news.
    pub fn take_lookup(&mut self, outcome: Result<Option<Article>, Fault>) -> bool {
        match outcome {
            Ok(Some(article)) => {
                self.job.article = Some(article);
                self.phase = Phase::Idle;
                true
            }
            Ok(None) => self.set_phase(Phase::Failed(Cause::NotHeard)),
            Err(fault) => self.fail(&fault),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::voice::Pickup;
    use alloc::vec;

    #[test]
    fn lookup_report() {
        let mut state = State::new(true);
        assert!(state.set_phase(Phase::Searching) && !state.set_phase(Phase::Searching));
        state.begin_recording(true, 0);
        assert_eq!(state.take_report().phase, Phase::Listening, "a recording that runs comes first");
        state.end_recording();
        assert!(state.take_lookup(Err(Fault::Offline("asleep".into()))));
        assert_eq!(state.take_report().phase, Phase::Failed(Cause::NoNetwork));
        assert!(state.take_lookup(Ok(None)), "no name is near what was said");
        assert_eq!(state.take_report().phase, Phase::Failed(Cause::NotHeard));
        let entry = Entry { name: "pikachu".into(), ..Entry::default() };
        assert!(state.take_lookup(Ok(Some(Article { entry, picture: None }))));
        let report = state.take_report();
        assert_eq!(
            (report.phase, report.article.map(|article| article.entry.name)),
            (Phase::Idle, Some("pikachu".into()))
        );
        assert!(state.take_report().article.is_none(), "once");
    }

    #[test]
    fn typed_name() {
        let mut state = State::new(true);
        state.take_text("mew".into());
        assert!(!state.is_keyed());
        state.take_text("pikachu".into());
        assert_eq!(state.pick_up(0), Pickup::Work(vec![Query::Text("pikachu".into())], false));
    }
}
