//! What the owner has set.

use super::news::Edition;
use super::pokedex::Language;
use crate::base::lines::{last_value, values};
use crate::base::math::Scale;
use crate::base::text::{OFF, ON, OR, parse_switch, switch_word, to_listing};
use crate::base::time::offset;
use crate::gfx::palette;
use crate::gfx::theme::{THEMES, Theme};
use crate::hw::sound::volume;
use crate::logs::Logging;
use crate::openai::account::Terms;
use crate::openai::models::{Effort, Model, Task, Thinking};
use alloc::format;
use alloc::string::String;
use core::fmt::Write;
use core::str::FromStr;

pub const FILE: &str = "settings";
pub(crate) const DIMMEST: u8 = 10;
pub(crate) const BRIGHTEST: u8 = 100;
pub(crate) const BRIGHTNESS_STEP: u8 = 10;
const DEFAULT_BRIGHTNESS: u8 = 40;
const DEFAULT_VOLUME: u8 = 70;
const DEFAULT_SOUNDS: u8 = 40;
const DEFAULT_OFFSET: i16 = 0;
const KEY_BRIGHTNESS: &str = "brightness";
const KEY_VOLUME: &str = "volume";
const KEY_SOUNDS: &str = "sounds";
const KEY_ROBOT: &str = "robot";
const KEY_OFFSET: &str = "offset";
pub(super) const KEY_LOGGING: &str = "logging";
const KEY_THEME: &str = "theme";
const KEY_COLOURS: &str = "colours";
const KEY_SIGNAL_WAKE: &str = "signal_wake";
const KEY_SIGNAL_ALOUD: &str = "signal_aloud";
const KEY_AUTO_REPLIES: &str = "auto_replies";
const KEY_AUTO_GAP: &str = "auto_gap";
const KEY_AUTO_HOURLY: &str = "auto_hourly";
const KEY_LIMIT: &str = "limit";
const KEY_MODEL: &str = "model";
const KEY_TOOL: &str = "tool";
const KEY_NEWS: &str = "news";
const KEY_SIGNAL_SAID: &str = "signal_said";
const KEY_SIGNAL_TEXT: &str = "signal_text";
const KEY_POKEDEX: &str = "pokedex";
const KEY_CELLULAR: &str = "cellular";
const KEY_KEEP_PIN: &str = "keep_pin";
const KEY_USB_DRIVE: &str = "usb_drive";
const KEY_USB_MODEM: &str = "usb_modem";
const KEY_VPN: &str = "vpn";
const NO_LIMIT: &str = "none";
const SAID_AS_TEXT: &str = "text";
const SAID_AS_VOICE: &str = "voice";
// The most OpenAI's services may cost a month, in dollars, that the owner may set; or none.
const LIMITS: [u16; 8] = [1, 2, 5, 10, 20, 25, 50, 100];
const DEFAULT_LIMIT: u16 = 25;
const HOURS_IN_A_DAY: u8 = 24;
const AUTO_REPLIES_ALLOWED: [u8; 5] = [5, 10, 20, 50, 100];
const AUTO_GAPS: [u16; 5] = [10, 30, 60, 120, 300];
const AUTO_HOURLIES: [u8; 4] = [10, 20, 30, 60];
const DEFAULT_AUTO_REPLIES: u8 = 20;
const DEFAULT_AUTO_GAP: u16 = 30;
const DEFAULT_AUTO_HOURLY: u8 = 30;
const SIGNAL_WAKES: [u8; 5] = [0, 5, 10, 15, 60];
const DEFAULT_SIGNAL_WAKE: u8 = 60;

/// What the owner lets the robot use, each switched on or off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Web,
    Device,
    Notes,
    News,
    Camera,
    Signal,
}

impl Tool {
    pub(crate) const ALL: [Tool; 6] = [Tool::Web, Tool::Device, Tool::Notes, Tool::News, Tool::Camera, Tool::Signal];

    // The word the settings' file keeps it by.
    pub(super) const fn to_word(self) -> &'static str {
        match self {
            Tool::Web => "web",
            Tool::Device => "device",
            Tool::Notes => "notes",
            Tool::News => "news",
            Tool::Camera => "camera",
            Tool::Signal => "signal",
        }
    }

    fn parse(word: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|tool| tool.to_word() == word)
    }
}

/// Which tools are on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tools([bool; Tool::ALL.len()]);

impl Default for Tools {
    // Signal's conversations are other people's words: the robot reads none until the owner says so.
    fn default() -> Tools {
        Tools(Tool::ALL.map(|tool| tool != Tool::Signal))
    }
}

impl Tools {
    pub const fn is_on(self, tool: Tool) -> bool {
        self.0[tool as usize]
    }

    pub(super) const fn with(mut self, tool: Tool, is_on: bool) -> Tools {
        self.0[tool as usize] = is_on;
        self
    }

    pub(crate) fn count_on(self) -> usize {
        self.0.iter().filter(|is_on| **is_on).count()
    }
}

/// What the owner has set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    /// The backlight, in percent of its range.
    pub brightness: u8,
    /// How loud everything the speaker plays is, in percent.
    pub volume: u8,
    /// How loud the interface's own sounds play, on the volume's scale.
    pub sounds: u8,
    pub has_robot: bool,
    /// How far the clock on screen is ahead of UTC, in minutes.
    pub offset: i16,
    pub logging: Logging,
    pub theme: Theme,
    /// The palette each theme is drawn in.
    pub colours: [usize; THEMES.len()],
    /// How often Signal's messages are fetched, in minutes, nought for never.
    pub signal_wake: u8,
    pub is_signal_aloud: bool,
    pub auto_replies: u8,
    pub auto_gap: u16,
    pub auto_hourly: u8,
    /// The model and effort of each use of the language model, and the most a month may cost.
    pub terms: Terms,
    pub tools: Tools,
    pub edition: Edition,
    /// True where a message said for Signal goes as a voice note, false where the speech model writes it down.
    pub is_said_voice: bool,
    /// True where Signal's conversations show text alone.
    pub is_text_only: bool,
    /// The language the Pokédex hears names in and writes its entries in.
    pub pokedex: Language,
    /// True where the modem runs and its data connection stands in where WiFi is not to be had.
    pub is_cellular: bool,
    /// True where a PIN the SIM takes is kept and handed to it at the modem's next starts.
    pub is_pin_kept: bool,
    /// True where a computer on the cable is shown the files as a drive.
    pub is_drive_shown: bool,
    /// True where a computer on the cable is lent the modem's connection.
    pub is_modem: bool,
    /// True where that computer's packets leave through the tunnel alone.
    pub uses_tunnel: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            brightness: DEFAULT_BRIGHTNESS,
            volume: DEFAULT_VOLUME,
            sounds: DEFAULT_SOUNDS,
            has_robot: true,
            offset: DEFAULT_OFFSET,
            logging: Logging::On,
            theme: Theme::STANDARD,
            colours: [palette::STANDARD; THEMES.len()],
            signal_wake: DEFAULT_SIGNAL_WAKE,
            is_signal_aloud: true,
            auto_replies: DEFAULT_AUTO_REPLIES,
            auto_gap: DEFAULT_AUTO_GAP,
            auto_hourly: DEFAULT_AUTO_HOURLY,
            terms: Terms { limit: Some(DEFAULT_LIMIT), ..Terms::default() },
            tools: Tools::default(),
            edition: Edition::default(),
            is_said_voice: false,
            is_text_only: false,
            pokedex: Language::default(),
            is_cellular: false,
            is_pin_kept: true,
            is_drive_shown: false,
            is_modem: false,
            uses_tunnel: false,
        }
    }
}

fn parse_number<T: FromStr>(word: &str) -> Option<T> {
    word.trim().parse().ok()
}

fn parse_limit(word: &str) -> Option<Option<u16>> {
    match word.trim() {
        NO_LIMIT => Some(None),
        word => parse_allowed(word, &LIMITS).map(Some),
    }
}

