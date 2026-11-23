//! A lookup, step by step: the names of the language where they are not held, the Pokémon whose name is nearest to what was said, its entry and its artwork.

use super::entry::Entry;
use super::names::{Names, needs_names};
use super::state::Article;
use super::{
    Language, NO_NAMES, entry_body, names_body, to_found_line, to_names_line, to_no_entry_line, to_no_entry_text,
};
use crate::gfx::Picture;
use crate::openai::fault::Fault;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// What the shell does next: asks PokéAPI with a query's body for every Pokémon's names or for an entry, fetches and draws the artwork at an address, or takes what the lookup came to (`Search::finish`).
pub enum Step<'a> {
    Names(String),
    Entry(String),
    Artwork(&'a str),
    Done,
}

enum Stage {
    Names,
    Entry(u32),
    // The number asked for, the entry that came and the address of its artwork.
    Artwork(u32, Entry, String),
    End(Result<Option<Article>, Fault>),
}

// The entry of the Pokémon whose name is nearest to what was said is asked for; where none is, the lookup ends.
fn to_asked(names: &Names, said: &str) -> Stage {
    names.nearest(said).map_or(Stage::End(Ok(None)), Stage::Entry)
}

fn to_failed(reason: String) -> Stage {
    Stage::End(Err(Fault::Temporary(reason)))
}

/// A lookup of the Pokédex's.
pub struct Search {
    said: String,
    language: Language,
    stage: Stage,
}

impl Search {
    /// The lookup of what was `said` in `language`, by the names held where they are of that language.
    pub fn new(said: &str, language: Language, names: Option<&Names>) -> Search {
        let stage = match names.filter(|_| !needs_names(names, language)) {
            Some(names) => to_asked(names, said),
            None => Stage::Names,
        };
        Search { said: said.into(), language, stage }
    }

