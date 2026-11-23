//! The functions the language model may call.

use super::memory::{Area, NAME_MAX_CHARS};
use super::state::Lapse;
use crate::base::json::{escape, items, number_at, text_at, unescape, value_at};
use crate::base::text::has_words;
use crate::base::time::offset;
use crate::domain::bell::{self, Change};
use crate::domain::scene;
use crate::domain::settings::{Setting, Tool, Tools, Value, to_change_description};
use crate::domain::signal::robot::Question;
use crate::gfx::palette;
use crate::gfx::theme::Theme;
use crate::signal::address::Peer;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const MEMORY_FUNCTIONS: &str = r#"{"type":"function","name":"save_memory","description":"Keep something about the user in your long memory. Call it in the same turn they share something durable or ask you to remember something. core true for facts that stay true and change your defaults: name, age, home, languages, work, the people in their life, hard constraints, standing tastes; core false for the rest worth keeping. When a fact changes or corrects an earlier one, set replaces to distinctive text of the old entry, which then goes; null when nothing is corrected.","strict":true,"parameters":{"type":"object","properties":{"content":{"type":"string","description":"The fact, plainly and on its own."},"core":{"type":"boolean","description":"True for what always holds of the user."},"replaces":{"type":["string","null"],"description":"Text of the entry this one replaces, or null."}},"required":["content","core","replaces"],"additionalProperties":false}},{"type":"function","name":"search_memory","description":"Search your long memory, the words of every earlier day of the conversation and, where you may read them, the user's notes and translations, for what the user refers to and you do not have in front of you. Results come dated, the best first; an empty query gives what you learned last.","strict":true,"parameters":{"type":"object","properties":{"query":{"type":"string","description":"Words to look for."}},"required":["query"],"additionalProperties":false}},{"type":"function","name":"profile","description":"In the first conversation: note what you learned of the user in one of the areas you ask about, or that they would rather not say. Entries are short: a name, an age, a place, or each pick of a list by itself. The answer says what is known, what is still open, and when you know enough.","strict":true,"parameters":{"type":"object","properties":{"area":{"type":"string","enum":["name","age","place","languages","work","interests","movies","games","music","media"]},"entries":{"type":"array","items":{"type":"string"}},"declined":{"type":"boolean","description":"True when they would rather not say."}},"required":["area","entries","declined"],"additionalProperties":false}},{"type":"function","name":"name_robot","description":"Take the name the user gives you, or the one you chose when they asked you to; null when they would rather you had none. Letters, digits, blanks, hyphens and apostrophes the screen can write, {NAME_MAX_CHARS} characters at the most: the answer says whether the name was taken.","strict":true,"parameters":{"type":"object","properties":{"name":{"type":["string","null"]}},"required":["name"],"additionalProperties":false}},{"type":"function","name":"set_tone","description":"Take how the user wants you to sound: one tone or several, a word or two each, or their own words; an empty list when they want none in particular. It colours your replies and your voice from then on.","strict":true,"parameters":{"type":"object","properties":{"tones":{"type":"array","items":{"type":"string"}}},"required":["tones"],"additionalProperties":false}},{"type":"function","name":"end_onboarding","description":"End the first conversation's questions now: the user has had enough of them or wants to talk about something else. Never come back to the questions after it.","strict":true,"parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}},{"type":"function","name":"forget","description":"Have something forgotten the user wants forgotten: a fact you keep, a day of the conversation, or the thread you are given every turn. It opens that on its screen once your reply is over, and only the user's click there forgets it: say so, and never that it is forgotten. which is text only that fact holds, the day as YYYYMMDD, or thread.","strict":true,"parameters":{"type":"object","properties":{"which":{"type":"string"}},"required":["which"],"additionalProperties":false}}"#;
const NAME_MAX_MARK: &str = "{NAME_MAX_CHARS}";
const READ_DEVICE_HEAD: &str = r#"{"type":"function","name":"read_device","description":"Read what the device holds. "#;
const READ_NOTES: &str = "notes and translations list the user's audio notes and translated conversations, and with day as YYYYMMDD give that day's in full with their summaries";
const READ_NEWS: &str = "news gives a day's edition of the daily news, the latest with day null";
const READ_DEVICE: &str = "battery gives the battery's level and what is left of it; alarm gives every alarm, reminder and timer that is set, with its day and time on the device's clock; cost gives what OpenAI's services were paid this month and in all, app by app and month by month; about gives what the device runs and what it is; settings gives every setting by the name change_setting knows it by";
const USE_DEVICE: &str = r#"{"type":"function","name":"use_device","description":"Do on the device what its screens do, for the user who asks it by voice or by keyboard: open a screen (the photo camera to take a picture, the video camera, the pictures and videos, the audio notes, the translations, the news, the rundowns the daily news is made of, the web browser, Signal, Signal's calls while the device is linked to it, the Pokédex where the device has it, wifi, brightness, volume, themes, colors, clock, the alarms, reminders and timers that are set, the cost of OpenAI's services, power and battery, about, what you keep of the user, the days of your conversation, the window that changes the passphrase, the keys the SIM's PIN is typed on, the window that scans the config of the tunnel a computer on the cable goes through, the debug screen once the user has unlocked it), open a menu (the camera's, the microphone's, the settings, the features under them, the settings of one feature: yours, your tools, Signal's, the notes', the news', the web browser's, the bell's, the Pokédex's, or mobile data, or the USB drive), begin an audio note or a translated conversation, set brightness, volume or the device's own sounds (searching, the bell's ring, a notice) to a percent, the theme everything is laid out in by its name (stage, dial, deck, terminal, companion, rail), the colors the theme is drawn in by their letter A to P or the clock's offset from UTC as +HH:MM or -HH:MM, go home, or sleep. value is the percent, the name, the letter or the offset for the six settings, else null. The screen opens when your reply is over: say at most a few words.","strict":true,"parameters":{"type":"object","properties":{"action":{"type":"string","enum":["open_photo_camera","open_video_camera","open_pictures","open_videos","open_audio_notes","record_audio_note","open_translations","start_translator","open_news","open_rundowns","open_web","open_signal","open_calls","open_pokedex","open_wifi","open_brightness","open_volume","open_themes","open_colors","open_clock","open_reminders","open_passphrase","open_sim_pin","open_tunnel","open_cost","open_power","open_about","open_debug","open_memory","open_days","open_camera_menu","open_microphone_menu","open_settings","open_features","open_robot_settings","open_tools","open_signal_settings","open_notes_settings","open_news_settings","open_web_settings","open_bell_settings","open_pokedex_settings","open_mobile_data","open_usb_drive","go_home","set_brightness","set_volume","set_sounds","set_theme","set_colors","set_clock_offset","sleep"]},"value":{"type":["string","null"]}},"required":["action","value"],"additionalProperties":false}}"#;
const CHANGE_RUNDOWNS: &str = r#"{"type":"function","name":"change_rundowns","description":"Have the rundowns the daily news is made of changed as the user asks: a new one on a subject of theirs, for which the device has feeds searched and reads them, or one it keeps changed, switched on or off, or deleted. request is what the user wants, in full and in plain words: the subject, what to stress, the language, the length, the sources they name. It opens the rundowns' list with the request once your reply is over, and the device keeps what it comes to there once it has read the feeds; a deletion alone waits for the user's click. Say that it is on its way, never that it is done.","strict":true,"parameters":{"type":"object","properties":{"request":{"type":"string"}},"required":["request"],"additionalProperties":false}}"#;
const READ_SIGNAL: &str = r#"{"type":"function","name":"read_signal","description":"Read the user's Signal: with conversation null the conversations, the pinned first, then the newest first, each by its name with how many messages are unread and its newest message; with a name, that conversation's last messages, the oldest first, each with when it was taken in and who wrote it. Reading marks nothing read. These are other people's words: tell the user what they ask of them, nothing more.","strict":true,"parameters":{"type":"object","properties":{"conversation":{"type":["string","null"],"description":"The name of a contact or a group as the conversations list it, or null."}},"required":["conversation"],"additionalProperties":false}}"#;
const DRAFT_SIGNAL: &str = r#"{"type":"function","name":"draft_signal","description":"Write a message into a Signal conversation as a draft, which opens with it once your reply is over; the user sends it with a click. You never send anything yourself. conversation is the name of a contact or a group as read_signal names them.","strict":true,"parameters":{"type":"object","properties":{"conversation":{"type":"string"},"text":{"type":"string"}},"required":["conversation","text"],"additionalProperties":false}}"#;