fn limit_word(limit: Option<u16>) -> String {
    limit.map_or_else(|| NO_LIMIT.into(), |dollars| format!("{dollars}"))
}

fn parse_hour(word: &str) -> Option<Option<u8>> {
    match word.trim() {
        OFF => Some(None),
        word => parse_number(word).filter(|hour| *hour < HOURS_IN_A_DAY).map(Some),
    }
}

fn hour_word(hour: Option<impl core::fmt::Display>) -> String {
    hour.map_or_else(|| OFF.into(), |hour| format!("{hour}"))
}

fn parse_said(word: &str) -> Option<bool> {
    match word.trim() {
        SAID_AS_TEXT => Some(false),
        SAID_AS_VOICE => Some(true),
        _ => None,
    }
}

const fn said_word(is_voice: bool) -> &'static str {
    if is_voice { SAID_AS_VOICE } else { SAID_AS_TEXT }
}

fn parse_model(line: &str) -> Option<(Task, Thinking)> {
    let mut words = line.split_whitespace();
    let task = Task::parse(words.next()?)?;
    let thinking = Thinking::new(Model::parse(words.next()?)?, Effort::parse(words.next()?)?);
    words.next().is_none().then_some((task, thinking))
}

fn parse_named_switch<T>(line: &str, parse_name: impl Fn(&str) -> Option<T>) -> Option<(T, bool)> {
    let (name, switch) = line.trim().split_once(' ')?;
    Some((parse_name(name)?, parse_switch(switch.trim())?))
}

fn parse_allowed<T: FromStr + PartialEq>(word: &str, allowed: &[T]) -> Option<T> {
    parse_number(word).filter(|value| allowed.contains(value))
}

fn parse_colour(line: &str) -> Option<(Theme, usize)> {
    let (theme, letter) = line.trim().rsplit_once(' ')?;
    Some((Theme::parse(theme)?, palette::parse_letter(letter.trim())?))
}

fn merge_field<T: PartialEq>(current: T, default: T, stored: T) -> T {
    if current == default { stored } else { current }
}

pub(crate) const BRIGHTNESS_SCALE: Scale = Scale { min: DIMMEST, max: BRIGHTEST, step: BRIGHTNESS_STEP };

/// A setting set where its line stands, in a menu or a screen's list: a switch turns at a click, any other steps through its values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    Tool(Tool),
    Model(Task),
    Effort(Task),
    SignalWake,
    SignalAloud,
    SaidVoice,
    TextOnly,
    AutoReplies,
    AutoGap,
    AutoHourly,
    Edition,
    Limit,
    PokedexLanguage,
    Cellular,
    KeepPin,
    Drive,
    Modem,
    Tunnel,
}

impl Setting {
    // Every setting set where it stands, in the order the robot is told them.
    pub(super) const ALL: [Setting; 37] = [
        Setting::Edition,
        Setting::SignalAloud,
        Setting::TextOnly,
        Setting::Tool(Tool::Web),
        Setting::Tool(Tool::Device),
        Setting::Tool(Tool::Notes),
        Setting::Tool(Tool::News),
        Setting::Tool(Tool::Camera),
        Setting::Tool(Tool::Signal),
        Setting::Limit,
        Setting::SignalWake,
        Setting::SaidVoice,
        Setting::AutoReplies,
        Setting::AutoGap,
        Setting::AutoHourly,
        Setting::Model(Task::Chat),
        Setting::Model(Task::Recollect),
        Setting::Model(Task::Summary),
        Setting::Model(Task::News),
        Setting::Model(Task::Auto),
        Setting::Model(Task::Web),
        Setting::Model(Task::Bell),
        Setting::Model(Task::Rundowns),
        Setting::Effort(Task::Chat),
        Setting::Effort(Task::Recollect),
        Setting::Effort(Task::Summary),
        Setting::Effort(Task::News),
        Setting::Effort(Task::Auto),
        Setting::Effort(Task::Web),
        Setting::Effort(Task::Bell),
        Setting::Effort(Task::Rundowns),
        Setting::PokedexLanguage,
        Setting::Cellular,
        Setting::KeepPin,
        Setting::Drive,
        Setting::Modem,
        Setting::Tunnel,
    ];

    pub(crate) fn is_switch(self) -> bool {
        matches!(Settings::default().value(self), Value::Switch(_))
    }

    // The name the robot changes it by.
    pub(super) const fn to_name(self) -> &'static str {
        match self {
            Setting::Edition => "daily_news",
            Setting::SignalAloud => "signal_read_aloud",
            Setting::TextOnly => "signal_text_only",
            Setting::Tool(Tool::Web) => "tool_web",
            Setting::Tool(Tool::Device) => "tool_device",
            Setting::Tool(Tool::Notes) => "tool_notes",
            Setting::Tool(Tool::News) => "tool_news",
            Setting::Tool(Tool::Camera) => "tool_camera",
            Setting::Tool(Tool::Signal) => "tool_signal",
            Setting::Limit => "spending_limit",
            Setting::SignalWake => "signal_fetch",
            Setting::SaidVoice => "signal_said",
            Setting::AutoReplies => "auto_replies",
            Setting::AutoGap => "auto_gap",
            Setting::AutoHourly => "auto_hourly",
            Setting::Model(Task::Chat) => "robot_model",
            Setting::Model(Task::Recollect) => "recollection_model",
            Setting::Model(Task::Summary) => "summarize_model",
            Setting::Model(Task::News) => "news_model",
            Setting::Model(Task::Auto) => "auto_model",
            Setting::Model(Task::Web) => "web_model",
            Setting::Model(Task::Bell) => "bell_model",
            Setting::Model(Task::Rundowns) => "rundowns_model",
            Setting::Effort(Task::Chat) => "robot_reasoning",
            Setting::Effort(Task::Recollect) => "recollection_reasoning",
            Setting::Effort(Task::Summary) => "summarize_reasoning",
            Setting::Effort(Task::News) => "news_reasoning",
            Setting::Effort(Task::Auto) => "auto_reasoning",
            Setting::Effort(Task::Web) => "web_reasoning",
            Setting::Effort(Task::Bell) => "bell_reasoning",
            Setting::Effort(Task::Rundowns) => "rundowns_reasoning",
            Setting::PokedexLanguage => "pokedex_language",
            Setting::Cellular => "mobile_data",
            Setting::KeepPin => "keep_sim_pin",
            Setting::Drive => "usb_drive",
            Setting::Modem => "usb_modem",
            Setting::Tunnel => "vpn",
        }
    }

    pub(super) fn parse_name(name: &str) -> Option<Setting> {
        Setting::ALL.into_iter().find(|setting| setting.to_name() == name)
    }

    // The value the robot's word for it names, where that is one of its steps.
    pub(super) fn parse_value(self, word: &str) -> Option<Value> {
        let word = word.trim();
        let small = |steps: &[u8]| parse_allowed(word, steps).map(u16::from);
        Some(match self {
            Setting::SaidVoice => Value::Switch(parse_said(word)?),
            Setting::Tool(_)
            | Setting::SignalAloud
            | Setting::TextOnly
            | Setting::Cellular
            | Setting::KeepPin
            | Setting::Drive
            | Setting::Modem
            | Setting::Tunnel => Value::Switch(parse_switch(word)?),
            Setting::Model(_) => Value::Model(Model::parse(word)?),
            Setting::Effort(_) => Value::Effort(Effort::parse(word)?),
            Setting::Edition => Value::Step(parse_hour(word)?.map(u16::from)),
            Setting::Limit => Value::Step(parse_limit(word)?),
            Setting::SignalWake => Value::Step(Some(small(&SIGNAL_WAKES)?)),
            Setting::AutoReplies => Value::Step(Some(small(&AUTO_REPLIES_ALLOWED)?)),
            Setting::AutoGap => Value::Step(Some(parse_allowed(word, &AUTO_GAPS)?)),
            Setting::AutoHourly => Value::Step(Some(small(&AUTO_HOURLIES)?)),
            Setting::PokedexLanguage => Value::Language(Language::parse(word)?),
        })
    }
}