    /// What the shell does next.
    pub fn step(&self) -> Step<'_> {
        match &self.stage {
            Stage::Names => Step::Names(names_body(self.language)),
            Stage::Entry(number) => Step::Entry(entry_body(*number, self.language)),
            Stage::Artwork(_, _, address) => Step::Artwork(address),
            Stage::End(_) => Step::Done,
        }
    }

    /// What a lookup that has ended came to: the article, none where no name is near what was said, or why it failed.
    pub fn finish(self) -> Result<Option<Article>, Fault> {
        match self.stage {
            Stage::End(outcome) => outcome,
            Stage::Names | Stage::Entry(_) | Stage::Artwork(..) => Ok(None),
        }
    }

    /// Takes the answer to the names' query, or why none came: the names it gives are kept in `names`, and an answer that gives none ends the lookup. Returns the log's lines.
    pub fn take_names(&mut self, answer: Result<String, String>, names: &mut Option<Names>) -> Vec<String> {
        if !matches!(self.stage, Stage::Names) {
            return Vec::new();
        }
        let found = answer.and_then(|answer| Names::parse(&answer, self.language).ok_or_else(|| NO_NAMES.into()));
        let (stage, line) = match found {
            Ok(found) => {
                let step = (to_asked(&found, &self.said), to_names_line(found.count(), self.language));
                *names = Some(found);
                step
            }
            Err(reason) => {
                let line = format!("pokedex: PokéAPI's names: {reason}");
                (to_failed(reason), line)
            }
        };
        self.stage = stage;
        Vec::from([line])
    }

    /// Takes the answer to the entry's query, or why none came: an entry that names an artwork has it fetched, one that names none ends the lookup, and so does an answer with no entry. Returns the log's lines.
    pub fn take_entry(&mut self, answer: Result<String, String>) -> Vec<String> {
        let Stage::Entry(number) = self.stage else {
            return Vec::new();
        };
        let (stage, line) = match answer.map(|answer| Entry::parse(&answer, self.language)) {
            Err(reason) => {
                let line = format!("pokedex: the entry of Pokémon {number}: {reason}");
                (to_failed(reason), Some(line))
            }
            Ok(None) => (to_failed(to_no_entry_text(number)), Some(to_no_entry_line(number))),
            Ok(Some(entry)) => match entry.artwork.clone() {
                Some(address) => (Stage::Artwork(number, entry, address), None),
                None => (Stage::End(Ok(Some(Article { entry, picture: None }))), Some(to_found_line(number, false))),
            },
        };
        self.stage = stage;
        line.into_iter().collect()
    }

    /// Takes the artwork as it was drawn, or why it was not, which is no failure of the lookup: its entry is shown without. Returns the log's lines.
    pub fn take_artwork(&mut self, picture: Result<Picture, String>) -> Vec<String> {
        let (number, entry) = match core::mem::replace(&mut self.stage, Stage::End(Ok(None))) {
            Stage::Artwork(number, entry, _) => (number, entry),
            stage => {
                self.stage = stage;
                return Vec::new();
            }
        };
        let undrawn =
            picture.as_ref().err().map(|reason| format!("pokedex: the artwork of Pokémon {number}: {reason}"));
        let picture = picture.ok();
        let found = to_found_line(number, picture.is_some());
        self.stage = Stage::End(Ok(Some(Article { entry, picture })));
        undrawn.into_iter().chain([found]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pokedex::entry::tests::answer;
    use alloc::vec;

    const NAMES: &str = r#"{"data":{"pokemonspeciesname":[{"name":"Glurak","pokemon_species_id":6}, {"name":"Charizard","pokemon_species_id":6}, {"name":"Mew","pokemon_species_id":151}]}}"#;
    const NO_ARTWORK: &str =
        r#"{"data":{"pokemonspecies":[{"id":151,"pokemonspeciesnames":[{"name":"Mew","language":{"name":"de"}}]}]}}"#;

    fn german_names() -> Option<Names> {
        Names::parse(NAMES, Language::German)
    }

    // An article's number and its picture's words.
    type Shown = (u32, Option<Vec<u32>>);

    // What a lookup that has ended came to.
    fn outcome(search: Search) -> Result<Option<Shown>, Fault> {
        assert!(matches!(search.step(), Step::Done), "the lookup has a step to take");
        search.finish().map(|article| article.map(|article| (article.entry.number, article.picture.map(|p| p.words))))
    }

    // A lookup of Glurak that holds the names and has its entry asked for.
    fn at_entry() -> Search {
        let search = Search::new("Glurak", Language::German, german_names().as_ref());
        assert!(matches!(search.stage, Stage::Entry(6)));
        search
    }

    #[test]
    fn names_entry_and_artwork() {
        let mut held = None;
        let mut search = Search::new("Glurack", Language::German, held.as_ref());
        assert!(matches!(search.step(), Step::Names(body) if body == names_body(Language::German)));
        assert!(search.take_entry(Ok(answer())).is_empty() && search.take_artwork(Err("early".into())).is_empty());
        assert!(matches!(search.step(), Step::Names(_)), "what answers another step is passed over");
        assert_eq!(search.take_names(Ok(NAMES.into()), &mut held), [to_names_line(3, Language::German)]);
        assert!(held.as_ref().is_some_and(|names| names.count() == 3), "the names that came are kept");
        assert!(
            matches!(search.step(), Step::Entry(body) if body == entry_body(6, Language::German)),
            "the Pokémon whose name is nearest to the one heard a letter wrong"
        );
        assert!(search.take_entry(Ok(answer())).is_empty());
        assert!(matches!(search.step(), Step::Artwork(address) if address.ends_with("/official-artwork/6.png")));
        let picture = Picture { words: vec![7], width: 1 };
        assert_eq!(search.take_artwork(Ok(picture)), ["pokedex: Pokémon 6 found, its artwork drawn"]);
        assert_eq!(outcome(search), Ok(Some((6, Some(vec![7])))));
        let unended = Search::new("Glurak", Language::German, None);
        assert!(
            unended.finish().is_ok_and(|article| article.is_none()),
            "a lookup left before its end came to nothing"
        );
    }

    #[test]
    fn held_names_of_the_language() {
        let held = german_names();
        let known = Search::new("mew", Language::German, held.as_ref());
        assert!(
            matches!(known.step(), Step::Entry(body) if body == entry_body(151, Language::German)),
            "names held in the language asked are not asked for again"
        );
        let other = Search::new("mew", Language::French, held.as_ref());
        assert!(matches!(other.step(), Step::Names(body) if body == names_body(Language::French)));
        let unsaid = Search::new("?", Language::German, held.as_ref());
        assert_eq!(outcome(unsaid), Ok(None), "no name is near what says nothing");
        let (mut late, mut none) = (Search::new("...", Language::German, None), None);
        late.take_names(Ok(NAMES.into()), &mut none);
        assert_eq!((outcome(late), none.is_some()), (Ok(None), true), "nor once the names have come");
    }

    #[test]
    fn answers_that_fail() {
        let failed = |reason: &str| Err(Fault::Temporary(reason.into()));
        for (answer, reason) in [(Err("timed out".into()), "timed out"), (Ok(r#"{"errors":[]}"#.into()), NO_NAMES)] {
            let (mut search, mut held) = (Search::new("mew", Language::German, None), None);
            assert_eq!(search.take_names(answer, &mut held), [format!("pokedex: PokéAPI's names: {reason}")]);
            assert_eq!((outcome(search), held.is_none()), (failed(reason), true), "no names are kept");
        }
        let mut unanswered = at_entry();
        assert_eq!(unanswered.take_entry(Err("reset".into())), ["pokedex: the entry of Pokémon 6: reset"]);
        assert_eq!(outcome(unanswered), failed("reset"));
        let mut empty = at_entry();
        assert_eq!(empty.take_entry(Ok(r#"{"data":{"pokemonspecies":[]}}"#.into())), [to_no_entry_line(6)]);
        assert_eq!(outcome(empty), failed(&to_no_entry_text(6)));
    }

    #[test]
    fn entry_without_its_artwork() {
        let mut undrawn = at_entry();
        undrawn.take_entry(Ok(answer()));
        assert_eq!(
            undrawn.take_artwork(Err("the artwork is NotPng".into())),
            [
                "pokedex: the artwork of Pokémon 6: the artwork is NotPng",
                "pokedex: Pokémon 6 found, its artwork not drawn"
            ]
        );
        assert_eq!(outcome(undrawn), Ok(Some((6, None))), "a failed artwork fails no lookup");
        let mut bare = Search::new("Mew", Language::German, german_names().as_ref());
        assert_eq!(bare.take_entry(Ok(NO_ARTWORK.into())), ["pokedex: Pokémon 151 found, its artwork not drawn"]);
        assert_eq!(outcome(bare), Ok(Some((151, None))), "an entry that names no artwork asks for none");
    }
}
