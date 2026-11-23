use super::action::Action;
use super::cost;
use super::pin::write_tries;
use super::tree::{Content, Node, holds};
use crate::base::time::SECONDS_PER_MINUTE;
use crate::cell::modem::Status;
use crate::domain::chat::tools::Destination;
use crate::domain::device::Part;
use crate::domain::pokedex::Language;
use crate::domain::router::{Report, Standing};
use crate::domain::settings::{Setting, Settings, Tool, Value};
use crate::domain::uses::Use;
use crate::ui::Ui;
use crate::ui::view::lines::List;
use crate::ui::words::{CONNECTING, OFF, STARTING};
use alloc::string::String;
use core::fmt::Write;

const CLICK_TO_SET: &str = "CLICK TO SET";
const READ_ON: &str = "WHAT COMES, BY OPENAI";
const READ_OFF: &str = "OFF, ONLY SHOWN";
const SAID_AS_TEXT: &str = "AS TEXT, BY OPENAI";
const SAID_AS_VOICE: &str = "AS VOICE NOTES";
const TEXT_ALONE: &str = "PHOTOS, VOICE NOTES AS WORDS";
const NO_LIMIT: &str = "NONE";
const PIN_KEPT: &str = "KEPT ON THE DEVICE";
const PIN_TYPED: &str = "TYPED AT EACH START";
const DRIVE_SHOWN: &str = "SHOWS THE FILES TO A COMPUTER";
const TUNNEL_ALONE: &str = "ALL THROUGH THE TUNNEL";
const ONLINE: &str = "ONLINE";

// What the modem lent to a computer on the cable is at, in as many letters as a line of the narrowest theme holds beside its name.
const fn router_word(standing: Option<Standing>) -> &'static str {
    match standing {
        None => STARTING,
        // On, the router is off only while the modem takes no asks.
        Some(Standing::Off) => "MODEM NOT READY",
        Some(Standing::Absent) => "NO ADAPTER THIS BOOT",
        Some(Standing::Alone) => "NO COMPUTER ON THE CABLE",
        Some(Standing::Joining) => CONNECTING,
        Some(Standing::Lte) => ONLINE,
        Some(Standing::Tunnel) => "ONLINE, THROUGH THE TUNNEL",
        Some(Standing::Shut) => "TUNNEL DOWN, BLOCKED",
        Some(Standing::Unset) => "NO TUNNEL SET, BLOCKED",
    }
}

// The screens whose lists hold setting lines, by the destination that opens each; the tree holds the others.
const LISTED: [(Destination, &[Node]); 1] = [(Destination::Cost, &cost::SETTINGS)];

fn listed_on(setting: Setting) -> Option<Destination> {
    LISTED.into_iter().find(|(_, nodes)| holds(nodes, setting)).map(|(destination, _)| destination)
}

const fn tool_word(tool: Tool) -> &'static str {
    match tool {
        Tool::Web => "SEARCHES THE WEB, BY OPENAI",
        Tool::Device => "OPENS SCREENS, CHANGES SETTINGS",
        Tool::Notes => "READS NOTES AND TRANSLATIONS",
        Tool::News => "READS THE EDITIONS",
        Tool::Camera => "SEES THE PHOTOS YOU SHOW IT",
        Tool::Signal => "READS AND DRAFTS MESSAGES",
    }
}

// A language as it names itself.
const fn language_word(language: Language) -> &'static str {
    match language {
        Language::English => "ENGLISH",
        Language::German => "DEUTSCH",
        Language::French => "FRANÇAIS",
        Language::Spanish => "ESPAÑOL",
        Language::Italian => "ITALIANO",
    }
}

fn write_modem(status: Status, text: &mut String) -> core::fmt::Result {
    let word = match status {
        Status::Pin(tries) => return text.write_str("SIM PIN ").and_then(|()| write_tries(text, tries)),
        Status::Off => OFF,
        Status::Absent => "NO MODEM THIS BOOT",
        Status::Starting => STARTING,
        Status::NoSim => "NO SIM",
        Status::Puk => "SIM LOCKED, PUK NEEDED",
        Status::Locked => "SIM LOCKED",
        Status::Ready => "READY",
        Status::Attaching => CONNECTING,
        Status::Online => ONLINE,
        Status::Attached => "ATTACHED",
        Status::Failed => "FAILED, SWITCH IT OFF AND ON",
        Status::Ended => "FAILED UNTIL A REBOOT",
    };
    text.write_str(word)
}

