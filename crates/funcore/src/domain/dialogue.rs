//! A conversation the translator rendered.

use crate::base::text::has_words;
use alloc::string::String;
use alloc::vec::Vec;

/// A remark as spoken and as translated.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Turn {
    pub source: String,
    pub translation: String,
}

/// The remarks of a conversation, built from the pieces that arrive.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Dialogue {
    pub turns: Vec<Turn>,
    reply_ms: Option<u64>,
}

impl Dialogue {
    pub(super) fn add_source(&mut self, piece: &str, start_ms: Option<u64>) {
        let is_next = match (self.reply_ms, start_ms) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(reply), Some(start)) => start >= reply,
        };
        if let Some(turn) = self.turns.last_mut().filter(|_| !is_next) {
            turn.source.push_str(piece);
        } else {
            self.turns.push(Turn { source: piece.into(), translation: String::new() });
            self.reply_ms = None;
        }
    }

    pub(super) fn add_translation(&mut self, piece: &str, sent_ms: u64) {
        self.reply_ms = self.reply_ms.or(Some(sent_ms));
        if self.turns.is_empty() {
            self.turns.push(Turn::default());
        }
        if let Some(turn) = self.turns.last_mut() {
            turn.translation.push_str(piece);
        }
    }

    /// True once someone spoke whom the translator had to render.
    pub fn has_speech(&self) -> bool {
        self.turns.iter().any(|turn| has_words(&turn.source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remarks_from_pieces() {
        let mut dialogue = Dialogue::default();
        assert!(dialogue.turns.is_empty() && !dialogue.has_speech());
        dialogue.add_translation("Hello, I am ", 400);
        dialogue.add_translation("the translator.", 900);
        assert!(!dialogue.has_speech());
        dialogue.add_source("Wo ist ", Some(6000));
        dialogue.add_source("der Bahnhof?", Some(6600));
        dialogue.add_translation("Where is ", 8200);
        dialogue.add_source(" Bitte.", Some(7400));
        dialogue.add_translation("the station? Please.", 8900);
        dialogue.add_source("Over there.", Some(12_000));
        dialogue.add_translation("Dort dr\u{FC}ben.", 14_000);
        dialogue.add_source("Thanks", None);
        dialogue.add_source(" a lot.", None);
        let turn = |source: &str, translation: &str| Turn { source: source.into(), translation: translation.into() };
        assert_eq!(
            dialogue.turns,
            [
                turn("", "Hello, I am the translator."),
                turn("Wo ist der Bahnhof? Bitte.", "Where is the station? Please."),
                turn("Over there.", "Dort dr\u{FC}ben."),
                turn("Thanks a lot.", "")
            ]
        );
        assert!(dialogue.has_speech());
    }
}
