//! The models and addresses of OpenAI's services, with their parameters and time limits, and the language model and effort a use is set to.

use core::time::Duration;

/// A language model a use may be set to, the cheapest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Model {
    #[default]
    Luna,
    Terra,
    Sol,
    Astra,
}

impl Model {
    pub(crate) const ALL: [Model; 4] = [Model::Luna, Model::Terra, Model::Sol, Model::Astra];

    pub(crate) const fn to_word(self) -> &'static str {
        match self {
            Model::Luna => "gpt-6-luna",
            Model::Terra => "gpt-6.1-terra",
            Model::Sol => "gpt-6.1-sol",
            Model::Astra => "gpt-6-astra",
        }
    }

    pub(crate) fn parse(word: &str) -> Option<Model> {
        Model::ALL.into_iter().find(|model| model.to_word() == word)
    }

    // The model an answer names, a snapshot's date behind the name included; Luna for any other name.
    pub(super) fn named(name: &str) -> Model {
        let is_named = |model: &Model| {
            name.strip_prefix(model.to_word()).is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
        };
        Model::ALL.into_iter().find(is_named).unwrap_or_default()
    }

    const fn least_effort(self) -> Effort {
        match self {
            Model::Luna | Model::Terra => Effort::None,
            Model::Sol | Model::Astra => Effort::Low,
        }
    }
}

/// How hard a language model thinks, the least first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Effort {
    None,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub(crate) const ALL: [Effort; 6] =
        [Effort::None, Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max];

    pub(crate) const fn to_word(self) -> &'static str {
        match self {
            Effort::None => "none",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub(crate) fn parse(word: &str) -> Option<Effort> {
        Effort::ALL.into_iter().find(|effort| effort.to_word() == word)
    }
}

/// A language model and the effort it thinks at, one the model takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Thinking {
    model: Model,
    effort: Effort,
}

impl Thinking {
    // What every use is set to on a new device: the owner's choice.
    pub(crate) const STANDARD: Thinking = Thinking::new(Model::Terra, Effort::High);

    // The model at that effort, or at the least it takes above it.
    pub(crate) const fn new(model: Model, effort: Effort) -> Thinking {
        let least = model.least_effort();
        Thinking { model, effort: if (effort as u8) < (least as u8) { least } else { effort } }
    }

    pub(crate) const fn model(self) -> Model {
        self.model
    }

    pub(crate) const fn effort(self) -> Effort {
        self.effort
    }

    // The efforts the model takes, the least first.
    pub(crate) fn efforts(self) -> impl Iterator<Item = Effort> {
        Effort::ALL.into_iter().filter(move |effort| *effort >= self.model.least_effort())
    }
}

/// A use of the language model, each set to a model and effort of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Task {
    Chat,
    Recollect,
    // The summaries of notes and of translations.
    Summary,
    News,
    Auto,
    // The browser's resolving of what was asked and its writing of a page.
    Web,
    // The reading of a request said or typed to the bell.
    Bell,
    // The reading of a request for the rundowns, with the search for a new one's feeds.
    Rundowns,
}

impl Task {
    pub(crate) const ALL: [Task; 8] =
        [Task::Chat, Task::Recollect, Task::Summary, Task::News, Task::Auto, Task::Web, Task::Bell, Task::Rundowns];

    // The word the settings' file keeps it by.
    pub(crate) const fn to_word(self) -> &'static str {
        match self {
            Task::Chat => "robot",
            Task::Recollect => "recollection",
            Task::Summary => "notes",
            Task::News => "news",
            Task::Auto => "auto",
            Task::Web => "web",
            Task::Bell => "bell",
            Task::Rundowns => "rundowns",
        }
    }

    pub(crate) fn parse(word: &str) -> Option<Task> {
        Task::ALL.into_iter().find(|task| task.to_word() == word)
    }
}

pub(super) const VOICE: &str = "marin";
/// The limit on each piece of an answer before the connection counts as dead: as long as a long recording's words, the conversation's thinking or a picture may take.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// The model list, where a key proves itself.
pub mod check {
    /// Listing the models costs nothing.
    pub const ADDRESS: &str = "https://api.openai.com/v1/models";
}