fn push_capitals(text: &mut String, word: &str) {
    text.extend(word.chars().map(|character| character.to_ascii_uppercase()));
}

fn write_gap(text: &mut String, seconds: u16) -> core::fmt::Result {
    let seconds = u64::from(seconds);
    if seconds < SECONDS_PER_MINUTE {
        write!(text, "{seconds} S")
    } else {
        write!(text, "{} MIN", seconds / SECONDS_PER_MINUTE)
    }
}

impl Ui {
    // What the line of a setting shows: the value being set where it is staged, else the one set.
    fn shown_settings(&self, setting: Setting) -> Settings {
        let staged = self.staged.filter(|(staged, _)| *staged == setting);
        staged.map_or(self.settings, |(_, value)| self.settings.with_value(setting, value))
    }

    pub(super) fn write_setting(&self, setting: Setting, text: &mut String) {
        let settings = self.shown_settings(setting);
        let _ = match setting {
            Setting::Tool(tool) => text.write_str(if settings.tools.is_on(tool) { tool_word(tool) } else { OFF }),
            Setting::Model(task) => {
                push_capitals(text, settings.terms.thinking(task).model().to_word());
                Ok(())
            }
            Setting::Effort(task) => {
                push_capitals(text, settings.terms.thinking(task).effort().to_word());
                Ok(())
            }
            Setting::SignalWake => match settings.signal_wake {
                0 => text.write_str(OFF),
                minutes => write!(text, "EVERY {minutes} MIN"),
            },
            Setting::SignalAloud => text.write_str(if settings.is_signal_aloud { READ_ON } else { READ_OFF }),
            Setting::SaidVoice => text.write_str(if settings.is_said_voice { SAID_AS_VOICE } else { SAID_AS_TEXT }),
            Setting::TextOnly => text.write_str(if settings.is_text_only { TEXT_ALONE } else { OFF }),
            Setting::AutoReplies => write!(text, "AFTER {} REPLIES", settings.auto_replies),
            Setting::AutoGap => write_gap(text, settings.auto_gap).and_then(|()| text.write_str(" BETWEEN REPLIES")),
            Setting::AutoHourly => write!(text, "{} REPLIES AT THE MOST", settings.auto_hourly),
            Setting::Edition => match settings.edition.hour {
                Some(hour) => write!(text, "{hour:02}:00"),
                None => text.write_str(OFF),
            },
            Setting::Limit => match settings.terms.limit {
                Some(dollars) => write!(text, "${dollars} A MONTH"),
                None => text.write_str(NO_LIMIT),
            },
            Setting::PokedexLanguage => text.write_str(language_word(settings.pokedex)),
            Setting::Cellular => match (settings.is_cellular, self.modem) {
                (false, _) => text.write_str(OFF),
                (true, None | Some(Status::Off)) => text.write_str(STARTING),
                (true, Some(status)) => write_modem(status, text),
            },
            Setting::KeepPin => text.write_str(if settings.is_pin_kept { PIN_KEPT } else { PIN_TYPED }),
            Setting::Drive => text.write_str(if settings.is_drive_shown { DRIVE_SHOWN } else { OFF }),
            Setting::Modem => text.write_str(if settings.is_modem { router_word(self.router) } else { OFF }),
            Setting::Tunnel => text.write_str(if settings.uses_tunnel { TUNNEL_ALONE } else { OFF }),
        };
    }

    /// Takes the router's report; true if the screen changed.
    #[must_use]
    pub fn take_router_report(&mut self, report: &Report, now_ns: u64) -> bool {
        let is_standing_new = report.standing.is_some_and(|standing| self.router.replace(standing) != Some(standing));
        let is_config_new = report.has_config.is_some_and(|has_config| self.set_tunnel_kept(has_config));
        let is_kept =
            report.store.as_ref().is_some_and(|(asker, result)| self.note_config_kept(*asker, result, now_ns));
        if is_standing_new || is_config_new {
            self.deal_menu();
        }
        is_kept || (is_standing_new || is_config_new) && self.does_line_show(Content::tells_router)
    }

    /// Takes the modem's status; true if the screen changed.
    #[must_use]
    pub fn set_modem(&mut self, status: Status) -> bool {
        if self.modem.replace(status) == Some(status) {
            return false;
        }
        let mut word = String::new();
        let _ = write_modem(status, &mut word);
        let is_fact = self.set_fact(Part::Modem, Some(word));
        self.deal_menu();
        is_fact || self.does_line_show(Content::tells_modem)
    }

