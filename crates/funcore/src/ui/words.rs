use crate::domain::camera::Outcome;
use crate::domain::cause::Cause;
use crate::gfx::palette;
use crate::gfx::theme::Theme;
use alloc::string::String;

pub(super) const HOLD_TO_TALK: &str = "HOLD TO TALK";
pub(super) const TRANSCRIBING: &str = "TRANSCRIBING";
pub(super) const READING: &str = "READING";
pub(super) const LISTENING: &str = "LISTENING";
pub(super) const CONNECTING: &str = "CONNECTING";
pub(super) const STARTING: &str = "STARTING";
pub(super) const JOINING: &str = "JOINING";
pub(super) const OFF: &str = "OFF";
pub(super) const VOICE: &str = "VOICE";
pub(super) const WAITING: &str = "WAITING";
pub(super) const WHAT_YOU_SAY: &str = "WHAT YOU SAY";
pub(super) const RINGING: &str = "RINGING";
pub(super) const ENDED: &str = "ENDED";
pub(super) const ENDING: &str = "ENDING";
// Before a line's word, of what the owner has not read or seen yet.
pub(super) const NEW_MARK: &str = "NEW: ";
pub(super) const NO_CONTACTS: &str = "NO CONTACTS";
pub(super) const UNREADABLE: &str = "UNREADABLE";
const SAVED: &str = "SAVED";
pub(super) const NO_TIME: &str = "NO TIME";
pub(super) const CLOCK_SET: &str = "CLOCK SET";
pub(super) const CLOCK_RIGHT: &str = "CLOCK IS RIGHT";

pub(super) const fn cause_word(cause: Cause) -> &'static str {
    match cause {
        Cause::NoKey => "NO OPENAI KEY",
        Cause::Limit => "LIMIT REACHED",
        Cause::NoNetwork => "NO NETWORK",
        Cause::NoAnswer => "NO ANSWER",
        Cause::Refused => "REFUSED",
        Cause::NotWritten => "NOT WRITTEN",
        Cause::SessionFailed => "FAILED",
        Cause::NoStorage => "NO STORAGE",
        Cause::StorageFull => "STORAGE FULL",
        Cause::NotDeleted => "NOT DELETED",
        Cause::NotRecorded => "NOT RECORDED",
        Cause::NotPlayed => "NOT PLAYED",
        Cause::NotHeard => "NOT HEARD",
        Cause::NoFeed => "NO FEED FOUND",
        Cause::PixelsRefused => "PIXELS REFUSED",
        Cause::NoPixels => "NO PIXELS",
        Cause::CodeExpired => "CODE EXPIRED",
        Cause::TooOld => "TOO OLD",
        Cause::TooManyDevices => "TOO MANY DEVICES",
    }
}

pub(super) const fn outcome_word(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Saved(_) => SAVED,
        Outcome::NoPhoto => "PHOTO FAILED",
        Outcome::NoVideo => "VIDEO FAILED",
        Outcome::Full => cause_word(Cause::StorageFull),
        Outcome::TooWarm => "CAMERA TOO WARM",
        Outcome::LowBattery => "BATTERY TOO LOW",
        Outcome::Failed => "CAMERA FAILED",
    }
}

pub(super) const fn theme_name(theme: Theme) -> &'static str {
    match theme {
        Theme::Stage => "STAGE",
        Theme::Dial => "DIAL",
        Theme::Deck => "DECK",
        Theme::Terminal => "TERMINAL",
        Theme::Companion => "COMPANION",
        Theme::Rail => "RAIL",
    }
}

pub(super) const PALETTE_NAMES: [&str; 16] = [
    "SAND", "SLATE", "AMBER", "GRAPHITE", "SUNSET", "NIXIE", "CORAL", "ROSE", "ORCHID", "IRIS", "ICE", "ARCTIC",
    "LAGOON", "AURORA", "PHOSPHOR", "MOSS",
];

pub(super) fn write_palette_name(at: usize, text: &mut String) {
    text.push(palette::letter(at));
    text.push(' ');
    text.push_str(PALETTE_NAMES[at]);
}

pub(super) const fn theme_summary(theme: Theme) -> &'static str {
    match theme {
        Theme::Stage => "ONE LARGE ICON AT A TIME",
        Theme::Dial => "THE WHEEL ROLLS EVERYTHING",
        Theme::Deck => "EVERYTHING ON A CARD",
        Theme::Terminal => "MENUS MADE OF TYPE",
        Theme::Companion => "THE ROBOT LEADS",
        Theme::Rail => "THE RAIL KEEPS YOUR PLACE",
    }
}
