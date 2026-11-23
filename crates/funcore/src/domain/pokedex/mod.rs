//! The Pokédex's source, PokéAPI: its languages, its queries and the checks of its answers.

pub mod entry;
pub mod names;
pub mod search;
pub mod state;

use crate::base::json::escape;
use crate::codec::http::{CLOSE, IDENTITY, USER_AGENT};
use crate::gfx::png::{self, Png};
use alloc::format;
use alloc::string::String;
use core::fmt;

/// PokéAPI's GraphQL service, which takes every query by POST.
pub const ADDRESS: &str = "https://graphql.pokeapi.co/v1beta2";
const JSON: &str = "application/json";
/// The headers of a query.
pub const QUERY_HEADERS: [(&str, &str); 5] = [
    ("User-Agent", USER_AGENT),
    ("Accept", JSON),
    ("Accept-Encoding", IDENTITY),
    ("Connection", CLOSE),
    ("Content-Type", JSON),
];
/// The headers of an artwork's request.
pub const HEADERS: [(&str, &str); 4] =
    [("User-Agent", USER_AGENT), ("Accept", "*/*"), ("Accept-Encoding", IDENTITY), ("Connection", CLOSE)];
const ARTWORK_MAX_PIXELS: usize = 1024;
const NAMES_QUERY: &str = "query names($languages: [String!]!) { pokemonspeciesname(where: {language: {name: {_in: $languages}}}, order_by: {pokemon_species_id: asc}) { name pokemon_species_id } }";
// Every list of names comes in the language asked and in English, which fills in where the language has none.
const ENTRY_QUERY: &str = "query entry($id: Int!, $languages: [String!]!) { pokemonspecies(where: {id: {_eq: $id}}) { id pokemonspeciesnames(where: {language: {name: {_in: $languages}}}) { name genus language { name } } pokemonspeciesflavortexts(where: {language: {name: {_in: $languages}}}, order_by: {version_id: desc}) { flavor_text language { name } } evolutionchain { pokemonspecies(order_by: {order: asc}) { pokemonspeciesnames(where: {language: {name: {_in: $languages}}}) { name language { name } } } } pokemons(where: {is_default: {_eq: true}}) { height weight pokemontypes(order_by: {slot: asc}) { type { typenames(where: {language: {name: {_in: $languages}}}) { name language { name } } } } pokemonstats(order_by: {stat_id: asc}) { base_stat stat { statnames(where: {language: {name: {_in: $languages}}}) { name language { name } } } } pokemonabilities(order_by: {slot: asc}) { is_hidden ability { abilitynames(where: {language: {name: {_in: $languages}}}) { name language { name } } } } pokemonsprites { sprites(path: \"other.official-artwork.front_default\") } pokemonmoves(distinct_on: move_id) { move { movenames(where: {language: {name: {_in: $languages}}}) { name language { name } } } } } } }";
const NO_NAMES: &str = "PokéAPI's answer names no Pokémon";

/// A language the Pokédex is written in: of those the games are written in, the European Union's.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Language {
    #[default]
    English,
    German,
    French,
    Spanish,
    Italian,
}

impl Language {
    pub(super) const ALL: [Language; 5] =
        [Language::English, Language::German, Language::French, Language::Spanish, Language::Italian];

    // Its ISO 639-1 code, by which PokéAPI and the settings' file name it.
    pub(super) const fn code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::German => "de",
            Language::French => "fr",
            Language::Spanish => "es",
            Language::Italian => "it",
        }
    }

    pub(super) fn parse(word: &str) -> Option<Language> {
        Language::ALL.into_iter().find(|language| language.code() == word)
    }

    const fn english_name(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::German => "German",
            Language::French => "French",
            Language::Spanish => "Spanish",
            Language::Italian => "Italian",
        }
    }

    // The languages a query asks for, this one first.
    fn query_codes(self) -> String {
        match self {
            Language::English => format!("[\"{}\"]", self.code()),
            _ => format!("[\"{}\",\"{}\"]", self.code(), Language::English.code()),
        }
    }
}

/// The transcriber's prompt for a name said in `language`.
pub fn prompt(language: Language) -> String {
    format!("The name of a Pokémon, as the Pokédex writes it in {}.", language.english_name())
}

// The body of the query of every Pokémon's names in `language`, and in English.
fn names_body(language: Language) -> String {
    format!("{{\"query\":\"{}\",\"variables\":{{\"languages\":{}}}}}", escape(NAMES_QUERY), language.query_codes())
}

// The body of the query of the entry of the Pokémon of number `number` in `language`, English filling in what it lacks.
fn entry_body(number: u32, language: Language) -> String {
    format!(
        "{{\"query\":\"{}\",\"variables\":{{\"id\":{number},\"languages\":{}}}}}",
        escape(ENTRY_QUERY),
        language.query_codes()
    )
}

/// The error text of an address that is no URL.
pub fn to_no_url_text(address: &str) -> String {
    format!("{address} is no URL")
}

fn to_names_line(count: usize, language: Language) -> String {
    format!("pokedex: PokéAPI gave {count} names of Pokémon in {}", language.query_codes())
}

fn to_no_entry_text(number: u32) -> String {
    format!("PokéAPI's answer holds no entry of Pokémon {number}")
}

fn to_no_entry_line(number: u32) -> String {
    format!("pokedex: {}", to_no_entry_text(number))
}