    // The setting of the chosen line, in the tree or in a screen's list.
    fn chosen_setting(&self) -> Option<Setting> {
        match self.screen {
            None => self.selected_content().and_then(Content::setting),
            Some(_) => {
                let list = self.visible_list()?;
                let grid = self.grid_of(list);
                let chosen = Some(grid.selection).filter(|at| grid.items.is_some_and(|count| *at < count))?;
                self.node_at(list, chosen)?.content.setting()
            }
        }
    }

    // What is staged, while its line is the one chosen.
    pub(in crate::ui) fn staged_shown(&self) -> Option<(Setting, Value)> {
        self.staged.filter(|(setting, _)| self.chosen_setting() == Some(*setting))
    }

    // The bar's status of every list while a setting is staged on its chosen line.
    pub(in crate::ui) fn staged_word(&self) -> Option<&'static str> {
        self.staged_shown().map(|_| CLICK_TO_SET)
    }

    // Where the staged setting's line stands among a list's first `count` lines.
    pub(in crate::ui) fn staged_line(&self, list: List, count: usize) -> Option<usize> {
        let (setting, _) = self.staged?;
        (0..count).position(|index| self.node_at(list, index).is_some_and(|node| node.content.sets(setting)))
    }

    // Deals anew the lines that show, the one a setting stands on among them.
    fn deal_shown(&mut self) {
        if let Some(list) = self.visible_list() {
            self.deal_lines(list, |_| true);
        }
    }

    // A click on the chosen line, wherever it stands: a setting's is set there, any other lets go of what is staged; true for a setting's.
    pub(super) fn click_setting(&mut self) -> bool {
        let chosen = self.chosen_setting();
        if self.staged.is_some_and(|(staged, _)| Some(staged) != chosen) {
            self.let_go_staged();
        }
        let Some(setting) = chosen else {
            return false;
        };
        self.set_here(setting);
        true
    }

    // A click on a setting's line: the line staged sets the value it shows, a switch turns, any other is staged with the value it has.
    fn set_here(&mut self, setting: Setting) {
        let next = match self.staged.take() {
            Some((staged, value)) if staged == setting => Some(self.settings.with_value(setting, value)),
            _ if setting.is_switch() => self.settings.stepped(setting, 1),
            _ => {
                self.staged = Some((setting, self.settings.value(setting)));
                None
            }
        };
        if let Some(next) = next {
            self.settings = next;
        }
        self.deal_shown();
    }

    // A change the conversation asked: made where it eases the device, else staged on its line, which the screens reach as a hand would.
    pub(super) fn change_setting(
        &mut self,
        setting: Setting,
        value: Value,
        is_held: bool,
        now_ns: u64,
    ) -> Option<Action> {
        let next = self.settings.with_value(setting, value);
        if self.settings.is_eased(setting, &next) {
            self.settings = next;
            self.deal_shown();
            return None;
        }
        if is_held {
            return None;
        }
        let listed = listed_on(setting);
        let target = move |content: Content| match listed {
            Some(destination) => content.runs(Use::Open(destination)),
            None => content.sets(setting),
        };
        let is_reached = self.route_to(&target).is_some();
        let action = self.reach(&target, listed.is_some(), now_ns);
        if is_reached {
            self.staged = Some((setting, next.value(setting)));
            self.choose_staged();
        }
        action
    }

    // Chooses the staged setting's line where the list that shows holds it; a list filled later chooses it in `show_lines`.
    fn choose_staged(&mut self) {
        let Some(list) = self.visible_list() else {
            return;
        };
        let count = self.grid_of(list).items.unwrap_or(0);
        if let Some(at) = self.staged_line(list, count) {
            self.grid_of_mut(list).choose(at);
        }
        self.deal_lines(list, |_| true);
    }

    pub(super) fn step_staged(&mut self, detents: i32) {
        let Some((setting, value)) = self.staged else {
            return;
        };
        match self.settings.with_value(setting, value).stepped(setting, -detents.signum()) {
            Some(next) => {
                self.staged = Some((setting, next.value(setting)));
                self.deal_shown();
            }
            None => self.swing_line(),
        }
    }

    pub(in crate::ui) fn let_go_staged(&mut self) {
        if self.staged.take().is_some() {
            self.deal_shown();
        }
    }

    pub(super) fn swing_line(&mut self) {
        if let Some(grid) = self.visible_grid_mut() {
            grid.swing(1.0);
        }
    }
}
