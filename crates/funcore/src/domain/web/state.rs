//! State shared by the browser's threads and the main loop.

use super::Page;
use super::visit::Outcome;
use crate::domain::voice::{Job, Keep, Phase, Query, Voice};
use crate::net::fetch::MAX_BYTES;
use alloc::vec::Vec;
use core::convert::Infallible;

/// The transcriber's prompt for a request said.
pub const PROMPT: &str = "A request to a web browser: a site's name or address, a topic, a link's number, or \"back\".";

/// The browser's report for the main loop.
#[derive(Clone)]
pub struct Report {
    pub phase: Phase,
    /// The page to show, where another shows than before.
    pub page: Option<Page>,
}

/// The pages visited, the one that shows last, and the page the main loop has not taken.
#[derive(Default)]
pub struct Browser {
    pages: Vec<Page>,
    news: Option<Page>,
}

impl Job for Browser {
    type Input = Query;
    type Chore = Infallible;
    type Report = Report;
    const KEEP: Keep = Keep::Newest;
    const IS_KEYED: bool = true;

    fn take_query(&mut self, query: Query) -> Option<Query> {
        Some(query)
    }

    fn report(&mut self, phase: Phase) -> Report {
        Report { phase, page: self.news.take() }
    }
}

impl Browser {
    /// The page that shows.
    pub fn page(&self) -> Option<&Page> {
        self.pages.last()
    }

    // The pages before the one that shows are kept while their words together are no more than one document may be.
    fn add(&mut self, page: Page) {
        self.pages.push(page);
        while self.pages.len() > 1 && self.pages.iter().map(|page| page.text.len()).sum::<usize>() > MAX_BYTES {
            self.pages.remove(0);
        }
        self.news = self.pages.last().cloned();
    }

    // The page before shows again; where there is none, the one that shows stays.
    fn go_back(&mut self) {
        if self.pages.len() > 1 {
            self.pages.pop();
            self.news = self.pages.last().cloned();
        }
    }
}

type State = Voice<Browser>;

impl State {
    /// Takes what a visit came to; true if the screen has news.
    pub fn take_visit(&mut self, outcome: Outcome) -> bool {
        match outcome {
            Outcome::Page(page) => self.job.add(page),
            Outcome::Back => self.job.go_back(),
            Outcome::Nothing => {}
            Outcome::Failed(fault) => return self.fail(&fault),
        }
        self.phase = Phase::Idle;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cause::Cause;
    use crate::openai::fault::Fault;

    fn page(title: &str, bytes: usize) -> Page {
        Page { title: title.into(), text: "x".repeat(bytes), ..Page::default() }
    }

    #[test]
    fn pages_and_back() {
        let mut state = State::new(true);
        assert!(state.job.page().is_none() && state.is_keyed(), "a request typed needs the key as one said does");
        assert!(state.take_visit(Outcome::Back) && state.take_report().page.is_none(), "nothing to go back to");
        assert!(state.take_visit(Outcome::Page(page("one", 10))));
        assert!(state.take_visit(Outcome::Page(page("two", 10))));
        let report = state.take_report();
        assert_eq!((report.phase, report.page.map(|page| page.title)), (Phase::Idle, Some("two".into())));
        assert!(state.take_report().page.is_none(), "told once");
        assert!(state.take_visit(Outcome::Back));
        assert_eq!(state.take_report().page.map(|page| page.title), Some("one".into()), "the page before");
        assert!(state.take_visit(Outcome::Back));
        assert_eq!(
            (state.take_report().page.map(|page| page.title), state.job.page().map(|page| page.title.as_str())),
            (None, Some("one")),
            "the first page stays"
        );
        assert!(state.take_visit(Outcome::Nothing) && state.take_report().page.is_none(), "nothing asked, nothing new");
        assert!(state.take_visit(Outcome::Failed(Fault::Offline("asleep".into()))));
        assert_eq!(state.take_report().phase, Phase::Failed(Cause::NoNetwork));
        assert_eq!(
            state.job.page().map(|page| page.title.as_str()),
            Some("one"),
            "a visit that failed leaves the page"
        );
    }

    #[test]
    fn history_within_one_document() {
        let mut state = State::new(true);
        for title in ["one", "two", "three"] {
            state.take_visit(Outcome::Page(page(title, MAX_BYTES / 2)));
        }
        assert_eq!(
            state.job.pages.iter().map(|page| page.title.as_str()).collect::<Vec<_>>(),
            ["two", "three"],
            "the oldest goes once the pages hold more than a document may"
        );
        state.take_visit(Outcome::Page(page("large", MAX_BYTES + 1)));
        assert_eq!(state.job.pages.len(), 1, "the page that shows stays, whatever it holds");
    }
}
