//! A Pokémon's entry, as PokéAPI's answer gives it in a language, English filling in what the language lacks.

use super::Language;
use super::names::to_key;
use crate::base::json::{items, number_at, text_at, unescape, value_at};
use crate::base::text::to_one_line;
use alloc::string::String;
use alloc::vec::Vec;

const NAME: &str = "name";

/// A Pokémon's entry in a language.
#[derive(Default, Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub(crate) language: Language,
    pub(crate) name: String,
    /// Its number in the National Pokédex.
    pub number: u32,
    pub(crate) kind: String,
    pub(crate) types: Vec<String>,
    pub(crate) description: String,
    // In tenths of a metre and of a kilogram, as PokéAPI counts them.
    pub(crate) height: Option<u32>,
    pub(crate) weight: Option<u32>,
    pub(crate) stats: Vec<(String, u32)>,
    // Each ability, and whether it is hidden.
    pub(crate) abilities: Vec<(String, bool)>,
    pub(crate) evolution: Vec<String>,
    pub(crate) moves: Vec<String>,
    // The address of its official artwork.
    pub(crate) artwork: Option<String>,
}

// Of a list of texts in languages, the field of the one in `language`, else of the English one.
fn localized<'a>(texts: impl Iterator<Item = &'a str>, field: &str, language: Language) -> Option<String> {
    let mut english = None;
    for text in texts {
        let code = text_at(text, &["language", NAME]);
        let value = || text_at(text, &[field]).map(unescape);
        if code == Some(language.code()) {
            return value();
        }
        if code == Some(Language::English.code()) && english.is_none() {
            english = value();
        }
    }
    english
}