// What a model is told of a change of a setting: which changes wait for the owner's click, as `Settings::is_eased` holds them, and the words `Setting::parse_value` reads each value by.
pub(super) fn to_change_description() -> String {
    format!(
        "Change one of the device's settings. A change takes effect once your reply is over, but for one that leaves the device less safe or less private, a tool switched on, signal_read_aloud on, signal_said {SAID_AS_TEXT}, keep_sim_pin on, usb_drive on, usb_modem on or vpn off, which waits on its line for the user's click: the answer tells which, and of one that waits say so, and never that it is set. value is {ON} or {OFF} for a switch; for daily_news an hour 0 to {} of the device's clock, or {OFF}; for spending_limit {} dollars a month, or {NO_LIMIT}; for signal_fetch {} minutes, 0 for never; for signal_said {SAID_AS_TEXT} or {SAID_AS_VOICE}; for auto_replies {}; for auto_gap {} seconds; for auto_hourly {}; for a model {}, the cheapest first; for a reasoning {}; for pokedex_language {}.",
        HOURS_IN_A_DAY - 1,
        to_listing(LIMITS, OR),
        to_listing(SIGNAL_WAKES, OR),
        to_listing(AUTO_REPLIES_ALLOWED, OR),
        to_listing(AUTO_GAPS, OR),
        to_listing(AUTO_HOURLIES, OR),
        to_listing(Model::ALL.map(Model::to_word), OR),
        to_listing(Effort::ALL.map(Effort::to_word), OR),
        to_listing(Language::ALL.map(Language::code), OR),
    )
}

/// A value the robot asks a setting to take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Value {
    Switch(bool),
    // An hour, minutes, dollars or replies; none for off or no limit.
    Step(Option<u16>),
    Model(Model),
    Effort(Effort),
    Language(Language),
}

fn step_among<T: Copy + PartialEq>(steps: &[T], current: T, way: i32) -> Option<T> {
    let at = steps.iter().position(|step| *step == current)?;
    steps.get(at.checked_add_signed(way.signum() as isize)?).copied()
}

fn step_index(at: usize, count: usize, way: i32) -> Option<usize> {
    at.checked_add_signed(way.signum() as isize).filter(|next| *next < count)
}

/// The level of a backlight whose brightest is `max`, rounded up so that the dimmest still lights the panel.
pub fn backlight_level(max: u32, percent: u8) -> u32 {
    (max * u32::from(percent.min(BRIGHTEST))).div_ceil(u32::from(BRIGHTEST))
}

impl Settings {
    /// The palette the set theme is drawn in.
    pub const fn palette(&self) -> usize {
        self.colours[self.theme as usize]
    }

    pub(crate) const fn with_palette(mut self, palette: usize) -> Settings {
        self.colours[self.theme as usize] = palette;
        self
    }

    pub(crate) fn step_brightness(self, steps: i32) -> Settings {
        Settings { brightness: BRIGHTNESS_SCALE.step_from(self.brightness, steps), ..self }
    }

    pub(crate) fn step_volume(self, steps: i32) -> Settings {
        Settings { volume: volume::SCALE.step_from(self.volume, steps), ..self }
    }

    pub(crate) fn step_sounds(self, steps: i32) -> Settings {
        Settings { sounds: volume::SCALE.step_from(self.sounds, steps), ..self }
    }

    pub(crate) fn step_offset(self, steps: i32) -> Settings {
        Settings { offset: offset::step(self.offset, steps), ..self }
    }

    // The settings with `setting` at `value`, unchanged where the value is not of the setting's kind; an effort the model does not take is the least it takes above it.
    pub(crate) fn with_value(self, setting: Setting, value: Value) -> Settings {
        let mut next = self;
        match (setting, value) {
            (Setting::Tool(tool), Value::Switch(is_on)) => next.tools = self.tools.with(tool, is_on),
            (Setting::SignalAloud, Value::Switch(is_on)) => next.is_signal_aloud = is_on,
            (Setting::SaidVoice, Value::Switch(is_on)) => next.is_said_voice = is_on,
            (Setting::TextOnly, Value::Switch(is_on)) => next.is_text_only = is_on,
            (Setting::Model(task), Value::Model(model)) => {
                next.terms.models[task as usize] = Thinking::new(model, self.terms.thinking(task).effort());
            }
            (Setting::Effort(task), Value::Effort(effort)) => {
                next.terms.models[task as usize] = Thinking::new(self.terms.thinking(task).model(), effort);
            }
            (Setting::Edition, Value::Step(hour)) => next.edition.hour = hour.and_then(|hour| u8::try_from(hour).ok()),
            (Setting::Limit, Value::Step(dollars)) => next.terms.limit = dollars,
            (Setting::SignalWake, Value::Step(Some(minutes))) => {
                next.signal_wake = u8::try_from(minutes).unwrap_or(self.signal_wake);
            }
            (Setting::AutoReplies, Value::Step(Some(replies))) => {
                next.auto_replies = u8::try_from(replies).unwrap_or(self.auto_replies);
            }
            (Setting::AutoGap, Value::Step(Some(seconds))) => next.auto_gap = seconds,
            (Setting::AutoHourly, Value::Step(Some(replies))) => {
                next.auto_hourly = u8::try_from(replies).unwrap_or(self.auto_hourly);
            }
            (Setting::PokedexLanguage, Value::Language(language)) => next.pokedex = language,
            (Setting::Cellular, Value::Switch(is_on)) => next.is_cellular = is_on,
            (Setting::KeepPin, Value::Switch(is_on)) => next.is_pin_kept = is_on,
            (Setting::Drive, Value::Switch(is_on)) => next.is_drive_shown = is_on,
            (Setting::Modem, Value::Switch(is_on)) => next.is_modem = is_on,
            (Setting::Tunnel, Value::Switch(is_on)) => next.uses_tunnel = is_on,
            (
                Setting::Tool(_)
                | Setting::SignalAloud
                | Setting::SaidVoice
                | Setting::TextOnly
                | Setting::Model(_)
                | Setting::Effort(_)
                | Setting::Edition
                | Setting::Limit
                | Setting::SignalWake
                | Setting::AutoReplies
                | Setting::AutoGap
                | Setting::AutoHourly
                | Setting::PokedexLanguage
                | Setting::Cellular
                | Setting::KeepPin
                | Setting::Drive
                | Setting::Modem
                | Setting::Tunnel,
                _,
            ) => {}
        }
        next
    }

    pub(crate) fn value(&self, setting: Setting) -> Value {
        match setting {
            Setting::Tool(tool) => Value::Switch(self.tools.is_on(tool)),
            Setting::SignalAloud => Value::Switch(self.is_signal_aloud),
            Setting::SaidVoice => Value::Switch(self.is_said_voice),
            Setting::TextOnly => Value::Switch(self.is_text_only),
            Setting::Model(task) => Value::Model(self.terms.thinking(task).model()),
            Setting::Effort(task) => Value::Effort(self.terms.thinking(task).effort()),
            Setting::Edition => Value::Step(self.edition.hour.map(u16::from)),
            Setting::Limit => Value::Step(self.terms.limit),
            Setting::SignalWake => Value::Step(Some(u16::from(self.signal_wake))),
            Setting::AutoReplies => Value::Step(Some(u16::from(self.auto_replies))),
            Setting::AutoGap => Value::Step(Some(self.auto_gap)),
            Setting::AutoHourly => Value::Step(Some(u16::from(self.auto_hourly))),
            Setting::PokedexLanguage => Value::Language(self.pokedex),
            Setting::Cellular => Value::Switch(self.is_cellular),
            Setting::KeepPin => Value::Switch(self.is_pin_kept),
            Setting::Drive => Value::Switch(self.is_drive_shown),
            Setting::Modem => Value::Switch(self.is_modem),
            Setting::Tunnel => Value::Switch(self.uses_tunnel),
        }
    }