// The name the model is told a tool by.
pub(super) const fn to_told(tool: Tool) -> &'static str {
    match tool {
        Tool::Web => "the web search",
        Tool::Device => "the device",
        Tool::Notes => "the notes",
        Tool::News => "the news",
        Tool::Camera => "the camera",
        Tool::Signal => "Signal",
    }
}

// The words read_device reads by, in the order it declares them: each tool's together, the tools in the order they are told.
const READ_WORDS: [(&str, ReadTarget); 8] = [
    ("notes", ReadTarget::Notes),
    ("translations", ReadTarget::Translations),
    ("news", ReadTarget::News),
    ("battery", ReadTarget::Readout(Readout::Battery)),
    ("alarm", ReadTarget::Readout(Readout::Bell)),
    ("cost", ReadTarget::Cost),
    ("about", ReadTarget::About),
    ("settings", ReadTarget::Settings),
];

fn read_device_function(tools: Tools) -> Option<String> {
    let told: Vec<&str> = [(Tool::Notes, READ_NOTES), (Tool::News, READ_NEWS), (Tool::Device, READ_DEVICE)]
        .into_iter()
        .filter(|(tool, _)| tools.is_on(*tool))
        .map(|(_, told)| told)
        .collect();
    if told.is_empty() {
        return None;
    }
    let read = READ_WORDS.iter().filter(|(_, target)| tools.is_on(target.tool()));
    let words: Vec<String> = read.map(|(word, _)| format!("\"{word}\"")).collect();
    Some(format!(
        "{READ_DEVICE_HEAD}{}.\",\"strict\":true,\"parameters\":{{\"type\":\"object\",\"properties\":{{\"what\":{{\"type\":\"string\",\"enum\":[{}]}},\"day\":{{\"type\":[\"string\",\"null\"]}}}},\"required\":[\"what\",\"day\"],\"additionalProperties\":false}}}}",
        told.join("; "),
        words.join(",")
    ))
}

