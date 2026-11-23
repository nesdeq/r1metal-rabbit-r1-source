use super::bar::BarWords;
use super::typing::KeyboardId;
use super::voice::{PageId, ScreenPage};
use super::{Screen, View};
use crate::base::math::round;
use crate::base::text::to_capitalized;
use crate::domain::chat::tools::Destination;
use crate::domain::pokedex::Language;
use crate::domain::pokedex::entry::Entry;
use crate::domain::pokedex::state::Article;
use crate::domain::scene::{KEYS, POKEDEX};
use crate::domain::uses::Use;
use crate::domain::voice::{Bid, Phase};
use crate::gfx::measures::PHI;
use crate::ui::Ui;
use crate::ui::keys::take_typed;
use crate::ui::words::{HOLD_TO_TALK, VOICE};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{Display, Write};

const TYPE_A_NAME: &str = "TYPE A NAME";
const SAY_OR_TYPE: &str = "SAY OR TYPE A NAME";
const TENTHS: u32 = 10;

// The words of an entry's text, and the sign of its decimals, in the language it is written in.
struct Words {
    stats: &'static str,
    abilities: &'static str,
    evolution: &'static str,
    moves: &'static str,
    hidden: &'static str,
    height: &'static str,
    weight: &'static str,
    decimal: char,
}

const fn words(language: Language) -> Words {
    match language {
        Language::English => Words {
            stats: "Base stats",
            abilities: "Abilities",
            evolution: "Evolution chain",
            moves: "Moves",
            hidden: "hidden",
            height: "Height",
            weight: "Weight",
            decimal: '.',
        },
        Language::German => Words {
            stats: "Basiswerte",
            abilities: "Fähigkeiten",
            evolution: "Entwicklungsreihe",
            moves: "Attacken",
            hidden: "versteckt",
            height: "Größe",
            weight: "Gewicht",
            decimal: ',',
        },
        Language::French => Words {
            stats: "Statistiques de base",
            abilities: "Talents",
            evolution: "Évolutions",
            moves: "Capacités",
            hidden: "caché",
            height: "Taille",
            weight: "Poids",
            decimal: ',',
        },
        Language::Spanish => Words {
            stats: "Estadísticas base",
            abilities: "Habilidades",
            evolution: "Cadena evolutiva",
            moves: "Movimientos",
            hidden: "oculta",
            height: "Altura",
            weight: "Peso",
            decimal: ',',
        },
        Language::Italian => Words {
            stats: "Statistiche di base",
            abilities: "Abilità",
            evolution: "Catena evolutiva",
            moves: "Mosse",
            hidden: "nascosta",
            height: "Altezza",
            weight: "Peso",
            decimal: ',',
        },
    }
}

fn to_number(number: u32) -> String {
    format!("#{number:04}")
}

fn to_pair(name: &str, value: impl Display) -> String {
    format!("**{name}** {value}")
}

fn write_list(text: &mut String, heading: Option<&str>, items: Vec<String>) {
    if items.is_empty() {
        return;
    }
    if let Some(heading) = heading {
        let _ = writeln!(text, "## {heading}\n");
    }
    for item in items {
        let _ = writeln!(text, "- {item}");
    }
    text.push('\n');
}

fn to_text(entry: &Entry) -> String {
    let words = words(entry.language);
    let mut text = format!("# {}\n\n", to_capitalized(&entry.name));
    let number = (entry.number > 0).then(|| format!("**{}**", to_number(entry.number)));
    let heads: Vec<String> = number
        .into_iter()
        .chain([entry.kind.clone(), entry.types.join(", ")])
        .filter(|part| !part.is_empty())
        .collect();
    if !heads.is_empty() {
        let _ = writeln!(text, "{}\n", heads.join(" \u{B7} "));
    }
    if !entry.description.is_empty() {
        let _ = writeln!(text, "{}\n", entry.description);
    }
    let measure = |tenths: u32, unit: &str| format!("{}{}{} {unit}", tenths / TENTHS, words.decimal, tenths % TENTHS);
    let facts = [(words.height, entry.height, "m"), (words.weight, entry.weight, "kg")];
    let facts = facts.into_iter().filter_map(|(name, tenths, unit)| Some(to_pair(name, measure(tenths?, unit))));
    write_list(&mut text, None, facts.collect());
    write_list(&mut text, Some(words.stats), entry.stats.iter().map(|(name, value)| to_pair(name, value)).collect());
    let ability_line = |ability: &(String, bool)| {
        if ability.1 { format!("{}, {}", ability.0, words.hidden) } else { ability.0.clone() }
    };
    write_list(&mut text, Some(words.abilities), entry.abilities.iter().map(ability_line).collect());
    write_list(&mut text, Some(words.evolution), entry.evolution.clone());
    if !entry.moves.is_empty() {
        let _ = writeln!(text, "## {}\n\n{}", words.moves, entry.moves.join(", "));
    }
    text.truncate(text.trim_end().len());
    text
}

// An article as its screen shows it: none of it is read aloud.
fn to_page(article: Article) -> ScreenPage {
    let Article { entry, picture } = article;
    let name = entry.name.to_uppercase();
    let title = if entry.number == 0 { name } else { format!("{} {name}", to_number(entry.number)) };
    ScreenPage { title, text: to_text(&entry), picture, spoken: String::new() }
}