/// The image model that redraws a photo as pixel art.
pub mod image {
    pub const ADDRESS: &str = "https://api.openai.com/v1/images/edits";
    pub(crate) const MODEL: &str = "gpt-image-2.5-flare";
    pub(crate) const QUALITY: &str = "medium";
    /// The pixels along a side of the picture both ways: both are paid for by their size.
    pub const SIDE: usize = 1024;
    pub(crate) const FORMAT: &str = "jpeg";
    pub(crate) const COMPRESSION: &str = "90";
}

/// The speech model that writes a recording's words down.
pub mod transcriber {
    pub const ADDRESS: &str = "https://api.openai.com/v1/audio/transcriptions";
    pub(crate) const MODEL: &str = "gpt-transcribe";
}

/// The language model's background work, whatever use asks for it.
pub mod writer {
    /// Pending work is asked after at this address with its id behind it.
    pub const ADDRESS: &str = "https://api.openai.com/v1/responses";
    pub(crate) const VERBOSITY: &str = "low";
}

/// The robot's conversation: its web search and where its thread is folded.
pub mod chat {
    use crate::base::time::SECONDS_PER_HOUR;

    /// The Responses API's WebSocket mode.
    pub const ADDRESS: &str = "wss://api.openai.com/v1/responses";
    pub(crate) const SEARCH_CONTEXT: &str = "high";
    pub(crate) const SEARCH_BUDGET: &str = "unlimited";
    pub(crate) const COMPACT_THRESHOLD: u64 = 200_000;
    pub(crate) const CACHE_HOURS: u64 = 12;
    // How closely the model looks at a photo the user shows it.
    pub(crate) const DETAIL: &str = "high";
    pub(crate) const CACHE_DURATION: super::Duration = super::Duration::from_secs(CACHE_HOURS * SECONDS_PER_HOUR);
}

/// The translator's live model.
pub mod interpreter {
    pub const ADDRESS: &str = "wss://api.openai.com/v1/live/sessions";
    pub(crate) const MODEL: &str = "gpt-live-1";
    /// The limit on the service's word that a session stands.
    pub const TIMEOUT: super::Duration = super::Duration::from_secs(15);
    /// The sample rate both ways, one channel of 16 bits: the rate the model takes that the microphone records at.
    pub const RATE: u32 = 24_000;
}

/// The realtime model that reads a text aloud.
pub mod reader {
    /// The address names the model.
    pub const ADDRESS: &str = "wss://api.openai.com/v1/realtime?model=gpt-realtime-2.1-mini";
    /// The limit on the session's setup and on each piece of an answer after that.
    pub const TIMEOUT: super::Duration = super::Duration::from_secs(15);
    pub(crate) const EFFORT: &str = "minimal";
    pub(crate) const RATE: u32 = 24_000;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn words_round_trip() {
        assert!(Model::ALL.into_iter().all(|model| Model::parse(model.to_word()) == Some(model)));
        assert!(Effort::ALL.into_iter().all(|effort| Effort::parse(effort.to_word()) == Some(effort)));
        assert!(Task::ALL.into_iter().all(|task| Task::parse(task.to_word()) == Some(task)));
        assert_eq!((Model::parse("gpt-6"), Effort::parse("minimal"), Task::parse("chat")), (None, None, None));
    }

    #[test]
    fn model_named_by_answer() {
        assert_eq!(
            ["gpt-6.1-sol-2026-09-30", "gpt-6-astra", "gpt-6.1-terra", "gpt-6-luna-x", "gpt-6.1-solar", "", "o9"]
                .map(Model::named),
            [Model::Sol, Model::Astra, Model::Terra, Model::Luna, Model::Luna, Model::Luna, Model::Luna],
            "a snapshot by its model, and any other name as the cheapest"
        );
    }

    #[test]
    fn effort_a_model_takes() {
        assert_eq!(Thinking::new(Model::Sol, Effort::None).effort(), Effort::Low, "the least it takes above it");
        assert_eq!(Thinking::new(Model::Terra, Effort::None).effort(), Effort::None);
        assert_eq!(Thinking::new(Model::Astra, Effort::Max).effort(), Effort::Max);
        assert_eq!(Thinking::new(Model::Astra, Effort::High).efforts().count(), 5);
        assert_eq!(Thinking::new(Model::Luna, Effort::High).efforts().collect::<Vec<_>>(), Effort::ALL);
        assert_eq!(
            (Thinking::STANDARD.model().to_word(), Thinking::STANDARD.effort().to_word()),
            ("gpt-6.1-terra", "high"),
            "a new device's, for every use"
        );
    }
}