    /// True while a computer on the cable is lent the modem's connection: both switches on and a modem that takes asks, so that one that serves nothing leaves the device its own ways into a network.
    pub const fn lends_modem(&self, takes_asks: bool) -> bool {
        self.is_modem && self.is_cellular && takes_asks
    }

    // True where `next`, which differs from these settings in `setting` alone, leaves the device as safe and as private as it is, or more; what a change costs is the owner's to say.
    pub(crate) fn is_eased(&self, setting: Setting, next: &Settings) -> bool {
        match setting {
            // A tool switched on lets the robot read or do more.
            Setting::Tool(tool) => !next.tools.is_on(tool) || self.tools.is_on(tool),
            // Read aloud, and written down where they are said, a conversation's words go to OpenAI.
            Setting::SignalAloud => !next.is_signal_aloud || self.is_signal_aloud,
            Setting::SaidVoice => next.is_said_voice || !self.is_said_voice,
            // A PIN no longer kept is a secret fewer on the device.
            Setting::KeepPin => !next.is_pin_kept || self.is_pin_kept,
            // A drive shown hands every file to whatever computer the cable leads to.
            Setting::Drive => !next.is_drive_shown || self.is_drive_shown,
            // A modem for whatever computer the cable leads to: what that sends leaves as the SIM's owner's.
            Setting::Modem => !next.is_modem || self.is_modem,
            // Without the tunnel that computer's packets leave in the open.
            Setting::Tunnel => next.uses_tunnel || !self.uses_tunnel,
            Setting::TextOnly
            | Setting::Edition
            | Setting::Limit
            | Setting::SignalWake
            | Setting::AutoReplies
            | Setting::AutoGap
            | Setting::AutoHourly
            | Setting::Model(_)
            | Setting::Effort(_)
            | Setting::PokedexLanguage
            | Setting::Cellular => true,
        }
    }

    fn value_word(&self, setting: Setting) -> String {
        match (setting, self.value(setting)) {
            (Setting::SaidVoice, Value::Switch(is_voice)) => String::from(said_word(is_voice)),
            (Setting::Edition, Value::Step(hour)) => hour_word(hour),
            (Setting::Limit, Value::Step(dollars)) => limit_word(dollars),
            (_, Value::Switch(is_on)) => String::from(switch_word(is_on)),
            (_, Value::Step(step)) => step.map_or_else(String::new, |step| format!("{step}")),
            (_, Value::Model(model)) => String::from(model.to_word()),
            (_, Value::Effort(effort)) => String::from(effort.to_word()),
            (_, Value::Language(language)) => String::from(language.code()),
        }
    }

    /// What the robot is told of the settings: a line each, by the names it changes them by.
    pub fn to_readout(&self) -> String {
        let mut text = format!(
            "- brightness: {}%\n- volume: {}%\n- sounds: {}%\n- theme: {}\n- colors: {}\n",
            self.brightness,
            self.volume,
            self.sounds,
            self.theme.to_word(),
            palette::letter(self.palette())
        );
        for setting in Setting::ALL {
            let _ = writeln!(text, "- {}: {}", setting.to_name(), self.value_word(setting));
        }
        text
    }

    // The settings with `setting` turned where it is a switch, else one step on, `way` up or down; none past either end.
    pub(crate) fn stepped(self, setting: Setting, way: i32) -> Option<Settings> {
        let mut next = self;
        match setting {
            Setting::Tool(_)
            | Setting::SignalAloud
            | Setting::SaidVoice
            | Setting::TextOnly
            | Setting::Cellular
            | Setting::KeepPin
            | Setting::Drive
            | Setting::Modem
            | Setting::Tunnel => {
                if let Value::Switch(is_on) = self.value(setting) {
                    next = self.with_value(setting, Value::Switch(!is_on));
                }
            }
            Setting::Model(task) => {
                let thinking = self.terms.thinking(task);
                let model = step_among(&Model::ALL, thinking.model(), way)?;
                next.terms.models[task as usize] = Thinking::new(model, thinking.effort());
            }
            Setting::Effort(task) => {
                let thinking = self.terms.thinking(task);
                let efforts: alloc::vec::Vec<Effort> = thinking.efforts().collect();
                let effort = step_among(&efforts, thinking.effort(), way)?;
                next.terms.models[task as usize] = Thinking::new(thinking.model(), effort);
            }
            Setting::SignalWake => next.signal_wake = step_among(&SIGNAL_WAKES, self.signal_wake, way)?,
            Setting::AutoReplies => next.auto_replies = step_among(&AUTO_REPLIES_ALLOWED, self.auto_replies, way)?,
            Setting::AutoGap => next.auto_gap = step_among(&AUTO_GAPS, self.auto_gap, way)?,
            Setting::AutoHourly => next.auto_hourly = step_among(&AUTO_HOURLIES, self.auto_hourly, way)?,
            Setting::PokedexLanguage => next.pokedex = step_among(&Language::ALL, self.pokedex, way)?,
            Setting::Edition => {
                let at = self.edition.hour.map_or(0, |hour| usize::from(hour) + 1);
                let next_at = step_index(at, usize::from(HOURS_IN_A_DAY) + 1, way)?;
                next.edition.hour = next_at.checked_sub(1).map(|hour| hour as u8);
            }
            Setting::Limit => {
                let at = self.terms.limit.and_then(|dollars| LIMITS.iter().position(|step| *step == dollars));
                let next_at = step_index(at.unwrap_or(LIMITS.len()), LIMITS.len() + 1, way)?;
                next.terms.limit = LIMITS.get(next_at).copied();
            }
        }
        Some(next)
    }

    pub(crate) fn merge_stored(self, stored: Settings) -> Settings {
        let default = Settings::default();
        Settings {
            brightness: merge_field(self.brightness, default.brightness, stored.brightness),
            volume: merge_field(self.volume, default.volume, stored.volume),
            sounds: merge_field(self.sounds, default.sounds, stored.sounds),
            has_robot: merge_field(self.has_robot, default.has_robot, stored.has_robot),
            offset: merge_field(self.offset, default.offset, stored.offset),
            logging: merge_field(self.logging, default.logging, stored.logging),
            theme: merge_field(self.theme, default.theme, stored.theme),
            colours: core::array::from_fn(|at| merge_field(self.colours[at], default.colours[at], stored.colours[at])),
            signal_wake: merge_field(self.signal_wake, default.signal_wake, stored.signal_wake),
            is_signal_aloud: merge_field(self.is_signal_aloud, default.is_signal_aloud, stored.is_signal_aloud),
            auto_replies: merge_field(self.auto_replies, default.auto_replies, stored.auto_replies),
            auto_gap: merge_field(self.auto_gap, default.auto_gap, stored.auto_gap),
            auto_hourly: merge_field(self.auto_hourly, default.auto_hourly, stored.auto_hourly),
            // Each use's model, the limit and each tool by itself, as each palette: one set in the session leaves the others as kept.
            terms: Terms {
                models: core::array::from_fn(|at| {
                    merge_field(self.terms.models[at], default.terms.models[at], stored.terms.models[at])
                }),
                limit: merge_field(self.terms.limit, default.terms.limit, stored.terms.limit),
            },
            tools: Tools(core::array::from_fn(|at| {
                merge_field(self.tools.0[at], default.tools.0[at], stored.tools.0[at])
            })),
            edition: merge_field(self.edition, default.edition, stored.edition),
            is_said_voice: merge_field(self.is_said_voice, default.is_said_voice, stored.is_said_voice),
            is_text_only: merge_field(self.is_text_only, default.is_text_only, stored.is_text_only),
            pokedex: merge_field(self.pokedex, default.pokedex, stored.pokedex),
            is_cellular: merge_field(self.is_cellular, default.is_cellular, stored.is_cellular),
            is_pin_kept: merge_field(self.is_pin_kept, default.is_pin_kept, stored.is_pin_kept),
            is_drive_shown: merge_field(self.is_drive_shown, default.is_drive_shown, stored.is_drive_shown),
            is_modem: merge_field(self.is_modem, default.is_modem, stored.is_modem),
            uses_tunnel: merge_field(self.uses_tunnel, default.uses_tunnel, stored.uses_tunnel),
        }
    }