impl Entry {
    // Parses an answer to the entry's query in `language`; none where it holds no Pokémon.
    pub(crate) fn parse(answer: &str, language: Language) -> Option<Entry> {
        let species = items(answer, &["data", "pokemonspecies"]).next()?;
        let names = || items(species, &["pokemonspeciesnames"]);
        let name = localized(names(), NAME, language)?;
        let pokemon = items(species, &["pokemons"]).next().unwrap_or_default();
        let named = |list: &'static [&'static str]| {
            move |item: &str| localized(items(item, list), NAME, language).filter(|name| !name.is_empty())
        };
        let mut moves: Vec<String> =
            items(pokemon, &["pokemonmoves"]).filter_map(named(&["move", "movenames"])).collect();
        moves.sort_by_cached_key(|name| to_key(name));
        moves.dedup();
        Some(Entry {
            language,
            name,
            number: number_at(species, &["id"])?,
            kind: localized(names(), "genus", language).unwrap_or_default(),
            types: items(pokemon, &["pokemontypes"]).filter_map(named(&["type", "typenames"])).collect(),
            description: localized(items(species, &["pokemonspeciesflavortexts"]), "flavor_text", language)
                .map(|text| to_one_line(&text))
                .unwrap_or_default(),
            height: number_at(pokemon, &["height"]),
            weight: number_at(pokemon, &["weight"]),
            stats: items(pokemon, &["pokemonstats"])
                .filter_map(|stat| Some((named(&["stat", "statnames"])(stat)?, number_at(stat, &["base_stat"])?)))
                .collect(),
            abilities: items(pokemon, &["pokemonabilities"])
                .filter_map(|ability| {
                    let is_hidden = value_at(ability, &["is_hidden"]) == Some("true");
                    Some((named(&["ability", "abilitynames"])(ability)?, is_hidden))
                })
                .collect(),
            evolution: items(species, &["evolutionchain", "pokemonspecies"])
                .filter_map(named(&["pokemonspeciesnames"]))
                .collect(),
            moves,
            artwork: text_at(pokemon, &["pokemonsprites", "sprites"]).map(unescape),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::vec;

    fn names(german: &str, english: &str) -> String {
        alloc::format!(
            r#"[{{"name":"{german}","language":{{"name":"de"}}}}, {{"name":"{english}","language":{{"name":"en"}}}}]"#
        )
    }

    // An answer of PokéAPI's to the entry's query in German, cut short, the English filling in where the German has no text.
    pub(crate) fn answer() -> String {
        alloc::format!(
            r#"{{"data":{{"pokemonspecies":[{{"id":6,"pokemonspeciesnames":[{{"name":"Glurak","genus":"Flammen-Pokémon","language":{{"name":"de"}}}}, {{"name":"Charizard","genus":"Flame Pokémon","language":{{"name":"en"}}}}],"pokemonspeciesflavortexts":[{{"flavor_text":"It spits fire\nthat is hot\fenough.","language":{{"name":"en"}}}}],"evolutionchain":{{"pokemonspecies":[{{"pokemonspeciesnames":{}}}, {{"pokemonspeciesnames":{}}}, {{"pokemonspeciesnames":{}}}]}},"pokemons":[{{"height":17,"weight":905,"pokemontypes":[{{"type":{{"typenames":{}}}}}, {{"type":{{"typenames":{}}}}}],"pokemonstats":[{{"base_stat":78,"stat":{{"statnames":{}}}}}, {{"base_stat":100,"stat":{{"statnames":{}}}}}],"pokemonabilities":[{{"is_hidden":false,"ability":{{"abilitynames":{}}}}}, {{"is_hidden":true,"ability":{{"abilitynames":[{{"name":"Solar Power","language":{{"name":"en"}}}}]}}}}],"pokemonsprites":[{{"sprites":"https:\/\/raw.githubusercontent.com\/PokeAPI\/sprites\/master\/sprites\/pokemon\/other\/official-artwork\/6.png"}}],"pokemonmoves":[{{"move":{{"movenames":{}}}}}, {{"move":{{"movenames":{}}}}}, {{"move":{{"movenames":{}}}}}]}}]}}]}}}}"#,
            names("Glumanda", "Charmander"),
            names("Glutexo", "Charmeleon"),
            names("Glurak", "Charizard"),
            names("Feuer", "Fire"),
            names("Flug", "Flying"),
            names("KP", "HP"),
            names("Initiative", "Speed"),
            names("Großbrand", "Blaze"),
            names("Ölschicht", "Oil Coat"),
            names("Ätzer", "Acid"),
            names("Biss", "Bite"),
        )
    }

    #[test]
    fn answer_entry() {
        let entry = Entry::parse(&answer(), Language::German).expect("an entry");
        assert_eq!(
            entry,
            Entry {
                language: Language::German,
                name: "Glurak".into(),
                number: 6,
                kind: "Flammen-Pokémon".into(),
                types: vec!["Feuer".into(), "Flug".into()],
                description: "It spits fire that is hot enough.".into(),
                height: Some(17),
                weight: Some(905),
                stats: vec![("KP".into(), 78), ("Initiative".into(), 100)],
                abilities: vec![("Großbrand".into(), false), ("Solar Power".into(), true)],
                evolution: vec!["Glumanda".into(), "Glutexo".into(), "Glurak".into()],
                moves: vec!["Ätzer".into(), "Biss".into(), "Ölschicht".into()],
                artwork: Some(
                    "https://raw.githubusercontent.com/PokeAPI/sprites/master/sprites/pokemon/other/official-artwork/6.png"
                        .into()
                ),
            },
            "each text in German, the English where there is none, on one line; the moves by their letters, an accented one as its plain one"
        );
        let english = Entry::parse(&answer(), Language::English).expect("an entry");
        assert_eq!(
            (english.name.as_str(), english.kind.as_str(), english.evolution.last().map(String::as_str)),
            ("Charizard", "Flame Pokémon", Some("Charizard"))
        );
        assert_eq!(Entry::parse(r#"{"data":{"pokemonspecies":[]}}"#, Language::German), None, "no such Pokémon");
        assert_eq!(Entry::parse(r#"{"errors":[{"message":"x"}]}"#, Language::German), None);
        let bare = r#"{"data":{"pokemonspecies":[{"id":9,"pokemonspeciesnames":[{"name":"Turtok","language":{"name":"de"}}]}]}}"#;
        assert_eq!(
            Entry::parse(bare, Language::German),
            Some(Entry { language: Language::German, name: "Turtok".into(), number: 9, ..Entry::default() }),
            "a part it does not tell is empty"
        );
    }
}
