//! Every Pokémon's names in a language, and the Pokémon whose name is nearest to one said or typed.

use super::Language;
use crate::base::json::{items, number_at, text_at, unescape};
use alloc::string::String;
use alloc::vec::Vec;

/// The names PokéAPI gave in a language, each with its Pokémon's number, in the order of the numbers.
pub struct Names {
    language: Language,
    keys: Vec<(String, u32)>,
}

// The plain letters a lowercase accented letter is written over.
const fn plain(letter: char) -> Option<&'static str> {
    Some(match letter {
        'à'..='å' => "a",
        'ç' => "c",
        'è'..='ë' => "e",
        'ì'..='ï' => "i",
        'ñ' => "n",
        'ò'..='ö' | 'ø' => "o",
        'ù'..='ü' => "u",
        'ý' | 'ÿ' => "y",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        _ => return None,
    })
}

// A name as names are compared: lowercase plain letters and digits, its words joined by a dash, its other signs left out.
pub(super) fn to_key(text: &str) -> String {
    let mut key = String::with_capacity(text.len());
    let mut has_gap = false;
    for letter in text.chars().flat_map(char::to_lowercase) {
        let accented = plain(letter);
        if accented.is_none() && !letter.is_ascii_alphanumeric() {
            has_gap |= letter.is_whitespace() || letter == '-';
            continue;
        }
        if has_gap && !key.is_empty() {
            key.push('-');
        }
        has_gap = false;
        match accented {
            Some(letters) => key.push_str(letters),
            None => key.push(letter),
        }
    }
    key
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b_letters: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_letters.len()).collect();
    for (row, letter) in a.chars().enumerate() {
        let mut current = Vec::with_capacity(previous.len());
        current.push(row + 1);
        for (column, b_letter) in b_letters.iter().enumerate() {
            let replace = previous[column] + usize::from(letter != *b_letter);
            current.push(replace.min(current[column] + 1).min(previous[column + 1] + 1));
        }
        previous = current;
    }
    previous[b_letters.len()]
}

impl Names {
    // The names an answer gives, asked in `language`; none where it gives none.
    pub(super) fn parse(answer: &str, language: Language) -> Option<Names> {
        let keys: Vec<(String, u32)> = items(answer, &["data", "pokemonspeciesname"])
            .filter_map(|item| {
                let key = to_key(&unescape(text_at(item, &["name"])?));
                Some((key, number_at(item, &["pokemon_species_id"])?)).filter(|(key, _)| !key.is_empty())
            })
            .collect();
        (!keys.is_empty()).then_some(Names { language, keys })
    }

    /// How many names there are.
    pub fn count(&self) -> usize {
        self.keys.len()
    }

    // The number of the Pokémon whose name is fewest edits from `text`, the first of those in the order of the numbers.
    pub(super) fn nearest(&self, text: &str) -> Option<u32> {
        let target = to_key(text);
        if target.is_empty() {
            return None;
        }
        self.keys.iter().min_by_key(|(key, _)| edit_distance(&target, key)).map(|(_, number)| *number)
    }
}

// True where the names held are none, or of another language than `language`.
pub(super) fn needs_names(names: Option<&Names>, language: Language) -> bool {
    names.is_none_or(|names| names.language != language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const ANSWER: &str = r#"{"data":{"pokemonspeciesname":[{"name":"Bisasam","pokemon_species_id":1}, {"name":"Bulbasaur","pokemon_species_id":1}, {"name":"Glurak","pokemon_species_id":6}, {"name":"Charizard","pokemon_species_id":6}, {"name":"Pantimos","pokemon_species_id":122}, {"name":"Mr. Mime","pokemon_species_id":122}, {"name":"Nidoran♀","pokemon_species_id":29}, {"name":"Évoli","pokemon_species_id":133}, {"name":"...","pokemon_species_id":9}, {"name":"Mew"}]}}"#;

    #[test]
    fn answer_names() {
        let names = Names::parse(ANSWER, Language::German).expect("names");
        assert_eq!(
            names.keys,
            vec![
                ("bisasam".into(), 1),
                ("bulbasaur".into(), 1),
                ("glurak".into(), 6),
                ("charizard".into(), 6),
                ("pantimos".into(), 122),
                ("mr-mime".into(), 122),
                ("nidoran".into(), 29),
                ("evoli".into(), 133)
            ],
            "every name with its number, in the order given; none without a letter or a number"
        );
        assert_eq!(names.count(), 8);
        assert!(Names::parse(r#"{"errors":[{"message":"x"}]}"#, Language::German).is_none(), "an answer of none");
        assert!(Names::parse("", Language::English).is_none());
        assert!(needs_names(None, Language::German) && needs_names(Some(&names), Language::French));
        assert!(!needs_names(Some(&names), Language::German), "names of the language asked are kept");
    }

    #[test]
    fn name_keys() {
        assert_eq!(
            [
                "Pikachu.",
                " Mr. Mime ",
                "Farfetch'd",
                "Ho-Oh!",
                "PORYGON 2",
                "Évoli",
                "Flabébé",
                "Mänteliöß",
                "Lucario ♂"
            ]
            .map(to_key),
            ["pikachu", "mr-mime", "farfetchd", "ho-oh", "porygon-2", "evoli", "flabebe", "mantelioss", "lucario"]
        );
        assert_eq!((to_key(" ... "), to_key("")), (String::new(), String::new()));
    }

    #[test]
    fn nearest_name() {
        let names = Names::parse(ANSWER, Language::German).expect("names");
        assert_eq!(
            [names.nearest("Glurak"), names.nearest("Glurack"), names.nearest("Charizard"), names.nearest("evoli")],
            [Some(6), Some(6), Some(6), Some(133)],
            "a name of the language, one letter heard wrong, its English name, and without its accent"
        );
        assert_eq!(names.nearest("Mister Mime"), Some(122));
        assert_eq!(names.nearest("?"), None, "nothing said");
        assert_eq!(
            (edit_distance("kitten", "sitting"), edit_distance("", "abc"), edit_distance("abc", "abc")),
            (3, 3, 0)
        );
    }
}
