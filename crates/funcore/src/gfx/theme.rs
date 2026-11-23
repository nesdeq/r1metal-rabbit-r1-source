//! The interface themes.

/// A layout and drawing style of the whole interface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Stage,
    Dial,
    Deck,
    Terminal,
    Companion,
    Rail,
}

/// Themes in menu order.
pub const THEMES: [Theme; 6] = [Theme::Stage, Theme::Dial, Theme::Deck, Theme::Terminal, Theme::Companion, Theme::Rail];

impl Theme {
    pub(crate) const STANDARD: Theme = Theme::Dial;

    pub(crate) fn at(index: usize) -> Theme {
        THEMES.get(index).copied().unwrap_or(Theme::STANDARD)
    }

    /// The settings file's word for a theme.
    pub fn to_word(self) -> &'static str {
        match self {
            Theme::Stage => "stage",
            Theme::Dial => "dial",
            Theme::Deck => "deck",
            Theme::Terminal => "terminal",
            Theme::Companion => "companion",
            Theme::Rail => "rail",
        }
    }

    /// The theme the settings file's word names.
    pub fn parse(word: &str) -> Option<Theme> {
        let word = word.trim();
        THEMES.into_iter().find(|theme| theme.to_word() == word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_by_word_and_index() {
        for (index, theme) in THEMES.into_iter().enumerate() {
            assert_eq!((theme as usize, Theme::at(index), Theme::parse(theme.to_word())), (index, theme, Some(theme)));
        }
        assert_eq!((Theme::at(99), Theme::parse("C"), Theme::parse("")), (Theme::STANDARD, None, None));
    }
}