fn change_setting_function() -> String {
    let names: Vec<String> = Setting::ALL.iter().map(|setting| format!("\"{}\"", setting.to_name())).collect();
    format!(
        "{{\"type\":\"function\",\"name\":\"change_setting\",\"description\":\"{}\",\"strict\":true,\"parameters\":{{\"type\":\"object\",\"properties\":{{\"setting\":{{\"type\":\"string\",\"enum\":[{}]}},\"value\":{{\"type\":\"string\"}}}},\"required\":[\"setting\",\"value\"],\"additionalProperties\":false}}}}",
        escape(&to_change_description()),
        names.join(",")
    )
}

/// The request's function declarations of the tools that are on, in an order that never changes, for the service's cache.
pub fn functions(tools: Tools) -> String {
    let mut list = MEMORY_FUNCTIONS.replace(NAME_MAX_MARK, &format!("{NAME_MAX_CHARS}"));
    let device = tools.is_on(Tool::Device).then(|| {
        let functions = [String::from(USE_DEVICE), change_setting_function()].into_iter();
        functions.chain(bell::FUNCTIONS.map(String::from)).chain([String::from(CHANGE_RUNDOWNS)])
    });
    let signal = tools.is_on(Tool::Signal).then(|| [READ_SIGNAL, DRAFT_SIGNAL].map(String::from));
    let functions = read_device_function(tools).into_iter().chain(device.into_iter().flatten());
    for function in functions.chain(signal.into_iter().flatten()) {
        list.push(',');
        list.push_str(&function);
    }
    format!("[{list}]")
}

/// The answer to a call of an unknown function, of a tool that is off, or with foreign arguments.
pub const UNKNOWN: &str = "No such function, or arguments that are not its own.";
// The answer to a call that has the device act.
pub(super) const DONE: &str = "Done once your reply is over.";

/// What the model may read of the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadTarget {
    Notes,
    Translations,
    News,
    Cost,
    About,
    Settings,
    Readout(Readout),
}

impl ReadTarget {
    const fn tool(self) -> Tool {
        match self {
            ReadTarget::Notes | ReadTarget::Translations => Tool::Notes,
            ReadTarget::News => Tool::News,
            ReadTarget::Cost | ReadTarget::About | ReadTarget::Settings | ReadTarget::Readout(_) => Tool::Device,
        }
    }
}

/// What the interface reads out for the model, through the main loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Readout {
    Battery,
    Bell,
}

/// What the main loop is asked to answer the model: a readout of the interface's, a question for Signal's thread, or a change of the bell, which the interface holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Asked {
    Device(Readout),
    Signal(Question),
    Bell(Change),
}

/// The words of a draft the robot wrote, which no log tells.
#[derive(Clone, PartialEq, Eq)]
pub struct Words(pub(crate) String);

impl core::fmt::Debug for Words {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Words")
    }
}

/// A screen the model may open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Destination {
    PhotoCamera,
    VideoCamera,
    Pictures,
    Videos,
    Notes,
    Translations,
    News,
    Rundowns,
    Web,
    Signal,
    // Signal's calls: the log, and whom a new call may go to.
    Calls,
    Pokedex,
    Wifi,
    Brightness,
    Volume,
    Themes,
    Colours,
    Clock,
    Bell,
    // The window the passphrase is changed in.
    Passphrase,
    // The keys the SIM's PIN is typed on.
    SimPin,
    // The window a tunnel's config is scanned in.
    Tunnel,
    Cost,
    Power,
    About,
    Debug,
    Memory,
    Days,
}

/// A level of the screens' tree the model may open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Menu {
    Camera,
    Microphone,
    Settings,
    Features,
    Robot,
    Tools,
    Signal,
    Notes,
    News,
    Web,
    Bell,
    Pokedex,
    MobileData,
    UsbDrive,
}

impl Menu {
    // The name its line in the tree goes by.
    pub(crate) const fn to_name(self) -> &'static str {
        match self {
            Menu::Camera => scene::CAMERA,
            Menu::Microphone => scene::MIC,
            Menu::Settings => scene::SETTINGS,
            Menu::Features => scene::FEATURES,
            Menu::Robot => scene::ROBOT,
            Menu::Tools => scene::TOOLS,
            Menu::Signal => scene::SIGNAL,
            Menu::Notes => scene::NOTES,
            Menu::News => scene::NEWS,
            Menu::Web => scene::WEB,
            Menu::Bell => scene::BELL,
            Menu::Pokedex => scene::POKEDEX,
            Menu::MobileData => scene::MOBILE_DATA,
            Menu::UsbDrive => scene::USB_DRIVE,
        }
    }
}

/// What the model may have the device do; nothing that deletes, forgets or powers off by itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Act {
    Open(Destination),
    Enter(Menu),
    RecordNote,
    Translate,
    Brightness(u8),
    Volume(u8),
    Sounds(u8),
    Theme(Theme),
    // A palette's index.
    Colours(usize),
    // The clock's minutes ahead of UTC.
    Offset(i16),
    Home,
    Sleep,
    Change(Setting, Value),
    // Opens the line where a click forgets it.
    Forget(Lapse),
    // Opens a conversation with a draft in it, which a click sends.
    Draft(Peer, Words),
    // Opens the rundowns' list with a request for it.
    Rundowns(Words),
}