impl Ui {
    /// True while the Pokédex shows and the device is awake.
    pub fn is_pokedex_wanted(&self) -> bool {
        self.is_awake() && self.does_pokedex_show()
    }

    pub(crate) fn does_pokedex_show(&self) -> bool {
        self.screen == Some(Screen::Pokedex)
    }

    /// The bids the Pokédex's screen made since this was last taken.
    pub fn take_pokedex_bids(&mut self) -> Vec<Bid> {
        core::mem::take(&mut self.pokedex.bids)
    }

    /// The side of the square an artwork is fitted into.
    pub fn artwork_side(&self) -> usize {
        round(self.ruler.under_bar().w as f32 / PHI).max(0) as usize
    }

    pub(super) fn open_pokedex(&mut self) {
        self.screen = Some(Screen::Pokedex);
        self.open_paged(PageId::Pokedex);
    }

    /// Takes the lookup's phase and its article; true if the screen changed.
    #[must_use]
    pub fn set_pokedex(&mut self, phase: Phase, article: Option<Article>, now_ns: u64) -> bool {
        let has_new_phase = self.take_paged_phase(PageId::Pokedex, phase, now_ns);
        let has_article = article.is_some();
        if let Some(article) = article {
            self.show_page(PageId::Pokedex, to_page(article), now_ns);
        }
        let is_line = has_article && self.redeal_lines(&[Use::Open(Destination::Pokedex)]);
        (has_new_phase || has_article) && self.does_pokedex_show() || is_line
    }

    pub(super) fn press_pokedex(&mut self, now_ns: u64) {
        if self.pokedex.is_typing {
            self.hold_start = Some(now_ns);
            return;
        }
        self.pokedex.voice.is_talking = true;
        self.pokedex.bids.push(Bid::Listen);
    }

    pub(super) fn release_pokedex(&mut self, now_ns: u64) {
        if core::mem::take(&mut self.pokedex.voice.is_talking) {
            self.pokedex.bids.push(Bid::Send);
            return;
        }
        if self.pokedex.is_typing && self.hold_start.take().is_some() && self.pokedex.keys.ask_done() {
            self.send_pokedex(now_ns);
        }
    }

    pub(super) fn send_pokedex(&mut self, now_ns: u64) {
        if let Some(text) = take_typed(&mut self.pokedex.keys) {
            self.pokedex.bids.push(Bid::Text(text));
            self.toggle_paged_keys(PageId::Pokedex, now_ns);
        }
    }

    pub(in crate::ui) fn leave_pokedex(&mut self) {
        if self.does_pokedex_show() {
            self.pokedex.keys.release();
            self.let_go_pokedex();
        }
    }

    pub(crate) fn let_go_pokedex(&mut self) {
        self.pokedex.voice.is_talking = false;
    }

    pub(super) fn pokedex_word(&self) -> &str {
        self.pokedex.page.as_ref().map_or(SAY_OR_TYPE, |page| page.title.as_str())
    }

    pub(super) fn view_pokedex<'a>(&'a self, word: &'a mut String, now_ns: u64) -> View<'a> {
        if !self.pokedex.voice.write_word(now_ns, word) && self.pokedex.page.is_none() {
            word.push_str(if self.pokedex.is_typing { TYPE_A_NAME } else { HOLD_TO_TALK });
        }
        let status: &'a str = word;
        let words = BarWords {
            status: Some(status).filter(|status| !status.is_empty()),
            subject: self.pokedex.page.as_ref().map(|page| page.title.as_str()),
            state: Some(if self.pokedex.is_typing { KEYS } else { VOICE }),
            ..BarWords::default()
        };
        let keys = Some((KeyboardId::Pokedex, self.pokedex.band.at));
        View::Text { name: POKEDEX, words, room: self.pokedex.room.plate.as_room(), keys }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::pokedex::entry::tests::answer;

    #[test]
    fn entry_text() {
        let german = Entry::parse(&answer(), Language::German).unwrap();
        assert_eq!(
            to_text(&german),
            "# Glurak\n\n**#0006** \u{B7} Flammen-Pokémon \u{B7} Feuer, Flug\n\nIt spits fire that is hot enough.\n\n- **Größe** 1,7 m\n- **Gewicht** 90,5 kg\n\n## Basiswerte\n\n- **KP** 78\n- **Initiative** 100\n\n## Fähigkeiten\n\n- Großbrand\n- Solar Power, versteckt\n\n## Entwicklungsreihe\n\n- Glumanda\n- Glutexo\n- Glurak\n\n## Attacken\n\nÄtzer, Biss, Ölschicht",
            "in German, its decimals after a comma"
        );
        let english = Entry::parse(&answer(), Language::English).unwrap();
        assert!(
            to_text(&english).contains("- **Height** 1.7 m\n- **Weight** 90.5 kg\n\n## Base stats\n\n- **HP** 78"),
            "{}",
            to_text(&english)
        );
        let name_only = Entry { name: "missingno".into(), ..Entry::default() };
        assert_eq!(to_text(&name_only), "# Missingno", "a part it does not tell is left out, heading and all");
        assert_eq!((to_number(6), to_number(1025)), ("#0006".into(), "#1025".into()));
    }
}