    /// The settings a stored text holds; the last line that says something known for a key counts.
    pub fn parse(text: &str) -> Settings {
        let default = Settings::default();
        let mut colours = default.colours;
        for (theme, palette) in values(text, KEY_COLOURS).filter_map(parse_colour) {
            colours[theme as usize] = palette;
        }
        let mut terms = default.terms;
        for (task, thinking) in values(text, KEY_MODEL).filter_map(parse_model) {
            terms.models[task as usize] = thinking;
        }
        terms.limit = last_value(text, KEY_LIMIT, parse_limit).unwrap_or(terms.limit);
        let tools = values(text, KEY_TOOL)
            .filter_map(|line| parse_named_switch(line, Tool::parse))
            .fold(default.tools, |tools, (tool, is_on)| tools.with(tool, is_on));
        let edition = Edition { hour: last_value(text, KEY_NEWS, parse_hour).unwrap_or(default.edition.hour) };
        let number = parse_number;
        let switch = |word: &str| parse_switch(word.trim());
        let brightness = |percent| BRIGHTNESS_SCALE.nearest(percent);
        let loudness = |percent| volume::SCALE.nearest(percent);
        Settings {
            brightness: last_value(text, KEY_BRIGHTNESS, number).map_or(default.brightness, brightness),
            volume: last_value(text, KEY_VOLUME, number).map_or(default.volume, loudness),
            sounds: last_value(text, KEY_SOUNDS, number).map_or(default.sounds, loudness),
            has_robot: last_value(text, KEY_ROBOT, switch).unwrap_or(default.has_robot),
            offset: last_value(text, KEY_OFFSET, |word| offset::checked(parse_number(word)?)).unwrap_or(default.offset),
            logging: last_value(text, KEY_LOGGING, Logging::parse).unwrap_or(default.logging),
            theme: last_value(text, KEY_THEME, Theme::parse).unwrap_or(default.theme),
            colours,
            signal_wake: last_value(text, KEY_SIGNAL_WAKE, |word| parse_allowed(word, &SIGNAL_WAKES))
                .unwrap_or(default.signal_wake),
            is_signal_aloud: last_value(text, KEY_SIGNAL_ALOUD, switch).unwrap_or(default.is_signal_aloud),
            auto_replies: last_value(text, KEY_AUTO_REPLIES, |word| parse_allowed(word, &AUTO_REPLIES_ALLOWED))
                .unwrap_or(default.auto_replies),
            auto_gap: last_value(text, KEY_AUTO_GAP, |word| parse_allowed(word, &AUTO_GAPS))
                .unwrap_or(default.auto_gap),
            auto_hourly: last_value(text, KEY_AUTO_HOURLY, |word| parse_allowed(word, &AUTO_HOURLIES))
                .unwrap_or(default.auto_hourly),
            terms,
            tools,
            edition,
            is_said_voice: last_value(text, KEY_SIGNAL_SAID, parse_said).unwrap_or(default.is_said_voice),
            is_text_only: last_value(text, KEY_SIGNAL_TEXT, switch).unwrap_or(default.is_text_only),
            pokedex: last_value(text, KEY_POKEDEX, |word| Language::parse(word.trim())).unwrap_or(default.pokedex),
            is_cellular: last_value(text, KEY_CELLULAR, switch).unwrap_or(default.is_cellular),
            is_pin_kept: last_value(text, KEY_KEEP_PIN, switch).unwrap_or(default.is_pin_kept),
            is_drive_shown: last_value(text, KEY_USB_DRIVE, switch).unwrap_or(default.is_drive_shown),
            is_modem: last_value(text, KEY_USB_MODEM, switch).unwrap_or(default.is_modem),
            uses_tunnel: last_value(text, KEY_VPN, switch).unwrap_or(default.uses_tunnel),
        }
    }