/// The log line of an act.
pub fn to_act_line(act: &Act) -> String {
    format!("chat: the conversation has the device do {act:?}")
}

fn parse_destination(action: &str) -> Option<Destination> {
    Some(match action {
        "open_photo_camera" => Destination::PhotoCamera,
        "open_video_camera" => Destination::VideoCamera,
        "open_pictures" => Destination::Pictures,
        "open_videos" => Destination::Videos,
        "open_audio_notes" => Destination::Notes,
        "open_translations" => Destination::Translations,
        "open_news" => Destination::News,
        "open_rundowns" => Destination::Rundowns,
        "open_web" => Destination::Web,
        "open_signal" => Destination::Signal,
        "open_calls" => Destination::Calls,
        "open_pokedex" => Destination::Pokedex,
        "open_wifi" => Destination::Wifi,
        "open_brightness" => Destination::Brightness,
        "open_volume" => Destination::Volume,
        "open_themes" => Destination::Themes,
        "open_colors" => Destination::Colours,
        "open_clock" => Destination::Clock,
        "open_reminders" => Destination::Bell,
        "open_passphrase" => Destination::Passphrase,
        "open_sim_pin" => Destination::SimPin,
        "open_tunnel" => Destination::Tunnel,
        "open_cost" => Destination::Cost,
        "open_power" => Destination::Power,
        "open_about" => Destination::About,
        "open_debug" => Destination::Debug,
        "open_memory" => Destination::Memory,
        "open_days" => Destination::Days,
        _ => return None,
    })
}

fn parse_menu(action: &str) -> Option<Menu> {
    Some(match action {
        "open_camera_menu" => Menu::Camera,
        "open_microphone_menu" => Menu::Microphone,
        "open_settings" => Menu::Settings,
        "open_features" => Menu::Features,
        "open_robot_settings" => Menu::Robot,
        "open_tools" => Menu::Tools,
        "open_signal_settings" => Menu::Signal,
        "open_notes_settings" => Menu::Notes,
        "open_news_settings" => Menu::News,
        "open_web_settings" => Menu::Web,
        "open_bell_settings" => Menu::Bell,
        "open_pokedex_settings" => Menu::Pokedex,
        "open_mobile_data" => Menu::MobileData,
        "open_usb_drive" => Menu::UsbDrive,
        _ => return None,
    })
}

fn parse_percent(value: Option<&str>) -> Option<u8> {
    value?.trim().trim_end_matches('%').trim().parse().ok()
}

fn parse_act(action: &str, value: Option<&str>) -> Option<Act> {
    if let Some(destination) = parse_destination(action) {
        return Some(Act::Open(destination));
    }
    if let Some(menu) = parse_menu(action) {
        return Some(Act::Enter(menu));
    }
    Some(match action {
        "record_audio_note" => Act::RecordNote,
        "start_translator" => Act::Translate,
        "go_home" => Act::Home,
        "sleep" => Act::Sleep,
        "set_brightness" => Act::Brightness(parse_percent(value)?),
        "set_volume" => Act::Volume(parse_percent(value)?),
        "set_sounds" => Act::Sounds(parse_percent(value)?),
        "set_theme" => Act::Theme(Theme::parse(&value?.trim().to_ascii_lowercase())?),
        "set_colors" => Act::Colours(palette::parse_letter(&value?.trim().to_ascii_uppercase())?),
        "set_clock_offset" => Act::Offset(offset::parse_text(&value?.to_ascii_uppercase())?),
        _ => return None,
    })
}

/// A call the memory takes.
#[derive(Clone, PartialEq, Eq)]
pub enum MemoryCall {
    Save { content: String, is_core: bool, replaces: Option<String> },
    Profile { area: Area, entries: Vec<String>, is_declined: bool },
    Name(Option<String>),
    Tone(Vec<String>),
}

// The owner's facts are told by no log.
impl core::fmt::Debug for MemoryCall {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            MemoryCall::Save { .. } => "Save",
            MemoryCall::Profile { .. } => "Profile",
            MemoryCall::Name(_) => "Name",
            MemoryCall::Tone(_) => "Tone",
        })
    }
}

/// A parsed call of a function.
#[derive(Clone, PartialEq, Eq)]
pub enum ToolCall {
    Memory(MemoryCall),
    Search(String),
    EndOnboarding,
    Forget(String),
    Read { target: ReadTarget, day: Option<String> },
    Device(Act),
    Change(Setting, Value),
    Bell(Change),
    // The conversations, or the last messages of one by its name.
    ReadSignal(Option<String>),
    DraftSignal { conversation: String, text: String },
}

