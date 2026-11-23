//! A visit, step by step: what was asked resolved, the page fetched by the device where it can read it, and the model asked for what it cannot.

use super::to_unread_line;
use super::{Page, Reply, Request, parse, read_document, read_reply, to_input, to_page_input, to_read_line};
use crate::base::text::has_words;
use crate::codec::http::Target;
use crate::net::fetch::{Fetched, MAX_HOPS};
use crate::openai::fault::Fault;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const NO_LINK: &str = "the page that shows has no link of that number";
const NO_PAGE: &str = "the model wrote no page of the address";
const SENT_ON: &str = "sent on and on";

/// What a visit came to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    Page(Page),
    Back,
    // Nothing was asked: the page that shows stays.
    Nothing,
    Failed(Fault),
}

/// What the shell does next: fetches an address, asks the model with that input, or ends the visit with what it came to.
#[derive(PartialEq, Eq, Debug)]
pub enum Step<'a> {
    Fetch(&'a Target),
    Ask(&'a str),
    Done(Outcome),
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Stage {
    // The address, and how often the asker was sent on.
    Fetch(Target, usize),
    // The model's input, and whether a page was asked for and nothing else will do.
    Ask(String, bool),
    End(Outcome),
}

fn to_failed(reason: &str) -> Stage {
    Stage::End(Outcome::Failed(Fault::Temporary(reason.into())))
}

// An address the device cannot fetch is the model's to open.
fn to_fetch(address: &str) -> Stage {
    match Target::parse(address) {
        Some(target) => Stage::Fetch(target, 0),
        None => Stage::Ask(to_page_input(address), true),
    }
}

/// A visit of the browser's.
pub struct Visit {
    stage: Stage,
    links: Vec<String>,
}

impl Visit {
    /// The visit `said` asks for from the page that shows; words that say nothing ask for none.
    pub fn new(said: &str, page: Option<&Page>) -> Visit {
        let links = page.map(|page| page.links.clone()).unwrap_or_default();
        let mut visit = Visit { stage: Stage::End(Outcome::Nothing), links };
        visit.stage = match parse(said, visit.links.len()) {
            Request::Back => Stage::End(Outcome::Back),
            Request::Link(number) => visit.to_link(number),
            Request::Address(address) => to_fetch(&address),
            Request::Ask(words) if has_words(&words) => Stage::Ask(to_input(&words, page), false),
            Request::Ask(_) => Stage::End(Outcome::Nothing),
        };
        visit
    }

    fn to_link(&self, number: usize) -> Stage {
        match number.checked_sub(1).and_then(|at| self.links.get(at)).filter(|address| !address.is_empty()) {
            Some(address) => to_fetch(address),
            None => to_failed(NO_LINK),
        }
    }

    /// What the shell does next; a visit that has ended hands over what it came to, once.
    pub fn step(&mut self) -> Step<'_> {
        match &mut self.stage {
            Stage::End(outcome) => Step::Done(core::mem::replace(outcome, Outcome::Nothing)),
            Stage::Fetch(target, _) => Step::Fetch(target),
            Stage::Ask(input, _) => Step::Ask(input),
        }
    }

    /// Takes what a fetch gave in `ms` milliseconds, or why it gave nothing: a page the device reads ends the visit, an asker sent on is fetched there, `MAX_HOPS` times at the most, and whatever else came has the model write the page. Returns the log line of a page read or not read.
    pub fn take_fetched(&mut self, fetched: Result<Fetched, String>, ms: u128) -> Option<String> {
        let Stage::Fetch(target, hops) = &self.stage else { return None };
        let unread =
            |reason: &str| (Stage::Ask(to_page_input(&target.to_address()), true), Some(to_unread_line(&reason)));
        let (stage, line) = match fetched {
            Ok(Fetched::Redirect(next)) if *hops < MAX_HOPS => (Stage::Fetch(next, hops + 1), None),
            Ok(Fetched::Redirect(_)) => unread(SENT_ON),
            Ok(Fetched::Document(bytes)) => match read_document(&String::from_utf8_lossy(&bytes), target) {
                Some(page) => {
                    let line = to_read_line(&page, bytes.len(), ms);
                    (Stage::End(Outcome::Page(page)), Some(line))
                }
                None => unread(&format!("a document of {} bytes that holds no words", bytes.len())),
            },
            Err(reason) => unread(&reason),
        };
        self.stage = stage;
        line
    }

    /// Takes the model's answer, or why none came: a page ends the visit, and so does `BACK`; a link or an address it resolved the request to is fetched; where a page was asked for, nothing else will do.
    pub fn take_reply(&mut self, reply: Result<String, Fault>) {
        let Stage::Ask(_, is_page) = &self.stage else { return };
        self.stage = match (reply.map(|text| read_reply(&text)), is_page) {
            (Err(fault), _) => Stage::End(Outcome::Failed(fault)),
            (Ok(Reply::Page(page)), _) if page.text.is_empty() => to_failed(NO_PAGE),
            (Ok(Reply::Page(page)), _) => Stage::End(Outcome::Page(page)),
            (Ok(_), true) => to_failed(NO_PAGE),
            (Ok(Reply::Back), false) => Stage::End(Outcome::Back),
            (Ok(Reply::Link(number)), false) => self.to_link(number),
            (Ok(Reply::Open(address)), false) => to_fetch(&address),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Page {
        Page {
            address: "https://news.example/".into(),
            title: "News".into(),
            text: "# News\n\nOne **\\[1]**".into(),
            links: alloc::vec!["https://news.example/one".into(), String::new()],
        }
    }

    fn visit(said: &str) -> Visit {
        Visit::new(said, Some(&page()))
    }

    fn asked(visit: &mut Visit) -> String {
        match visit.step() {
            Step::Ask(input) => input.into(),
            step => panic!("{step:?}"),
        }
    }

    fn outcome(mut visit: Visit) -> Option<Outcome> {
        match visit.step() {
            Step::Done(outcome) => Some(outcome),
            Step::Fetch(_) | Step::Ask(_) => None,
        }
    }

    fn failed(reason: &str) -> Option<Outcome> {
        Some(Outcome::Failed(Fault::Temporary(reason.into())))
    }

    fn document(words: &str) -> Result<Fetched, String> {
        Ok(Fetched::Document(format!("<main><p>{words}</p></main>").into_bytes()))
    }

    #[test]
    fn link_fetched_and_read() {
        let mut visit = visit("the first link");
        assert_eq!(visit.step(), Step::Fetch(&Target::parse("https://news.example/one").unwrap()));
        let moved = Target::parse("https://www.news.example/one/").unwrap();
        assert_eq!(visit.take_fetched(Ok(Fetched::Redirect(moved.clone())), 5), None);
        assert_eq!(visit.step(), Step::Fetch(&moved), "sent on, it is fetched there");
        assert_eq!(
            visit.take_fetched(document("The story."), 9).as_deref(),
            Some("web: a page of 30 bytes read as 30 characters with 0 links in 9 ms")
        );
        let Some(Outcome::Page(page)) = outcome(visit) else { panic!("a page") };
        assert_eq!(
            (page.address.as_str(), page.text.as_str()),
            ("https://www.news.example/one/", "# www.news.example\n\nThe story."),
            "read by the device, under the address it came from"
        );
    }

    #[test]
    fn unread_page_is_the_models() {
        for (fetched, reason) in [
            (Err(String::from("status 403")), "status 403"),
            (document(""), "a document of 20 bytes that holds no words"),
        ] {
            let mut visit = visit("news.example/app");
            assert_eq!(visit.take_fetched(fetched, 5), Some(to_unread_line(&reason)));
            assert!(asked(&mut visit).contains("https://news.example/app"), "the model is asked to write the page");
            visit.take_reply(Ok("OPEN https://news.example/app".into()));
            assert_eq!(outcome(visit), failed(NO_PAGE), "and nothing but a page will do then");
        }
        let mut sent_on = visit("1");
        for _ in 0..MAX_HOPS {
            let next = Target::parse("https://news.example/again").unwrap();
            assert_eq!(sent_on.take_fetched(Ok(Fetched::Redirect(next)), 5), None);
        }
        let next = Target::parse("https://news.example/again").unwrap();
        assert_eq!(sent_on.take_fetched(Ok(Fetched::Redirect(next)), 5), Some(to_unread_line(&SENT_ON)));
        sent_on.take_reply(Ok("# Written\n\nBy the model.".into()));
        assert!(matches!(outcome(sent_on), Some(Outcome::Page(page)) if page.title == "Written"));
    }

    #[test]
    fn asked_requests() {
        let mut named = visit("the guardian");
        assert_eq!(
            asked(&mut named),
            "Request: the guardian\nThe address of the page that shows: https://news.example/\nThe page that shows:\n# News\n\nOne **\\[1]**\n"
        );
        named.take_reply(Ok("OPEN https://www.theguardian.com/".into()));
        assert_eq!(named.step(), Step::Fetch(&Target::parse("https://www.theguardian.com/").unwrap()));
        let mut by_words = visit("the one about one");
        by_words.take_reply(Ok("LINK 1".into()));
        assert_eq!(by_words.step(), Step::Fetch(&Target::parse("https://news.example/one").unwrap()));
        for missing in ["LINK 2", "LINK 9"] {
            let mut none = visit("the other one");
            none.take_reply(Ok(missing.into()));
            assert_eq!(outcome(none), failed(NO_LINK));
        }
        let mut back = visit("zurück bitte");
        back.take_reply(Ok("BACK".into()));
        assert_eq!(outcome(back), Some(Outcome::Back), "back in whatever language, as the model tells it");
        assert_eq!(outcome(visit("back")), Some(Outcome::Back), "and in English without the model");
        let mut refused = visit("anything");
        refused.take_reply(Err(Fault::Limit));
        assert_eq!(outcome(refused), Some(Outcome::Failed(Fault::Limit)));
        let mut empty = visit("anything");
        empty.take_reply(Ok("LINKS\n1 https://a.example/".into()));
        assert_eq!(outcome(empty), failed(NO_PAGE));
        let mut fresh = Visit::new("3", None);
        assert_eq!(asked(&mut fresh), "Request: 3\nNo page shows.\n", "without a page a number names no link");
        assert!(outcome(fresh).is_none(), "a visit with a step to take has come to nothing yet");
        for unsaid in ["", " \n", "..."] {
            assert_eq!(outcome(visit(unsaid)), Some(Outcome::Nothing), "{unsaid:?} asks for nothing");
        }
    }
}