fn to_found_line(number: u32, is_drawn: bool) -> String {
    format!("pokedex: Pokémon {number} found, its artwork {}", if is_drawn { "drawn" } else { "not drawn" })
}

/// The reason an artwork is not drawn: its file is not read, it is too large, or its pixels are not painted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArtworkFault {
    Unreadable(png::Error),
    TooLarge(usize, usize),
    Unpainted(png::Error),
}

impl fmt::Display for ArtworkFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtworkFault::Unreadable(error) => write!(f, "the artwork is {error:?}"),
            ArtworkFault::TooLarge(width, height) => write!(f, "an artwork of {width}x{height} pixels"),
            ArtworkFault::Unpainted(error) => write!(f, "the artwork's pixels are {error:?}"),
        }
    }
}

impl core::error::Error for ArtworkFault {}

/// Reads an artwork's file, refusing one too large before its image data is inflated.
pub fn read_artwork(file: &[u8]) -> Result<Png, ArtworkFault> {
    Png::read(file, ARTWORK_MAX_PIXELS).map_err(|error| match error {
        png::Error::TooLarge(width, height) => ArtworkFault::TooLarge(width, height),
        error => ArtworkFault::Unreadable(error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::json::{text_at, unescape, value_at};
    use crate::gfx::png::samples::grey_png;

    #[test]
    fn queries_and_headers() {
        let names = names_body(Language::German);
        assert_eq!(text_at(&names, &["query"]).map(unescape).as_deref(), Some(NAMES_QUERY));
        assert_eq!(
            value_at(&names, &["variables", "languages"]),
            Some(r#"["de","en"]"#),
            "the language asked, and English where it has no name"
        );
        let entry = entry_body(6, Language::English);
        assert_eq!(
            (value_at(&entry, &["variables", "id"]), value_at(&entry, &["variables", "languages"])),
            (Some("6"), Some(r#"["en"]"#)),
            "English once"
        );
        assert_eq!(
            text_at(&entry, &["query"]).map(unescape).as_deref(),
            Some(ENTRY_QUERY),
            "the path of the artwork's address in its quotes, escaped"
        );
        assert!(value_at(&entry, &[]).is_some_and(|body| body.len() == entry.len()), "one JSON object");
        assert_eq!(
            QUERY_HEADERS.map(|(name, _)| name),
            ["User-Agent", "Accept", "Accept-Encoding", "Connection", "Content-Type"]
        );
        assert_eq!((QUERY_HEADERS[2].1, QUERY_HEADERS[4].1), ("identity", "application/json"));
        assert_eq!(HEADERS.map(|(name, _)| name), ["User-Agent", "Accept", "Accept-Encoding", "Connection"]);
        assert_eq!((HEADERS[0].1, HEADERS[2].1, HEADERS[3].1), (USER_AGENT, "identity", "close"));
    }

    #[test]
    fn languages_by_code() {
        assert_eq!(Language::ALL.map(Language::code), ["en", "de", "fr", "es", "it"]);
        assert!(Language::ALL.into_iter().all(|language| Language::parse(language.code()) == Some(language)));
        assert_eq!(
            (Language::parse("nl"), Language::parse("DE"), Language::default()),
            (None, None, Language::English)
        );
        assert_eq!(prompt(Language::French), "The name of a Pokémon, as the Pokédex writes it in French.");
    }

    #[test]
    fn lines_of_a_lookup() {
        assert_eq!(to_no_url_text("x"), "x is no URL");
        assert_eq!(
            to_names_line(2050, Language::German),
            r#"pokedex: PokéAPI gave 2050 names of Pokémon in ["de","en"]"#
        );
        assert_eq!(to_no_entry_line(151), "pokedex: PokéAPI's answer holds no entry of Pokémon 151");
        assert_eq!(
            (to_found_line(151, true), to_found_line(151, false)),
            (
                "pokedex: Pokémon 151 found, its artwork drawn".into(),
                "pokedex: Pokémon 151 found, its artwork not drawn".into()
            )
        );
    }

    #[test]
    fn artwork_size_limit() {
        let parsed = read_artwork(&grey_png(475, 475)).map(|png| (png.width, png.height, png.inflated_bytes()));
        assert_eq!(parsed, Ok((475, 475, 475 * 476)), "every artwork seen");
        assert!(read_artwork(&grey_png(ARTWORK_MAX_PIXELS as u32, ARTWORK_MAX_PIXELS as u32)).is_ok());
        assert_eq!(
            [
                read_artwork(&grey_png(ARTWORK_MAX_PIXELS as u32 + 1, 1)).err(),
                read_artwork(&grey_png(1, ARTWORK_MAX_PIXELS as u32 + 1)).err()
            ],
            [
                Some(ArtworkFault::TooLarge(ARTWORK_MAX_PIXELS + 1, 1)),
                Some(ArtworkFault::TooLarge(1, ARTWORK_MAX_PIXELS + 1))
            ]
        );
        assert_eq!(read_artwork(b"<html>").err(), Some(ArtworkFault::Unreadable(png::Error::NotPng)));
        let faults = [
            ArtworkFault::Unreadable(png::Error::Damaged),
            ArtworkFault::TooLarge(2048, 9),
            ArtworkFault::Unpainted(png::Error::BadData),
        ];
        assert_eq!(
            faults.map(|error| alloc::string::ToString::to_string(&error)),
            ["the artwork is Damaged", "an artwork of 2048x9 pixels", "the artwork's pixels are BadData"]
        );
    }
}