// What is searched for, a contact's name and a draft's words are told by no log.
impl core::fmt::Debug for ToolCall {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            ToolCall::Memory(call) => return core::fmt::Debug::fmt(call, f),
            ToolCall::Search(_) => "Search",
            ToolCall::EndOnboarding => "EndOnboarding",
            ToolCall::Forget(_) => "Forget",
            ToolCall::Read { .. } => "Read",
            ToolCall::Device(_) => "Device",
            ToolCall::Change(..) => "Change",
            ToolCall::Bell(_) => "Bell",
            ToolCall::ReadSignal(_) => "ReadSignal",
            ToolCall::DraftSignal { .. } => "DraftSignal",
        })
    }
}

impl ToolCall {
    // True for a call whose answer the model waits for.
    pub(super) const fn needs_answer(&self) -> bool {
        matches!(
            self,
            ToolCall::Search(_)
                | ToolCall::Read { .. }
                | ToolCall::Forget(_)
                | ToolCall::Bell(_)
                | ToolCall::ReadSignal(_)
                | ToolCall::DraftSignal { .. }
        )
    }

    // The tool a call needs on; none for the memory's.
    const fn tool(&self) -> Option<Tool> {
        match self {
            ToolCall::Read { target, .. } => Some(target.tool()),
            ToolCall::Device(_) | ToolCall::Change(..) | ToolCall::Bell(_) => Some(Tool::Device),
            ToolCall::ReadSignal(_) | ToolCall::DraftSignal { .. } => Some(Tool::Signal),
            ToolCall::Memory(_) | ToolCall::Search(_) | ToolCall::EndOnboarding | ToolCall::Forget(_) => None,
        }
    }
}

fn text_field(arguments: &str, name: &str) -> Option<String> {
    text_at(arguments, &[name]).map(unescape)
}

fn nonblank_field(arguments: &str, name: &str) -> Option<String> {
    text_field(arguments, name).filter(|text| !text.trim().is_empty())
}

fn bool_field(arguments: &str, name: &str) -> bool {
    number_at(arguments, &[name]).unwrap_or(false)
}

fn list_field(arguments: &str, name: &str) -> Vec<String> {
    let path = [name];
    let texts = items(arguments, &path).filter_map(|item| text_at(item, &[]));
    texts.map(|text| String::from(unescape(text).trim())).filter(|entry| !entry.is_empty()).collect()
}

fn parse_read_target(word: &str) -> Option<ReadTarget> {
    READ_WORDS.iter().find(|(known, _)| *known == word).map(|(_, target)| *target)
}

fn parse_change(arguments: &str) -> Option<ToolCall> {
    let setting = Setting::parse_name(text_at(arguments, &["setting"])?)?;
    Some(ToolCall::Change(setting, setting.parse_value(&text_field(arguments, "value")?)?))
}