    /// The text of the settings' file.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        let mut line = |key: &str, value: &dyn core::fmt::Display| {
            let _ = writeln!(text, "{key} {value}");
        };
        line(KEY_BRIGHTNESS, &self.brightness);
        line(KEY_VOLUME, &self.volume);
        line(KEY_SOUNDS, &self.sounds);
        line(KEY_ROBOT, &switch_word(self.has_robot));
        line(KEY_OFFSET, &self.offset);
        line(KEY_LOGGING, &self.logging.to_word());
        line(KEY_THEME, &self.theme.to_word());
        line(KEY_SIGNAL_WAKE, &self.signal_wake);
        line(KEY_SIGNAL_ALOUD, &switch_word(self.is_signal_aloud));
        line(KEY_AUTO_REPLIES, &self.auto_replies);
        line(KEY_AUTO_GAP, &self.auto_gap);
        line(KEY_AUTO_HOURLY, &self.auto_hourly);
        for theme in THEMES {
            line(KEY_COLOURS, &format!("{} {}", theme.to_word(), palette::letter(self.colours[theme as usize])));
        }
        line(KEY_LIMIT, &limit_word(self.terms.limit));
        for task in Task::ALL {
            let thinking = self.terms.thinking(task);
            line(
                KEY_MODEL,
                &format!("{} {} {}", task.to_word(), thinking.model().to_word(), thinking.effort().to_word()),
            );
        }
        for tool in Tool::ALL {
            line(KEY_TOOL, &format!("{} {}", tool.to_word(), switch_word(self.tools.is_on(tool))));
        }
        line(KEY_NEWS, &hour_word(self.edition.hour));
        line(KEY_SIGNAL_SAID, &said_word(self.is_said_voice));
        line(KEY_SIGNAL_TEXT, &switch_word(self.is_text_only));
        line(KEY_POKEDEX, &self.pokedex.code());
        line(KEY_CELLULAR, &switch_word(self.is_cellular));
        line(KEY_KEEP_PIN, &switch_word(self.is_pin_kept));
        line(KEY_USB_DRIVE, &switch_word(self.is_drive_shown));
        line(KEY_USB_MODEM, &switch_word(self.is_modem));
        line(KEY_VPN, &switch_word(self.uses_tunnel));
        text
    }

    // The settings in one line, for the log.
    pub(super) fn to_log_line(self) -> String {
        self.to_text().trim_end().replace('\n', ", ")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::hw::sound::volume::{LOUDEST, QUIETEST};
    use alloc::vec::Vec;

    // The lines of the settings the file of an earlier firmware lacks, as a new device has them.
    pub(crate) const DEFAULT_TAIL: &str = "colours rail C\nlimit 25\nmodel robot gpt-6.1-terra high\nmodel recollection gpt-6.1-terra high\nmodel notes gpt-6.1-terra high\nmodel news gpt-6.1-terra high\nmodel auto gpt-6.1-terra high\nmodel web gpt-6.1-terra high\nmodel bell gpt-6.1-terra high\nmodel rundowns gpt-6.1-terra high\ntool web on\ntool device on\ntool notes on\ntool news on\ntool camera on\ntool signal off\nnews 8\nsignal_said text\nsignal_text off\npokedex en\ncellular off\nkeep_pin on\nusb_drive off\nusb_modem off\nvpn off\n";

    fn nearest_brightness(percent: u32) -> u8 {
        BRIGHTNESS_SCALE.nearest(percent)
    }

    fn nearest_volume(percent: u32) -> u8 {
        volume::SCALE.nearest(percent)
    }

    #[test]
    fn text_round_trip() {
        for settings in [
            Settings::default(),
            Settings {
                brightness: 10,
                volume: 0,
                sounds: 0,
                has_robot: false,
                offset: -210,
                logging: Logging::Off,
                theme: Theme::Stage,
                colours: [0, 1, 2, 3, 4, 5],
                signal_wake: 0,
                is_signal_aloud: false,
                auto_replies: 5,
                auto_gap: 300,
                auto_hourly: 60,
                terms: Terms { models: [Thinking::new(Model::Astra, Effort::Max); Task::ALL.len()], limit: None },
                tools: Tools::default().with(Tool::Web, false).with(Tool::Signal, true),
                edition: Edition { hour: None },
                is_said_voice: true,
                is_text_only: true,
                pokedex: Language::Italian,
                is_cellular: true,
                is_pin_kept: false,
                is_drive_shown: true,
                is_modem: true,
                uses_tunnel: true,
            },
            Settings {
                brightness: 100,
                volume: 30,
                offset: 840,
                logging: Logging::Debug,
                theme: Theme::Rail,
                colours: [9; 6],
                signal_wake: 15,
                edition: Edition { hour: Some(0) },
                terms: Terms { limit: Some(100), ..Settings::default().terms },
                ..Settings::default()
            },
        ] {
            assert_eq!(Settings::parse(&settings.to_text()), settings);
        }
        assert_eq!(
            Settings::default().to_text(),
            alloc::format!(
                "brightness 40\nvolume 70\nsounds 40\nrobot on\noffset 0\nlogging on\ntheme dial\nsignal_wake 60\nsignal_aloud on\nauto_replies 20\nauto_gap 30\nauto_hourly 30\ncolours stage C\ncolours dial C\ncolours deck C\ncolours terminal C\ncolours companion C\n{DEFAULT_TAIL}"
            )
        );
        let themed = Settings { theme: Theme::Dial, ..Settings::default() }.with_palette(5);
        assert_eq!(
            (themed.palette(), themed.colours, Settings { theme: Theme::Deck, ..themed }.palette()),
            (5, [2, 5, 2, 2, 2, 2], 2)
        );
        assert_eq!(
            Settings::parse("theme companion deck\ncolours companion deck F\ncolours companion D\n"),
            Settings { colours: [2, 2, 2, 2, 3, 2], ..Settings::default() }
        );
    }

    #[test]
    fn missing_keys_default() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(Settings::parse("robot off\n"), Settings { has_robot: false, ..Settings::default() });
        assert_eq!(
            Settings::parse(
                "contrast 3\nbrightness bright\nrobot maybe\nbrightness\noffset east\noffset\nlogging loud\nlogging\n"
            ),
            Settings::default()
        );
        assert_eq!(
            Settings::parse("volume 30\nvolume 50\nvolume loud\nvolume -10\nvolume\n"),
            Settings { volume: 50, ..Settings::default() }
        );
        assert_eq!(
            Settings::parse(
                "brightness 70\r\nvolume 40\r\nrobot off\r\noffset 120\r\nlogging off\r\ntheme dial\r\ncolours dial F\r\n"
            ),
            Settings {
                brightness: 70,
                volume: 40,
                has_robot: false,
                offset: 120,
                logging: Logging::Off,
                theme: Theme::Dial,
                colours: [2, 5, 2, 2, 2, 2],
                ..Settings::default()
            }
        );
        assert_eq!(
            Settings::parse(
                "colours stage P\ncolours stage Q\ncolours stage c\ncolours stage\ncolours stage AB\ncolours nothing C\n"
            )
            .colours,
            [15, 2, 2, 2, 2, 2]
        );
        assert_eq!(
            (Settings::parse("theme C\n").theme, Settings::parse("theme terminal\ntheme cube\n").theme),
            (Theme::STANDARD, Theme::Terminal)
        );
        assert_eq!(Settings::parse("logging debug\n").logging, Logging::Debug);
        assert_eq!(
            ["signal_wake 15\n", "signal_wake 7\n", "signal_wake off\n"].map(|text| Settings::parse(text).signal_wake),
            [15, 60, 60]
        );
        assert_eq!(
            ["signal_aloud off\n", "signal_aloud on\n", "signal_aloud loud\n"]
                .map(|text| Settings::parse(text).is_signal_aloud),
            [false, true, true]
        );
        assert_eq!(
            ["sounds 0\n", "sounds 33\n", "sounds 250\n", "sounds loud\n", "search_sound on\n"]
                .map(|text| Settings::parse(text).sounds),
            [0, 30, 100, 40, 40]
        );
        assert_eq!(
            Settings::parse("alarm 1792222200\ntimer 1792189200\nrundown finance off\n"),
            Settings::default(),
            "the alarm and the timer an earlier firmware kept here are the bell's to read, a rundown's switch the rundowns'"
        );
    }

    #[test]
    fn features_lines() {
        let settings = Settings::parse(
            "limit 7\nlimit 50\nlimit lots\nmodel robot gpt-6.1-sol none\nmodel news gpt-6-astra xhigh\nmodel news gpt-7 low\nmodel auto gpt-6-luna\nmodel nobody gpt-6-luna low\ntool web off\ntool signal on\ntool radio on\ntool news maybe\nnews 19\nnews 24\nrundown finance off\nrundown weather off\nsignal_said voice\nsignal_said loud\nsignal_text on\n",
        );
        assert_eq!(settings.terms.limit, Some(50), "a limit that is one of its steps");
        assert_eq!(
            (settings.terms.thinking(Task::Chat), settings.terms.thinking(Task::News)),
            (Thinking::new(Model::Sol, Effort::Low), Thinking::new(Model::Astra, Effort::Xhigh)),
            "a use's last model that reads, at an effort it takes"
        );
        assert_eq!(settings.terms.thinking(Task::Auto), Thinking::STANDARD, "a line without its effort reads as none");
        assert_eq!(settings.tools, Tools::default().with(Tool::Web, false).with(Tool::Signal, true));
        assert_eq!(settings.edition, Edition { hour: Some(19) });
        assert!(settings.is_said_voice && settings.is_text_only);
        assert_eq!(
            (Settings::parse("limit none\n").terms.limit, Settings::parse("news off\n").edition.hour),
            (None, None)
        );
        assert_eq!(
            Settings::parse("model notes gpt-6.1-sol low\nmodel translator gpt-6-astra max\n")
                .terms
                .thinking(Task::Summary),
            Thinking::new(Model::Sol, Effort::Low),
            "an earlier firmware's notes model is the summaries', and its translator's is passed over"
        );
        assert_eq!(
            ["pokedex de\npokedex nl\n", "pokedex fr\n", "pokedex\n"].map(|text| Settings::parse(text).pokedex),
            [Language::German, Language::French, Language::English],
            "the last language that reads, English where none does"
        );
        assert_eq!(
            (Settings::default().terms.limit, Settings::default().tools.count_on(), Settings::default().edition.hour),
            (Some(25), 5, Some(8)),
            "a new device: $25 a month, every tool on but Signal, the edition at 08:00"
        );
    }

    #[test]
    fn stored_merge() {
        let (default, stored) = (
            Settings::default(),
            Settings {
                brightness: 80,
                volume: 60,
                sounds: 80,
                has_robot: false,
                offset: 60,
                logging: Logging::Off,
                theme: Theme::Deck,
                colours: [7, 7, 7, 7, 7, 7],
                signal_wake: 5,
                is_signal_aloud: false,
                auto_replies: 50,
                auto_gap: 10,
                auto_hourly: 20,
                terms: Terms { limit: Some(5), ..Terms::default() },
                tools: Tools::default().with(Tool::Camera, false),
                edition: Edition { hour: Some(6) },
                is_said_voice: true,
                is_text_only: true,
                pokedex: Language::German,
                is_cellular: true,
                is_pin_kept: false,
                is_drive_shown: true,
                is_modem: true,
                uses_tunnel: true,
            },
        );
        assert_eq!(default.merge_stored(stored), stored);
        assert_eq!(default.merge_stored(default), default);
        let set =
            Settings { brightness: 30, volume: 20, offset: -300, signal_wake: 0, theme: Theme::Terminal, ..default };
        assert_eq!(
            set.merge_stored(stored),
            Settings { brightness: 30, volume: 20, offset: -300, signal_wake: 0, theme: Theme::Terminal, ..stored }
        );
        let switched = Settings { has_robot: false, is_signal_aloud: false, logging: Logging::Debug, ..default };
        assert_eq!(
            switched.merge_stored(Settings { has_robot: true, is_signal_aloud: true, logging: Logging::On, ..stored }),
            Settings { logging: Logging::Debug, ..stored }
        );
        assert_eq!(default.with_palette(0).merge_stored(stored).colours, [7, 0, 7, 7, 7, 7]);
        let cheap = Thinking::new(Model::Sol, Effort::Low);
        let kept = Settings { terms: Terms { models: [cheap; Task::ALL.len()], limit: Some(5) }, ..stored };
        let session = default
            .with_value(Setting::Tool(Tool::Web), Value::Switch(false))
            .with_value(Setting::Model(Task::Chat), Value::Model(Model::Astra));
        let merged = session.merge_stored(kept);
        assert_eq!(
            (merged.tools, merged.terms.limit, merged.terms.thinking(Task::News)),
            (kept.tools.with(Tool::Web, false), Some(5), cheap),
            "a tool and a model set in the session leave the other tools, the limit and the other uses' models as kept"
        );
        assert_eq!(merged.terms.thinking(Task::Chat).model(), Model::Astra, "and stand themselves");
    }

    #[test]
    fn steps_of_settings() {
        let settings = Settings::default();
        let step = |setting, way| settings.stepped(setting, way);
        assert_eq!(step(Setting::Tool(Tool::Signal), 1).map(|next| next.tools.is_on(Tool::Signal)), Some(true));
        assert_eq!(step(Setting::TextOnly, -1).map(|next| next.is_text_only), Some(true), "a switch turns either way");
        assert_eq!(
            (step(Setting::SignalWake, 1), step(Setting::SignalWake, -1).map(|next| next.signal_wake)),
            (None, Some(15)),
            "60 minutes is the last step"
        );
        assert_eq!(step(Setting::AutoGap, 1).map(|next| next.auto_gap), Some(60));
        let hour = |hour| Settings { edition: Edition { hour }, ..settings };
        assert_eq!(step(Setting::Edition, 1).map(|next| next.edition.hour), Some(Some(9)));
        assert_eq!(
            [hour(None).stepped(Setting::Edition, -1), hour(Some(23)).stepped(Setting::Edition, 1)],
            [None, None],
            "off below the first hour, 23:00 the last"
        );
        assert_eq!(hour(None).stepped(Setting::Edition, 1).map(|next| next.edition.hour), Some(Some(0)));
        let limit = |limit| Settings { terms: Terms { limit, ..settings.terms }, ..settings };
        assert_eq!(step(Setting::Limit, 1).map(|next| next.terms.limit), Some(Some(50)));
        assert_eq!(
            [Some(100), None].map(|dollars| limit(dollars).stepped(Setting::Limit, 1).map(|next| next.terms.limit)),
            [Some(None), None],
            "none above the highest"
        );
        assert_eq!(limit(Some(1)).stepped(Setting::Limit, -1), None);
        let auto = Setting::Model(Task::Auto);
        let terms = Terms { models: [Thinking::new(Model::Terra, Effort::None); Task::ALL.len()], ..settings.terms };
        let sol = Settings { terms, ..settings }.stepped(auto, 1).expect("a dearer model");
        assert_eq!(
            sol.terms.thinking(Task::Auto),
            Thinking::new(Model::Sol, Effort::Low),
            "a model that takes no effort as little as the one set thinks at the least it takes above it"
        );
        assert_eq!(sol.stepped(Setting::Effort(Task::Auto), -1), None, "and that is its least");
        assert_eq!(
            step(Setting::Effort(Task::Chat), 1).map(|next| next.terms.thinking(Task::Chat).effort()),
            Some(Effort::Xhigh)
        );
        let cheapest = step(Setting::Model(Task::Chat), -1).expect("a cheaper model");
        assert_eq!(
            (cheapest.terms.thinking(Task::Chat).model(), cheapest.stepped(Setting::Model(Task::Chat), -1)),
            (Model::Luna, None),
            "the cheapest is the first"
        );
        assert_eq!(
            (step(Setting::PokedexLanguage, 1).map(|next| next.pokedex), step(Setting::PokedexLanguage, -1)),
            (Some(Language::German), None),
            "English the first of the languages"
        );
        assert!(Setting::TextOnly.is_switch() && !Setting::Model(Task::News).is_switch());
        for setting in Setting::ALL.into_iter().filter(|setting| setting.is_switch()) {
            let turned = settings.stepped(setting, 1).expect("a switch has no end");
            assert_ne!(turned.value(setting), settings.value(setting), "{setting:?} turns");
            assert_eq!(turned.stepped(setting, -1), Some(settings), "{setting:?} turns back, and nothing else with it");
        }
    }

    #[test]
    fn robot_changes() {
        assert!(Setting::ALL.into_iter().all(|setting| Setting::parse_name(setting.to_name()) == Some(setting)));
        let settings = Settings::default();
        let change = |name: &str, word: &str| {
            let setting = Setting::parse_name(name).expect("a setting's name");
            let next = settings.with_value(setting, setting.parse_value(word)?);
            Some((next, settings.is_eased(setting, &next)))
        };
        assert_eq!(
            change("tool_web", "off").map(|(next, eased)| (next.tools.is_on(Tool::Web), eased)),
            Some((false, true))
        );
        assert_eq!(change("tool_signal", "on").map(|change| change.1), Some(false), "a tool switched on waits");
        assert_eq!(
            [change("spending_limit", "10"), change("spending_limit", "none")]
                .map(|change| change.map(|(next, eased)| (next.terms.limit, eased))),
            [Some((Some(10), true)), Some((None, true))],
            "what the device may cost is the owner's to say: no limit waits no more than a lower one"
        );
        assert_eq!(change("spending_limit", "7"), None, "a value that is none of the steps");
        assert_eq!(change("daily_news", "off").map(|(next, eased)| (next.edition.hour, eased)), Some((None, true)));
        assert_eq!(change("daily_news", "6").map(|change| change.1), Some(true), "another hour while it is on");
        assert_eq!(
            [change("signal_fetch", "0"), change("signal_fetch", "5")].map(|change| change.map(|change| change.1)),
            [Some(true), Some(true)],
            "fetched never or more often, at once"
        );
        assert_eq!(change("signal_said", "voice").map(|(next, eased)| (next.is_said_voice, eased)), Some((true, true)));
        let voiced = Settings { is_said_voice: true, is_signal_aloud: false, ..settings };
        assert!(
            !voiced.is_eased(Setting::SaidVoice, &settings) && !voiced.is_eased(Setting::SignalAloud, &settings),
            "what sends a conversation's words to OpenAI waits for the owner's click"
        );
        assert_eq!(
            change("robot_model", "gpt-6-astra").map(|(next, eased)| (next.terms.thinking(Task::Chat).model(), eased)),
            Some((Model::Astra, true)),
            "a dearer model is set at once"
        );
        assert_eq!(
            change("robot_reasoning", "low").map(|(next, eased)| (next.terms.thinking(Task::Chat).effort(), eased)),
            Some((Effort::Low, true))
        );
        assert_eq!(
            [change("auto_gap", "300"), change("auto_replies", "100")].map(|change| change.map(|change| change.1)),
            [Some(true), Some(true)]
        );
        assert_eq!(
            change("pokedex_language", "it").map(|(next, eased)| (next.pokedex, eased)),
            Some((Language::Italian, true))
        );
        assert_eq!(change("pokedex_language", "nl"), None, "a language the Pokédex is not written in");
        assert_eq!(
            [change("mobile_data", "on"), change("mobile_data", "off")]
                .map(|change| change.map(|(next, eased)| (next.is_cellular, eased))),
            [Some((true, true)), Some((false, true))],
            "the modem is switched at once, either way"
        );
        assert_eq!(
            [change("keep_sim_pin", "off"), change("keep_sim_pin", "on")]
                .map(|change| change.map(|(next, eased)| (next.is_pin_kept, eased))),
            [Some((false, true)), Some((true, true))],
            "a PIN no longer kept is at once, and kept as it is"
        );
        let unkept = Settings { is_pin_kept: false, ..settings };
        assert!(!unkept.is_eased(Setting::KeepPin, &settings), "a PIN kept where none was waits for the owner's click");
        assert_eq!(
            [change("usb_drive", "on"), change("usb_drive", "off")]
                .map(|change| change.map(|(next, eased)| (next.is_drive_shown, eased))),
            [Some((true, false)), Some((false, true))],
            "the drive shown to a computer waits for the owner's click, and is off as a new device has it"
        );
        assert_eq!(
            [change("usb_modem", "on"), change("usb_modem", "off")]
                .map(|change| change.map(|(next, eased)| (next.is_modem, eased))),
            [Some((true, false)), Some((false, true))],
            "the modem's connection lent to a computer waits for the owner's click too"
        );
        assert_eq!(change("vpn", "on").map(|(next, eased)| (next.uses_tunnel, eased)), Some((true, true)));
        let tunnelled = Settings { uses_tunnel: true, ..settings };
        assert!(
            !tunnelled.is_eased(Setting::Tunnel, &settings),
            "the tunnel switched off, which sends that computer's packets in the open, waits for it"
        );
        assert_eq!(Setting::parse_name("lock_on_wake"), None);
        assert_eq!(
            [
                settings.with_value(Setting::SignalWake, Value::Step(Some(256))),
                settings.with_value(Setting::AutoHourly, Value::Step(None)),
                settings.with_value(Setting::Limit, Value::Switch(true)),
            ],
            [settings; 3],
            "a step its setting cannot hold, and a value of another kind than its setting's, change nothing"
        );
        let readout = settings.to_readout();
        assert!(
            readout.starts_with("- brightness: 40%\n")
                && readout.contains("- robot_model: gpt-6.1-terra\n")
                && readout.contains("- bell_reasoning: high\n")
                && readout.contains("- spending_limit: 25\n")
                && readout.contains("- tool_signal: off\n")
                && readout.contains("- daily_news: 8\n"),
            "{readout}"
        );
    }

    #[test]
    fn change_description() {
        let told = to_change_description();
        let (_, rest) = told.split_once("less safe or less private, ").expect("the changes that wait");
        let (listed, _) = rest.split_once(", which waits").expect("their end");
        let mut listed: Vec<&str> = listed.split(", ").flat_map(|part| part.split(" or ")).collect();
        let mut waiting = alloc::vec![String::from("a tool switched on")];
        for setting in Setting::ALL.into_iter().filter(|setting| setting.is_switch()) {
            for is_on in [false, true] {
                let before = Settings::default().with_value(setting, Value::Switch(!is_on));
                let next = before.with_value(setting, Value::Switch(is_on));
                let waits = !before.is_eased(setting, &next);
                match setting {
                    Setting::Tool(_) => assert_eq!(waits, is_on, "{setting:?} waits switched on alone"),
                    _ if waits => waiting.push(format!("{} {}", setting.to_name(), next.value_word(setting))),
                    _ => {}
                }
            }
        }
        let mut waiting: Vec<&str> = waiting.iter().map(String::as_str).collect();
        listed.sort_unstable();
        waiting.sort_unstable();
        assert_eq!(listed, waiting, "the model is told every change that waits for a click, and no other");
        assert!(
            told.ends_with("for pokedex_language en, de, fr, es or it.")
                && told.contains("for spending_limit 1, 2, 5, 10, 20, 25, 50 or 100 dollars a month, or none;"),
            "each value by the words the settings read it by: {told}"
        );
    }

    #[test]
    fn modem_lent() {
        let lent = Settings { is_modem: true, is_cellular: true, ..Settings::default() };
        assert!(lent.lends_modem(true) && !lent.lends_modem(false), "only by a modem that takes asks");
        assert!(
            !Settings { is_cellular: false, ..lent }.lends_modem(true)
                && !Settings { is_modem: false, ..lent }.lends_modem(true),
            "and with both switches on"
        );
    }

    #[test]
    fn offset_steps() {
        let settings = Settings::default();
        assert_eq!(
            [settings.step_offset(1), settings.step_offset(-1), settings.step_offset(100), settings.step_offset(-100)]
                .map(|settings| settings.offset),
            [60, -60, 840, -660]
        );
        assert_eq!(
            (
                Settings::parse("offset 345").offset,
                Settings::parse("offset -661").offset,
                Settings::parse("offset 99999999999").offset
            ),
            (345, 0, 0)
        );
    }

    #[test]
    fn brightness_steps() {
        assert_eq!(
            [0, 4, 5, 14, 15, 50, 96, 100, 250].map(nearest_brightness),
            [10, 10, 10, 10, 20, 50, 100, 100, 100]
        );
        assert_eq!(Settings::parse("brightness 0").brightness, DIMMEST);
        assert_eq!(
            (Settings::parse("brightness 4294967295").brightness, nearest_brightness(u32::MAX)),
            (BRIGHTEST, BRIGHTEST)
        );
        let settings = Settings::default();
        assert_eq!(
            [
                settings.step_brightness(1),
                settings.step_brightness(-1),
                settings.step_brightness(9),
                settings.step_brightness(-9)
            ]
            .map(|settings| settings.brightness),
            [50, 30, 100, 10]
        );
    }

    #[test]
    fn backlight_share_rounds_up() {
        assert_eq!(backlight_level(255, 50), 128);
        assert_eq!(
            (backlight_level(255, 0), backlight_level(255, BRIGHTEST), backlight_level(255, 250)),
            (0, 255, 255)
        );
        assert_eq!(backlight_level(255, DIMMEST), 26);
        assert_eq!(backlight_level(255, 1), 3);
    }

    #[test]
    fn volume_steps() {
        assert_eq!([0, 4, 5, 14, 15, 50, 96, 100, 250].map(nearest_volume), [0, 0, 10, 10, 20, 50, 100, 100, 100]);
        assert_eq!((Settings::parse("volume 0").volume, Settings::parse("volume 33").volume), (QUIETEST, 30));
        assert_eq!((Settings::parse("volume 4294967295").volume, nearest_volume(u32::MAX)), (LOUDEST, LOUDEST));
        let settings = Settings::default();
        assert_eq!([1, -1, 3, -7, -8].map(|steps| settings.step_volume(steps).volume), [80, 60, 100, 0, 0]);
        let loudest = Settings { volume: LOUDEST, ..settings };
        assert_eq!([1, -1, -9, -10, -11].map(|steps| loudest.step_volume(steps).volume), [100, 90, 10, 0, 0]);
        assert_eq!(settings.step_volume(i32::MIN).volume, QUIETEST);
    }

    #[test]
    fn log_line() {
        assert_eq!(
            Settings::default().to_log_line(),
            alloc::format!(
                "brightness 40, volume 70, sounds 40, robot on, offset 0, logging on, theme dial, signal_wake 60, signal_aloud on, auto_replies 20, auto_gap 30, auto_hourly 30, colours stage C, colours dial C, colours deck C, colours terminal C, colours companion C, {}",
                DEFAULT_TAIL.trim_end().replace('\n', ", ")
            )
        );
    }
}