/// The call, where it is of a function declared with the tools that are on and with arguments of its own.
pub fn parse_tool_call(name: &str, arguments: &str, tools: Tools) -> Option<ToolCall> {
    value_at(arguments, &[]).filter(|value| value.starts_with('{'))?;
    let call = match name {
        "save_memory" => ToolCall::Memory(MemoryCall::Save {
            content: nonblank_field(arguments, "content")?,
            is_core: bool_field(arguments, "core"),
            replaces: nonblank_field(arguments, "replaces"),
        }),
        "search_memory" => ToolCall::Search(text_field(arguments, "query").unwrap_or_default()),
        "profile" => ToolCall::Memory(MemoryCall::Profile {
            area: Area::parse(text_at(arguments, &["area"])?)?,
            entries: list_field(arguments, "entries"),
            is_declined: bool_field(arguments, "declined"),
        }),
        "name_robot" => {
            ToolCall::Memory(MemoryCall::Name(text_field(arguments, "name").map(|name| name.trim().into())))
        }
        "set_tone" => ToolCall::Memory(MemoryCall::Tone(list_field(arguments, "tones"))),
        "end_onboarding" => ToolCall::EndOnboarding,
        "forget" => ToolCall::Forget(nonblank_field(arguments, "which")?.trim().into()),
        "read_device" => ToolCall::Read {
            target: parse_read_target(text_at(arguments, &["what"])?)?,
            day: nonblank_field(arguments, "day"),
        },
        "use_device" => {
            ToolCall::Device(parse_act(text_at(arguments, &["action"])?, text_field(arguments, "value").as_deref())?)
        }
        "change_setting" => parse_change(arguments)?,
        "change_rundowns" => ToolCall::Device(Act::Rundowns(Words(nonblank_field(arguments, "request")?))),
        "read_signal" => {
            ToolCall::ReadSignal(nonblank_field(arguments, "conversation").map(|name| String::from(name.trim())))
        }
        "draft_signal" => ToolCall::DraftSignal {
            conversation: nonblank_field(arguments, "conversation")?.trim().into(),
            text: text_field(arguments, "text").filter(|text| has_words(text))?,
        },
        _ => ToolCall::Bell(bell::parse_call(name, arguments)?),
    };
    call.tool().is_none_or(|tool| tools.is_on(tool)).then_some(call)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn all_on() -> Tools {
        Tools::default().with(Tool::Signal, true)
    }

    #[test]
    fn declared_functions_parse() {
        let names = [
            ("save_memory", r#"{"content":"x","core":true,"replaces":null}"#),
            ("search_memory", r#"{"query":"x"}"#),
            ("profile", r#"{"area":"age","entries":["34"],"declined":false}"#),
            ("name_robot", r#"{"name":null}"#),
            ("set_tone", r#"{"tones":[]}"#),
            ("end_onboarding", "{}"),
            ("forget", r#"{"which":"thread"}"#),
            ("read_device", r#"{"what":"notes","day":null}"#),
            ("use_device", r#"{"action":"go_home","value":null}"#),
            ("change_setting", r#"{"setting":"tool_web","value":"off"}"#),
            ("set_reminder", r#"{"on":null,"at":"07:30","in_seconds":null,"text":null}"#),
            ("cancel_reminder", r#"{"which":"all"}"#),
            ("change_rundowns", r#"{"request":"a rundown about Formula 1, in German"}"#),
            ("read_signal", r#"{"conversation":null}"#),
            ("draft_signal", r#"{"conversation":"Anna","text":"Lunch?"}"#),
        ];
        let declared = functions(all_on());
        for (name, arguments) in names {
            assert!(declared.contains(&format!("\"name\":\"{name}\"")), "{name}");
            assert!(parse_tool_call(name, arguments, all_on()).is_some(), "{name}");
        }
        assert!(
            value_at(&declared, &[]).is_some_and(|value| value.len() == declared.len()),
            "the declarations are one JSON list"
        );
    }

    #[test]
    fn tools_off_undeclared() {
        let memory_only = Tool::ALL.into_iter().fold(Tools::default(), |tools, tool| tools.with(tool, false));
        let declared = functions(memory_only);
        assert!(
            declared.contains("\"name\":\"forget\"")
                && !declared.contains("read_device")
                && !declared.contains("use_device")
                && !declared.contains("change_setting"),
            "the memory's functions and forget alone: {declared}"
        );
        let news_only = memory_only.with(Tool::News, true);
        assert!(functions(news_only).contains("\"enum\":[\"news\"]"), "read_device reads what its tools give");
        assert_eq!(
            parse_tool_call("read_device", r#"{"what":"notes","day":null}"#, news_only),
            None,
            "a call of a tool that is off is no call"
        );
        assert_eq!(parse_tool_call("use_device", r#"{"action":"go_home","value":null}"#, news_only), None);
        assert_eq!(
            parse_tool_call("forget", r#"{"which":" 20261016 "}"#, memory_only),
            Some(ToolCall::Forget("20261016".into()))
        );
        let changes = change_setting_function();
        assert!(
            changes.contains("a tool switched on, signal_read_aloud on, signal_said text, keep_sim_pin on, usb_drive on, usb_modem on or vpn off, which waits"),
            "what waits for the user's click, by the settings' own names: {changes}"
        );
        assert!(
            changes.contains("value is on or off for a switch; for daily_news an hour 0 to 23 of the device's clock, or off; for spending_limit 1, 2, 5, 10, 20, 25, 50 or 100 dollars a month, or none; for signal_fetch 0, 5, 10, 15 or 60 minutes, 0 for never; for signal_said text or voice; for auto_replies "),
            "the words a value is read by, as the settings read them: {changes}"
        );
        assert!(
            changes.contains("1, 2, 5, 10, 20, 25, 50 or 100 dollars")
                && changes.contains("gpt-6-luna, gpt-6.1-terra, gpt-6.1-sol or gpt-6-astra")
                && changes.contains("for pokedex_language en, de, fr, es or it.")
                && changes.contains("\"robot_reasoning\""),
            "{changes}"
        );
    }

    fn names_of(tools: Tools) -> Vec<String> {
        let declared = functions(tools);
        items(&declared, &[]).filter_map(|function| text_at(function, &["name"])).map(String::from).collect()
    }

    fn declared_words(function: &str, parameter: &str) -> Vec<String> {
        let declared = functions(all_on());
        let path = ["parameters", "properties", parameter, "enum"];
        let function = items(&declared, &[]).find(|declared| text_at(declared, &["name"]) == Some(function));
        items(function.expect("declared"), &path).filter_map(|word| text_at(word, &[])).map(String::from).collect()
    }

    #[test]
    fn declared_words_parse() {
        assert_eq!(
            declared_words("read_device", "what"),
            ["notes", "translations", "news", "battery", "alarm", "cost", "about", "settings"],
            "read_device declares the words it reads by, each tool's together"
        );
        let actions = declared_words("use_device", "action");
        assert_eq!(actions.len(), 52, "use_device declares its actions");
        for action in &actions {
            let values = ["50", "deck", "a", "+02:00"];
            assert!(values.iter().any(|value| parse_act(action, Some(value)).is_some()), "{action}");
        }
        let count = |is_kind: fn(&Act) -> bool| {
            actions.iter().filter(|action| parse_act(action, None).is_some_and(|act| is_kind(&act))).count()
        };
        assert_eq!(
            (count(|act| matches!(act, Act::Open(_))), count(|act| matches!(act, Act::Enter(_)))),
            (28, 14),
            "every screen and every menu of the tree by a word of its own"
        );
        let areas = declared_words("profile", "area");
        assert_eq!(areas.len(), 1 + Area::REST.len(), "the name and the areas after it");
        assert!(areas.iter().all(|area| Area::parse(area).is_some()), "{areas:?}");
    }

    #[test]
    fn signal_function_order() {
        let memory = ["save_memory", "search_memory", "profile", "name_robot", "set_tone", "end_onboarding", "forget"];
        let device =
            ["read_device", "use_device", "change_setting", "set_reminder", "cancel_reminder", "change_rundowns"];
        assert_eq!(names_of(Tools::default()), [&memory[..], &device].concat(), "Signal is off in a new device");
        assert_eq!(
            names_of(all_on()),
            [&memory[..], &device, &["read_signal", "draft_signal"]].concat(),
            "appended after the device's"
        );
        let signal_only =
            Tool::ALL.into_iter().fold(Tools::default(), |tools, tool| tools.with(tool, tool == Tool::Signal));
        assert_eq!(names_of(signal_only), [&memory[..], &["read_signal", "draft_signal"]].concat());
        let declared = functions(all_on());
        assert!(
            items(&declared, &[])
                .filter(|function| text_at(function, &["name"]).is_some_and(|name| name.ends_with("_signal")))
                .all(|function| number_at::<bool>(function, &["strict"]) == Some(true)),
            "both strict"
        );
        for (name, arguments) in
            [("read_signal", r#"{"conversation":null}"#), ("draft_signal", r#"{"conversation":"A","text":"B"}"#)]
        {
            assert_eq!(parse_tool_call(name, arguments, Tools::default()), None, "{name} while Signal is off");
            assert!(parse_tool_call(name, arguments, all_on()).is_some_and(|call| call.needs_answer()));
        }
    }

    #[test]
    fn signal_calls_name_a_conversation() {
        let call = |name, arguments| parse_tool_call(name, arguments, all_on());
        assert_eq!(call("read_signal", r#"{"conversation":null}"#), Some(ToolCall::ReadSignal(None)));
        assert_eq!(
            call("read_signal", r#"{"conversation":" Anna "}"#),
            Some(ToolCall::ReadSignal(Some("Anna".into())))
        );
        assert_eq!(
            call("read_signal", r#"{"conversation":" "}"#),
            Some(ToolCall::ReadSignal(None)),
            "no name: the list"
        );
        assert_eq!(
            call("draft_signal", r#"{"conversation":" Anna ","text":"Lunch at \"one\"?"}"#),
            Some(ToolCall::DraftSignal { conversation: "Anna".into(), text: "Lunch at \"one\"?".into() })
        );
        for arguments in [
            r#"{"conversation":"Anna","text":" \n"}"#,
            r#"{"conversation":" ","text":"Hi"}"#,
            r#"{"conversation":null,"text":"Hi"}"#,
            r#"{"text":"Hi"}"#,
            "not json",
        ] {
            assert_eq!(call("draft_signal", arguments), None, "{arguments}");
        }
        assert_eq!(
            alloc::format!("{:?}", Act::Draft(Peer::Group([1; 32]), Words("Lunch?".into()))),
            "Draft(Group, Words)"
        );
        assert_eq!(
            alloc::format!("{:?}", call("draft_signal", r#"{"conversation":"Anna","text":"Lunch?"}"#)),
            "Some(DraftSignal)",
            "nor does a call's formatting tell whom it is for or its words"
        );
        assert_eq!(
            [Lapse::Fact("**Name:** Anna".into()), Lapse::Day("20261016".into()), Lapse::Thread]
                .map(|lapse| to_act_line(&Act::Forget(lapse))),
            [
                "chat: the conversation has the device do Forget(Fact)",
                "chat: the conversation has the device do Forget(Day(\"20261016\"))",
                "chat: the conversation has the device do Forget(Thread)"
            ],
            "the log names no fact"
        );
    }

    #[test]
    fn call_arguments() {
        let tools = all_on();
        assert_eq!(
            parse_tool_call("save_memory", r#"{"content":"Likes \"tea\"","core":false,"replaces":" "}"#, tools),
            Some(ToolCall::Memory(MemoryCall::Save {
                content: "Likes \"tea\"".into(),
                is_core: false,
                replaces: None
            }))
        );
        assert_eq!(parse_tool_call("save_memory", r#"{"content":" ","core":false,"replaces":null}"#, tools), None);
        assert_eq!(
            parse_tool_call("profile", r#"{"area":"movies","entries":[" Alien ",""],"declined":false}"#, tools),
            Some(ToolCall::Memory(MemoryCall::Profile {
                area: Area::Movies,
                entries: vec!["Alien".into()],
                is_declined: false
            }))
        );
        assert_eq!(parse_tool_call("profile", r#"{"area":"pets","entries":[],"declined":false}"#, tools), None);
        assert_eq!(parse_tool_call("nothing", "{}", tools), None);
        assert_eq!(parse_tool_call("search_memory", "not json", tools), None);
        assert_eq!(
            parse_tool_call("read_device", r#"{"what":"alarm","day":"20261016"}"#, tools),
            Some(ToolCall::Read { target: ReadTarget::Readout(Readout::Bell), day: Some("20261016".into()) })
        );
        assert_eq!(parse_tool_call("read_device", r#"{"what":"secrets","day":null}"#, tools), None);
        assert_eq!(
            parse_tool_call("change_setting", r#"{"setting":"signal_text_only","value":"on"}"#, tools),
            Some(ToolCall::Change(Setting::TextOnly, Value::Switch(true)))
        );
        assert_eq!(parse_tool_call("change_setting", r#"{"setting":"spending_limit","value":"7"}"#, tools), None);
        assert_eq!(
            parse_tool_call("change_setting", r#"{"setting":"finance_rundown","value":"off"}"#, tools),
            None,
            "a rundown is no setting: the rundowns' own function changes one"
        );
        assert_eq!(
            parse_tool_call("change_rundowns", r#"{"request":" finance off "}"#, tools),
            Some(ToolCall::Device(Act::Rundowns(Words(" finance off ".into()))))
        );
        assert_eq!(parse_tool_call("change_rundowns", r#"{"request":" "}"#, tools), None);
        assert_eq!(
            to_act_line(&Act::Rundowns(Words("f1".into()))),
            "chat: the conversation has the device do Rundowns(Words)"
        );
        assert_eq!(parse_tool_call("change_setting", r#"{"setting":"lock_on_wake","value":"on"}"#, tools), None);
    }

    #[test]
    fn action_acts() {
        let act = |action: &str, value: Option<&str>| parse_act(action, value);
        let table = [
            (act("open_photo_camera", None), Some(Act::Open(Destination::PhotoCamera))),
            (act("open_colors", None), Some(Act::Open(Destination::Colours))),
            (act("open_days", None), Some(Act::Open(Destination::Days))),
            (act("open_web", None), Some(Act::Open(Destination::Web))),
            (act("open_rundowns", None), Some(Act::Open(Destination::Rundowns))),
            (act("open_calls", None), Some(Act::Open(Destination::Calls))),
            (act("set_brightness", Some(" 40 % ")), Some(Act::Brightness(40))),
            (act("set_brightness", Some("much")), None),
            (act("set_theme", Some(" DECK ")), Some(Act::Theme(Theme::Deck))),
            (act("set_colors", Some("c")), Some(Act::Colours(2))),
            (act("open_reminders", None), Some(Act::Open(Destination::Bell))),
            (act("open_passphrase", None), Some(Act::Open(Destination::Passphrase))),
            (act("open_sim_pin", None), Some(Act::Open(Destination::SimPin))),
            (act("open_tunnel", None), Some(Act::Open(Destination::Tunnel))),
            (act("open_debug", None), Some(Act::Open(Destination::Debug))),
            (act("open_settings", None), Some(Act::Enter(Menu::Settings))),
            (act("open_signal_settings", None), Some(Act::Enter(Menu::Signal))),
            (act("open_camera_menu", None), Some(Act::Enter(Menu::Camera))),
            (act("set_clock_offset", Some(" utc+02:00 ")), Some(Act::Offset(120))),
            (act("set_clock_offset", Some("-03:30")), Some(Act::Offset(-210))),
            (act("set_clock_offset", Some("+02:10")), None),
            (act("set_clock_offset", None), None),
            (act("set_alarm", Some("7:30")), None),
            (act("delete_everything", None), None),
        ];
        for (actual, expected) in table {
            assert_eq!(actual, expected);
        }
        assert_eq!(
            [Menu::Microphone, Menu::Robot, Menu::MobileData, Menu::UsbDrive].map(Menu::to_name),
            ["MIC", "ROBOT", "MOBILE DATA", "USB DRIVE"],
            "a menu by the name of its line"
        );
    }

    #[test]
    fn ring_calls() {
        let call = |name, arguments| parse_tool_call(name, arguments, all_on());
        assert!(
            matches!(
                call("set_reminder", r#"{"on":null,"at":"7:30","in_seconds":null,"text":null}"#),
                Some(ToolCall::Bell(Change::Set { .. }))
            ),
            "the bell reads the calls of its own functions"
        );
        assert_eq!(
            call("cancel_reminder", r#"{"which":" dentist "}"#),
            Some(ToolCall::Bell(Change::Cancel("dentist".into())))
        );
        assert_eq!(call("cancel_reminder", r#"{"which":" "}"#), None);
        let without_device = all_on().with(Tool::Device, false);
        assert_eq!(parse_tool_call("cancel_reminder", r#"{"which":"all"}"#, without_device), None);
        assert!(
            ToolCall::Bell(Change::Cancel("all".into())).needs_answer(),
            "the answer names what was set or cancelled"
        );
    }

    #[test]
    fn answer_needed() {
        let tone = ToolCall::Memory(MemoryCall::Tone(Vec::new()));
        assert!(!tone.needs_answer() && ToolCall::Search(String::new()).needs_answer());
        assert!(ToolCall::Read { target: ReadTarget::Notes, day: None }.needs_answer());
        assert!(ToolCall::Forget("thread".into()).needs_answer());
        assert!(!ToolCall::Change(Setting::Limit, Value::Step(None)).needs_answer());
        assert!(ToolCall::ReadSignal(None).needs_answer());
        assert!(ToolCall::DraftSignal { conversation: "Anna".into(), text: "Hi".into() }.needs_answer());
    }
}
