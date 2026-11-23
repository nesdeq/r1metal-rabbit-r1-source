//! Renders screens to PNG files, and a summary to its pages, so layout and motion can be judged without a device.
// Screens are set up by the interface's setters for what they set, whether or not each says the screen changed.
#![allow(unused_must_use)]

use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::base::time::{NANOS_PER_MS, NANOS_PER_SECOND};
use r1_funcore::cell::modem::Status as ModemStatus;
use r1_funcore::domain::bell::Bell;
use r1_funcore::domain::bell::state::Report as BellReport;
use r1_funcore::domain::calls::{Ending, Member, Phase as CallPhase, View as CallView};
use r1_funcore::domain::camera::{Development, Outcome, Review, Viewfinder};
use r1_funcore::domain::cause::Cause;
use r1_funcore::domain::chat::InputKind;
use r1_funcore::domain::chat::state::Report as ChatReport;
use r1_funcore::domain::chat::transcript::{Entry, EntryKind};
use r1_funcore::domain::clock::TimeAnswer;
use r1_funcore::domain::cost::{Figure, to_figures};
use r1_funcore::domain::device::{self, Part};
use r1_funcore::domain::dialogue::Turn;
use r1_funcore::domain::gallery::{Kind, Label, Stamp, Thumbnail};
use r1_funcore::domain::magic::{Progress, Stage};
use r1_funcore::domain::memos::state::Report as NotesReport;
use r1_funcore::domain::memos::{Memo, Request};
use r1_funcore::domain::news::request::{Change, Report as RundownsReport};
use r1_funcore::domain::news::silent::Silent;
use r1_funcore::domain::news::state::Report as EditorReport;
use r1_funcore::domain::news::topics::Topics;
use r1_funcore::domain::paper::pages::to_pages;
use r1_funcore::domain::paper::sheet::{self, Subject};
use r1_funcore::domain::pokedex::entry::Entry as PokedexEntry;
use r1_funcore::domain::pokedex::state::Article;
use r1_funcore::domain::router::{Report as RouterReport, Standing};
use r1_funcore::domain::rundown;
use r1_funcore::domain::settings::Settings;
use r1_funcore::domain::shelf::Shelf;
use r1_funcore::domain::signal::list::{CallRow, Contactable, ConversationView, Summary};
use r1_funcore::domain::signal::state::{LinkStatus, LinkStep, Report as SignalReport, SignalView, ThreadStatus};
use r1_funcore::domain::tally::Tally;
use r1_funcore::domain::voice::Phase;
use r1_funcore::domain::web::Page;
use r1_funcore::gfx::icon::{Icon, Point};
use r1_funcore::gfx::mask::Mask;
use r1_funcore::gfx::palette::{self, Roles};
use r1_funcore::gfx::theme::{THEMES, Theme};
use r1_funcore::gfx::{self, Surface};
use r1_funcore::hw::camera::develop;
use r1_funcore::hw::power::battery::{Cell, Charger, Figures, Sample, Supply};
use r1_funcore::hw::power::usage::Usage;
use r1_funcore::input::Event;
use r1_funcore::input::pose::G;
use r1_funcore::logs::writer;
use r1_funcore::openai::ledger::Ledger;
use r1_funcore::signal::address::{Peer, ServiceId};
use r1_funcore::signal::call::Kind as CallKind;
use r1_funcore::signal::conversation::{Entry as Message, Reaction as MessageReaction, Status};
use r1_funcore::signal::device::Status as CallStatus;
use r1_funcore::storage::progress::Progress as DataProgress;
use r1_funcore::ui::{Robot, Ui};
use r1_funcore::wifi::known;
use r1_funcore::wifi::radio::Way;
use r1_funcore::wifi::scan::{Network, Security};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::time::Duration;

const WIDTH: usize = 480;
const HEIGHT: usize = 640;
const RELEASE: &str = "4.19.191-r1metal";
const ADDRESS: [u8; 6] = [0x02, 0, 0, 0, 0, 0x01];
const MEMINFO: &str = "MemTotal:        3956404 kB\n";
const STORAGE_BYTES: (u64, u64) = (107 << 30, 104 << 30);
const ROLES: Roles = Roles::of(&palette::PALETTES[palette::STANDARD]);
const FRAME_NS: u64 = r1_funcore::gfx::refresh::REFRESH_NS;
const DEBUG_FRAMES: u64 = 90;
// As measured on the device.
const IDLE_TAIL_NS: u64 = 100_000_000;
const DRAW_NS: u64 = 3_000_000;
const SHEET_BOX: usize = 240;
const STRIP_SHRINK: usize = 2;
const SHRINK_VARIABLE: &str = "SHOT_SHRINK";
const THEME_VARIABLE: &str = "SHOT_THEME";
// Thursday 2026-10-08 22:41.
const SAMPLE_TIME: u64 = 1_791_499_260;
// Friday 2026-10-09 07:00.
const SAMPLE_ALARM: u64 = 1_791_529_200;
// Friday 2026-11-20 09:00.
const SAMPLE_REMINDER: u64 = 1_795_165_200;
const SAMPLE_TIMER_S: u64 = 300;
// Lights three of the microphone's four blocks.
const SAMPLE_PEAK: u32 = 2000;
const SAMPLE_POKEMON: u32 = 25;
const LISTENING: &str = "listening";
const TRANSLATOR: &str = "translator";
const POKEDEX: &str = "pokedex";
const WEB_PAGE: &str = "web page";
// Taller than the room a page is read in, with links in a heading's line, in a paragraph and in items.
const PAGE: &str = "# Harbour ferry returns after two years\n\nThe ferry across the old harbour **\\[1]** runs again from Monday, the port authority **\\[2]** said. It crosses every twenty minutes between six in the morning and midnight, and a ride costs 2 * 1.50 for a return.\n\n## What changes\n\n- The new pier at the fish market **\\[3]**\n\n- Bicycles ride free **\\[4]**\n\n- Night crossings on weekends **\\[5]**\n\n## Background\n\nThe line closed when the east quay was rebuilt. The works took a year longer than planned, as the council's report **\\[6]** sets out, and cost a third more.";
const PAGE_LINKS: usize = 6;
const SAMPLE_LEVEL: u8 = 64;
const SAMPLE_LOW: u8 = 5;
const ROBOT_BOX: usize = 120;
const ROBOT_ROW: usize = 12;
const REHEARSAL_NS: u64 = NANOS_PER_SECOND;
const REHEARSAL_SPAN_NS: u64 = 10 * NANOS_PER_SECOND;
const SETTLE_FRAMES: usize = 240;
// As the device's gallery takes for photos.
const THUMBNAIL_DELAY_NS: u64 = 180_000_000;
const PICTURE_DELAY_NS: u64 = 120_000_000;
const PHOTO_SIZE: (usize, usize) = (develop::SIDE, develop::SIDE);
// The stream and the photo it takes in two tints, so that the photo shows as it unfolds over the frame it stopped on.
const STREAM_TINT: usize = 1;
const PHOTO_TINT: usize = 0;
// Taken at the sample time.
const TAKEN_PHOTO: &str = "20261008_224100";
const VIDEO_SIZES: [(usize, usize); 2] = [(720, 1280), (1280, 720)];
const NOTES: [(&str, u64, Option<&str>, Option<&str>); 3] = [
    (
        "20261012_190328",
        42,
        Some("Buy milk. And call Anna about Friday."),
        Some("A shopping list.\n\n- **Anna**: call"),
    ),
    (
        "20261011_083000",
        2562,
        Some("The team met to plan the October release and agreed to ship sound first."),
        Some(MEETING),
    ),
    ("20261010_120000", 5, None, None),
];
// Taller than the room a text is read in.
const MEETING: &str = "## October release\n\nThe team planned the October release of the firmware and agreed to ship **sound** first.\n\n## Decided\n\n- Sound goes out before the gauge\n- **Anna** writes the notes for the release\n- The gallery keeps its two ways of showing a photo\n\n## Open\n\n- Who measures the current with the microphone on\n- Whether the translator keeps its voice\n\n---\n\n## Next\n\n1. Flash the build on Friday\n2. A day off the cable over the weekend\n3. Meet again on Monday at ten\n\n## Risks\n\nNobody has listened to a recording on the device yet, and the speaker *may be too quiet* for a room, as ***Ben*** said.";
const SESSION_DELAY_NS: u64 = 900_000_000;
const CHARACTER_NS: u64 = 45_000_000;
const REMARKS: [(&str, &str); 4] = [
    ("", "Hello, I am the translator of this device. Speak in your own language, one at a time."),
    ("Entschuldigung, wo ist der Bahnhof?", "Excuse me, where is the station?"),
    ("Straight on, then the second street on the left.", "Geradeaus, dann die zweite Stra\u{DF}e links."),
    ("Vielen Dank!", "Thank you very much!"),
];
const NEWS_DAY: &str = "20261013";
// The two a new device has, one of them off, and one the owner keeps under an icon of its own, a feed of which gave no story.
const RUNDOWNS: &str = "rundown news on\nrundown finance off\nrundown motorsport on\nabout motorsport Formula 1 and MotoGP: races, results and the paddock, in German\nicon motorsport sports_motorsports\nfeed motorsport https://www.motorsport.example/rss/f1/news/\nfeed motorsport https://www.autosport.example/rss/feed/f1\nfeed motorsport motogp.example https://news.example/rss/search?q=site:motogp.example\n";
const SILENT_FEEDS: &str = "feed motorsport motogp.example https://news.example/rss/search?q=site:motogp.example\n";
// The rundown a request asks to delete.
const DELETED: &str = "motorsport";
const RUNDOWNS_NAME: &str = "rundown";
const FEATURES_NEWS: &str = "features news";
const RUNDOWN_DELETION: &str = "rundown deletion";
// As the device's radio takes for its first pass.
const SCAN_DELAY_NS: u64 = 2_500_000_000;
const NETWORKS: [(&str, i32, Security); 10] = [
    ("Workshop", -47, Security::Password { has_sae: false }),
    ("Guests", -58, Security::Open),
    ("printer setup 4F2A", -66, Security::Open),
    ("Upstairs", -71, Security::Password { has_sae: true }),
    ("Campus", -74, Security::Other),
    ("a name of all the thirty-two let", -79, Security::Password { has_sae: true }),
    ("Garden", -83, Security::Password { has_sae: false }),
    ("Bakery", -86, Security::Open),
    ("Neighbours", -88, Security::Password { has_sae: false }),
    ("Far Away", -92, Security::Password { has_sae: false }),
];
const KNOWN_NETWORKS: [(&str, bool); 3] = [("Upstairs", true), ("Guests", false), ("Seaside", true)];
// A SIM that waits for its PIN, every try left.
const MODEM: ModemStatus = ModemStatus::Pin(3);
// More than any level of the tree has.
const MAX_STEPS: usize = 32;

#[derive(Clone, Copy)]
enum Step {
    Open(&'static str),
    // A line chosen and left to rest on, unclicked.
    Stand(&'static str),
    Lines(usize),
    // That many lines back, then a click.
    Before(usize),
}

use Step::{Before, Lines, Open, Stand};

const TO_RUNDOWNS: [Step; 4] = [Open("SETTINGS"), Open("FEATURES"), Open("NEWS"), Open("RUNDOWNS")];
// The newest call of the log, called back with sound alone, or with this side's picture.
const TO_AUDIO_CALL: [Step; 3] = [Open("CALLS"), Lines(0), Lines(0)];
const TO_VIDEO_CALL: [Step; 3] = [Open("CALLS"), Lines(0), Lines(1)];
const CALL: &str = "call";
// A call that rang, answered by a click on the window's first line.
const ANSWERED_CALL: &str = "call answered";
// A video call whose camera failed, which a note on its screen tells.
const CAMERA_FAILED: &str = "call video camera failed";
// A call's ending and a note show for a second: what is rendered of them stands half of that.
const ENDING_FRAMES: u64 = NANOS_PER_SECOND / 2 / FRAME_NS;

const SCREENS: [(&str, &[Step]); 101] = [
    ("home", &[]),
    ("home alarm", &[]),
    ("home alarm offline", &[]),
    ("home mobile", &[]),
    ("bell", &[Open("BELL")]),
    ("bell listening", &[Open("BELL")]),
    ("bell entry", &[Open("BELL"), Lines(1)]),
    ("bell rings", &[]),
    ("bell chosen", &[Stand("BELL")]),
    (FEATURES_NEWS, &[Open("SETTINGS"), Open("FEATURES"), Open("NEWS")]),
    ("rundowns", &TO_RUNDOWNS),
    ("rundowns listening", &TO_RUNDOWNS),
    ("rundown", &[TO_RUNDOWNS[0], TO_RUNDOWNS[1], TO_RUNDOWNS[2], TO_RUNDOWNS[3], Lines(3)]),
    (RUNDOWN_DELETION, &TO_RUNDOWNS),
    ("settings", &[Open("SETTINGS")]),
    ("notes", &[Open("MIC"), Open("AUDIO NOTE")]),
    ("note", &[Open("MIC"), Open("AUDIO NOTE"), Lines(1)]),
    ("power", &[Open("SETTINGS"), Open("POWER")]),
    ("pictures", &[Open("CAMERA"), Open("PICTURES")]),
    ("themes", &[Open("SETTINGS"), Open("THEMES")]),
    ("colors", &[Open("SETTINGS"), Open("COLORS")]),
    ("chat", &[Open("CHAT")]),
    ("chat listening", &[Open("CHAT")]),
    ("pokedex listening", &[Open("POKEDEX")]),
    ("pokedex page listening", &[Open("POKEDEX")]),
    ("web", &[Open("WEB")]),
    ("web page", &[Open("WEB")]),
    ("web page listening", &[Open("WEB")]),
    ("web page keys", &[Open("WEB")]),
    ("camera", &[Open("CAMERA")]),
    ("cost", &[Open("SETTINGS"), Open("COST")]),
    ("features web", &[Open("SETTINGS"), Open("FEATURES"), Open("WEB")]),
    ("features signal", &[Open("SETTINGS"), Open("FEATURES"), Open("SIGNAL")]),
    ("setting staged", &[Open("SETTINGS"), Open("FEATURES"), Open("SIGNAL"), Open("AUTO MODE STOPS")]),
    ("setting set", &[Open("SETTINGS"), Open("FEATURES"), Open("SIGNAL"), Open("AUTO MODE STOPS"), Lines(1)]),
    ("about", &[Open("SETTINGS"), Open("ABOUT")]),
    ("wifi", &[Open("SETTINGS"), Open("WIFI")]),
    ("network", &[Open("SETTINGS"), Open("WIFI"), Lines(1)]),
    ("password", &[Open("SETTINGS"), Open("WIFI"), Lines(2)]),
    ("mobile data", &[Open("SETTINGS"), Open("MOBILE DATA")]),
    ("sim pin", &[Open("SETTINGS"), Open("MOBILE DATA"), Open("SIM PIN")]),
    ("tunnel", &[Open("SETTINGS"), Open("MOBILE DATA"), Open("TUNNEL")]),
    ("usb drive", &[Open("SETTINGS"), Open("USB DRIVE")]),
    ("brightness", &[Open("SETTINGS"), Open("BRIGHTNESS")]),
    ("volume", &[Open("SETTINGS"), Open("VOLUME")]),
    ("sounds", &[Open("SETTINGS"), Open("VOLUME"), Lines(0)]),
    ("clock", &[Open("SETTINGS"), Open("CLOCK")]),
    ("clock hour", &[Open("SETTINGS"), Open("CLOCK"), Lines(0)]),
    ("clock month", &[Open("SETTINGS"), Open("CLOCK"), Lines(0), Lines(0), Lines(0), Lines(0)]),
    ("translations", &[Open("MIC"), Open("TRANSLATOR")]),
    ("translator", &[Open("MIC"), Open("TRANSLATOR"), Lines(0)]),
    ("news", &[Open("NEWS")]),
    ("reader", &[Open("NEWS"), Lines(0), Lines(0)]),
    ("link", &[Open("SIGNAL")]),
    ("signal", &[Open("SIGNAL")]),
    ("conversation", &[Open("SIGNAL"), Lines(1)]),
    ("conversation listening", &[Open("SIGNAL"), Lines(1)]),
    ("signal settings", &[Open("SIGNAL"), Lines(0)]),
    ("calls", &[Open("CALLS")]),
    ("calls new", &[Open("CALLS"), Before(1)]),
    ("call choice", &[Open("CALLS"), Lines(0)]),
    ("call calling", &TO_AUDIO_CALL),
    ("call ringing", &TO_AUDIO_CALL),
    ("call connecting", &TO_AUDIO_CALL),
    ("call talking", &TO_AUDIO_CALL),
    ("call muted", &TO_AUDIO_CALL),
    ("call reconnecting", &TO_AUDIO_CALL),
    ("call ended", &TO_AUDIO_CALL),
    ("call failed", &TO_AUDIO_CALL),
    ("call group", &TO_AUDIO_CALL),
    ("call video calling", &TO_VIDEO_CALL),
    ("call video", &TO_VIDEO_CALL),
    ("call video wide", &TO_VIDEO_CALL),
    ("call video 3x4", &TO_VIDEO_CALL),
    ("call video 4x3", &TO_VIDEO_CALL),
    ("call group video", &TO_VIDEO_CALL),
    (CAMERA_FAILED, &TO_VIDEO_CALL),
    ("call incoming", &[]),
    (ANSWERED_CALL, &[]),
    ("features chosen", &[Open("SETTINGS"), Stand("FEATURES")]),
    ("wifi chosen", &[Open("SETTINGS"), Stand("WIFI")]),
    ("mobile data chosen", &[Open("SETTINGS"), Stand("MOBILE DATA")]),
    ("brightness chosen", &[Open("SETTINGS"), Stand("BRIGHTNESS")]),
    ("volume chosen", &[Open("SETTINGS"), Stand("VOLUME")]),
    ("themes chosen", &[Open("SETTINGS"), Stand("THEMES")]),
    ("colors chosen", &[Open("SETTINGS"), Stand("COLORS")]),
    ("clock chosen", &[Open("SETTINGS"), Stand("CLOCK")]),
    ("passphrase chosen", &[Open("SETTINGS"), Stand("PASSPHRASE")]),
    ("usb drive chosen", &[Open("SETTINGS"), Stand("USB DRIVE")]),
    ("cost chosen", &[Open("SETTINGS"), Stand("COST")]),
    ("power chosen", &[Open("SETTINGS"), Stand("POWER")]),
    ("about chosen", &[Open("SETTINGS"), Stand("ABOUT")]),
    ("photo chosen", &[Open("CAMERA"), Stand("PHOTO")]),
    ("pictures chosen", &[Open("CAMERA"), Stand("PICTURES")]),
    ("notes chosen", &[Open("MIC"), Stand("AUDIO NOTE")]),
    ("translations chosen", &[Open("MIC"), Stand("TRANSLATOR")]),
    ("signal chosen", &[Stand("SIGNAL")]),
    ("calls chosen", &[Stand("CALLS")]),
    ("news chosen", &[Stand("NEWS")]),
    ("web chosen", &[Stand("WEB")]),
    ("camera chosen", &[Stand("CAMERA")]),
];
const EVERYTHING: &str = "EVERYTHING";
const SETTINGS: &str = "SETTINGS";
const TALLY: Tally = Tally {
    photos: 24,
    photos_today: 3,
    videos: 6,
    notes: NOTES.len(),
    newest_note_s: Some(NOTES[0].1),
    has_edition: true,
};
const CHAT: [(EntryKind, &str); 5] = [
    (EntryKind::User(InputKind::Voice), "What is the weather tomorrow?"),
    (EntryKind::Robot, "Rain from noon, **12 degrees**. Take the umbrella, the wind comes from the west."),
    (EntryKind::Sources, "[Weather](https://example.org/weather)"),
    (EntryKind::User(InputKind::Text), "And on Saturday?"),
    (EntryKind::Robot, "Sun, 15 degrees. A good day for the market."),
];
// Whether the owner's, the words, the seconds after the clock's time, and the emoji it was reacted to with.
type FakeMessage = (bool, &'static str, u64, &'static [&'static str]);
const SIGNAL_THREADS: [(u8, &str, &[FakeMessage]); 3] = [
    (
        1,
        "Anna",
        &[
            (false, "Are we still on for lunch? \u{1F35C}", 0, &[]),
            (true, "Yes, 12:30 at the market hall \u{1F44D}\u{1F3FD}", 60, &["\u{2764}\u{FE0F}"]),
            (false, "Perfect, see you there! \u{1F60A}\u{2764}\u{FE0F}", 240, &[]),
        ],
    ),
    (
        2,
        "Boris",
        &[
            (true, "The parcel is at the door.", 0, &["\u{1F44D}", "\u{1F44D}", "\u{1F64F}"]),
            (false, "Thanks, got it. \u{8B1D}\u{8B1D}\u{4F60} \u{1F4E6}", 30, &[]),
        ],
    ),
    (
        3,
        "Clara \u{1F1E9}\u{1F1EA}",
        &[(false, "Happy birthday! \u{1F389}\u{1F382} \u{304A}\u{3081}\u{3067}\u{3068}\u{3046}", 0, &[])],
    ),
];
// A group of the owner's, and a contact never written to.
const CLIMBING: Peer = Peer::Group([9; 32]);
const CLIMBING_NAME: &str = "Climbing";
const DANA: u8 = 4;
const DANA_NAME: &str = "Dana";
// Each call of the log: with the contact of SIGNAL_THREADS of that id, or the group where none; its kind, whether the owner began it, how it came out, the seconds before the clock's time, and whether it was seen.
type FakeCall = (Option<u8>, CallKind, bool, CallStatus, u64, bool);
const CALL_LOG: [FakeCall; 7] = [
    (Some(1), CallKind::Audio, false, CallStatus::Missed, 600, false),
    (Some(2), CallKind::Video, true, CallStatus::Accepted, 3_600, true),
    (None, CallKind::Group, false, CallStatus::Generic, 7_200, false),
    (Some(3), CallKind::Video, false, CallStatus::Declined, 90_000, true),
    (Some(1), CallKind::Audio, true, CallStatus::Missed, 100_000, true),
    (Some(2), CallKind::Audio, false, CallStatus::Accepted, 200_000, true),
    (None, CallKind::Group, true, CallStatus::Joined, 300_000, true),
];
// Who is in a group's call beside the owner: a name, whether one speaks, whether one is muted; more than a page of lines holds.
const MEMBERS: [(&str, bool, bool); 7] = [
    ("Anna", true, false),
    ("Boris", false, true),
    ("Clara", false, false),
    (DANA_NAME, false, false),
    ("Emil", false, true),
    ("Frida", false, false),
    ("Gustav", false, false),
];
// The far side's picture by the screen that shows it, width and height: as a phone held upright sends it, wide, and of the camera's shape upright and level; the first for a screen not named.
const FAR_PICTURES: [(&str, (usize, usize)); 5] = [
    ("call video", (720, 1280)),
    ("call video wide", (1280, 720)),
    ("call video 3x4", (480, 640)),
    ("call video 4x3", (640, 480)),
    ("call group video", (480, 640)),
];
const FAR_TINT: usize = 3;
const LINK_ADDRESS: &str = "sgnl://linkdevice?uuid=Hgz6LaxkHDNfnFbjl1BhBw%3D%3D&pub_key=BQ%2B8m0yYb7xQvQ3w9Jm0C1y0ZJp8Yq2cFzW3n5d9X1oF&capabilities=nopni%2Cnopni2";

const COST_DELAY_NS: u64 = 150_000_000;
const LEDGER: &str = "2026-10 15 6390 15360 6585 632580 3 240 352380 18000 3 2100 2 70000 1 50000 2 30000 12 40000 5 50000 4 900 1 800 1 6000 1 5000 2 400 3 3000 2 4000\n2026-09 39 16614 39936 17121 916188 0 0 916188 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
const THIS_MONTH: (u32, u8) = (2026, 10);
const PIXEL_BLOCK: usize = 8;
const USAGE: &str = "usage: shot debug OUT.png | shot themes FOLDER | shot turned FOLDER DEGREES | shot motion FOLDER STEP_MS FRAMES | shot icons OUT.png FILE.svg... | shot strip OUT.png FROM_MS STEP_MS FRAMES [MS:EVENT]... | shot roll OUT.png pictures|videos COUNT FROM_MS STEP_MS FRAMES [MS:EVENT]... | shot finder OUT.png FROM_MS STEP_MS FRAMES [MS:EVENT]... | shot wifi OUT.png COUNT FROM_MS STEP_MS FRAMES [MS:EVENT]... | shot cost OUT.png FROM_MS STEP_MS FRAMES [MS:EVENT]... | shot clock OUT.png FROM_MS STEP_MS FRAMES [MS:EVENT]... | shot notes OUT.png FROM_MS STEP_MS FRAMES [MS:EVENT]... | shot news OUT.png FILE.md FROM_MS STEP_MS FRAMES [MS:EVENT]... | shot talk OUT.png FROM_MS STEP_MS FRAMES [MS:EVENT]... | shot robot OUT.png FROM_MS SECONDS | shot gesture OUT.png NUMBER EVERY [mirrored] | shot budget FROM_MS SECONDS STIR_S | shot summary OUT.pdf FILE.md NAME [SECONDS]\n\
                     events: up, down (wheel), press, release (button), touch:X:Y, lift, plug, unplug, low (battery), wake, pickup and tilt:DEGREES (the accelerometer's), of the pixel cam joined, reached, sent, answered, kept, refused, of the camera taken, merging, finishing, developed and art, and of the asking for the time set, notime; the times of a roll, of the photo viewfinder, of the list of networks, of the cost, of the clock's screen, of the notes, of the news and of the translator count from when it has opened; a summary with SECONDS is a note's, one without a conversation's; what is filmed from FROM_MS stands in the theme SHOT_THEME names, stage, dial, deck, terminal, companion or rail, Stage where it names none, each frame shrunk by SHOT_SHRINK, 2 where it names none";

struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Canvas { width, height, pixels: vec![0; width * height] }
    }

    fn surface(&mut self) -> Surface<'_> {
        Surface::new(&mut self.pixels, self.width as u32, self.height as u32, self.width, PixelFormat::XRGB8888)
            .unwrap()
    }

    fn paste(&mut self, other: &Canvas, x: usize, y: usize, shrink: usize) {
        let area = gfx::Rect::new(x as i32, y as i32, (other.width / shrink) as i32, (other.height / shrink) as i32);
        self.surface().shrink(area, other.width, &other.pixels);
    }

    fn save(&self, path: &str) {
        let rgb: Vec<u8> = self.pixels.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
        let mut encoder =
            png::Encoder::new(BufWriter::new(File::create(path).unwrap()), self.width as u32, self.height as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&rgb).unwrap();
    }
}

fn facts() -> Vec<(Part, Option<String>)> {
    vec![
        (Part::Kernel, Some(RELEASE.into())),
        (Part::Screen, Some("480x640 rgbx 3 pages".into())),
        (Part::Input, Some("mtk-kpd mtk-pmic-keys och1970_holl_key mtk-tpd".into())),
        (Part::Data, Some(DataProgress::Mounted(STORAGE_BYTES.0).to_word())),
        (Part::Logs, Some(writer::Status::On(NEWS_DAY.into()).to_word())),
        (Part::Usb, Some("musb-hdrc".into())),
    ]
}

fn themed_ui(theme: Theme) -> Ui {
    let mut ui = Ui::new(WIDTH as i32, HEIGHT as i32, PixelFormat::XRGB8888, facts());
    ui.set_settings(Settings { theme, ..ui.settings() });
    ui
}

fn debug(out: &str) {
    let mut ui = themed_ui(Theme::Stage);
    ui.set_pacing(true);
    click(&mut ui, 0);
    ui.set_rundown(rundown::Status::Answered);
    for frame in 0..DEBUG_FRAMES {
        let start_ns = frame * FRAME_NS;
        ui.tick(start_ns);
        ui.note_frame_presented(start_ns, start_ns + DRAW_NS, start_ns + FRAME_NS);
    }
    let mut picture = Canvas::new(WIDTH, HEIGHT);
    ui.render_over(&mut picture.surface(), DEBUG_FRAMES * FRAME_NS, None);
    picture.save(out);
}

fn icons(out: &str, files: &[&str]) {
    let mut sheet = Canvas::new(SHEET_BOX * files.len().max(1), SHEET_BOX);
    for (index, file) in files.iter().enumerate() {
        let svg = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{file}: {e}"));
        let icon = Icon::from_svg(&svg).unwrap_or_else(|e| panic!("{file}: {e:?}"));
        let mut mask = Mask::new(SHEET_BOX, SHEET_BOX);
        let place = |point: &Point| (point.x * SHEET_BOX as f32, point.y * SHEET_BOX as f32);
        for contour in icon.contours() {
            mask.outline(contour.iter().map(place));
        }
        sheet.surface().cover((index * SHEET_BOX) as i32, 0, &mut mask, ROLES.text, ROLES.background);
    }
    sheet.save(out);
}

fn event(word: &str) -> Event {
    let fields: Vec<&str> = word.split(':').collect();
    match fields[..] {
        ["up"] => Event::Wheel(-1),
        ["down"] => Event::Wheel(1),
        ["press"] => Event::PttDown,
        ["release"] => Event::PttUp,
        ["touch", x, y] => Event::TouchAt { x: x.parse().unwrap(), y: y.parse().unwrap() },
        ["lift"] => Event::TouchUp,
        _ => panic!("unknown event {word:?}\n{USAGE}"),
    }
}

enum Cue {
    Input(Event),
    Battery(u8, Supply),
    Wake,
    Lift,
    Tilt(i32),
    Draw(Stage),
    Keep,
    Refuse,
    Taken,
    Develop(Development),
    Developed,
    Art,
    Time(bool),
}

fn cue(word: &str) -> Cue {
    match word {
        "plug" => Cue::Battery(SAMPLE_LEVEL, Supply::Charging),
        "unplug" => Cue::Battery(SAMPLE_LEVEL, Supply::Battery),
        "low" => Cue::Battery(SAMPLE_LOW, Supply::Battery),
        "wake" => Cue::Wake,
        "pickup" => Cue::Lift,
        "joined" => Cue::Draw(Stage::Joined),
        "reached" => Cue::Draw(Stage::Reached),
        "sent" => Cue::Draw(Stage::Sent),
        "answered" => Cue::Draw(Stage::Answered),
        "kept" => Cue::Keep,
        "refused" => Cue::Refuse,
        "taken" => Cue::Taken,
        "merging" => Cue::Develop(Development::Merging),
        "finishing" => Cue::Develop(Development::Finishing),
        "developed" => Cue::Developed,
        "art" => Cue::Art,
        "set" => Cue::Time(true),
        "notime" => Cue::Time(false),
        _ => match word.split_once(':') {
            Some(("tilt", degrees)) => Cue::Tilt(degrees.parse().unwrap_or_else(|_| panic!("{word:?}\n{USAGE}"))),
            _ => Cue::Input(event(word)),
        },
    }
}

impl Cue {
    fn apply(self, ui: &mut Ui, at: u64) {
        match self {
            Cue::Input(event) => drop(ui.handle(event, at)),
            Cue::Battery(level, supply) => drop(ui.set_battery(Some(level), supply, at)),
            Cue::Wake => drop(ui.note_wake(at, false)),
            Cue::Lift => drop(ui.take_lift(true)),
            Cue::Tilt(degrees) => {
                let (x, y) = gravity(degrees);
                ui.set_tilt(x, y);
            }
            Cue::Draw(stage) => drop(ui.set_drawing(Some(Progress { stage, since_ns: at }))),
            Cue::Keep => {
                ui.set_drawing(None);
                ui.note_pixels_kept(at);
            }
            Cue::Refuse => {
                ui.set_drawing(None);
                ui.note_pixels_failed(Cause::PixelsRefused, at);
            }
            Cue::Taken => ui.note_taken(TAKEN_PHOTO.into()),
            Cue::Develop(development) => drop(ui.take_development(development)),
            Cue::Developed => {
                if let Some(Viewfinder { size, .. }) = ui.visible_camera() {
                    let picture = fake_picture(PHOTO_TINT, size, false);
                    ui.take_review(Review { photo: TAKEN_PHOTO.into(), picture: picture.words, width: picture.width });
                }
                ui.take_development(Development::Done);
            }
            // As the main loop asks the darkroom for the art once the pixel cam has kept it.
            Cue::Art => {
                if let Some((photo, sight)) = ui.review_art(&[TAKEN_PHOTO.into()]) {
                    let picture = fake_picture(PHOTO_TINT, (sight.side, sight.side), true);
                    ui.take_review(Review { photo, picture: picture.words, width: picture.width });
                }
            }
            Cue::Time(is_right) => {
                let answer = if is_right {
                    TimeAnswer::Answered { is_moved: true }
                } else {
                    TimeAnswer::Failed(Cause::NoNetwork)
                };
                ui.take_time_answer(answer, at);
            }
        }
    }
}

// Rings and a cross show cropping and scaling; blocks stand for pixel art.
fn fake_picture(item: usize, (width, height): (usize, usize), is_pixel_art: bool) -> gfx::Picture {
    const TINTS: [(f64, f64, f64); 6] = [
        (90.0, 110.0, 140.0),
        (140.0, 100.0, 80.0),
        (80.0, 130.0, 100.0),
        (130.0, 90.0, 130.0),
        (120.0, 120.0, 80.0),
        (80.0, 120.0, 140.0),
    ];
    let (red, green, blue) = TINTS[item % TINTS.len()];
    let block = if is_pixel_art { (PIXEL_BLOCK * width / WIDTH).max(2) } else { 1 };
    let words = (0..width * height).map(|at| {
        let (x, y) = (at % width / block * block + block / 2, at / width / block * block + block / 2);
        let (u, v) = (x as f64 / width as f64 - 0.5, y as f64 / height as f64 - 0.5);
        let aspect = width as f64 / height as f64;
        let distance = ((u * aspect.max(1.0)).powi(2) + (v / aspect.min(1.0)).powi(2)).sqrt();
        let line = (distance * 12.0).fract() < 0.08 || (u.abs() - v.abs()).abs() < 0.006;
        let shade = if line { 1.6 } else { 0.55 + 0.45 * (u + v + 1.0) / 2.0 };
        let channel = |value: f64| (value * shade).min(255.0) as u32;
        channel(red) << 16 | channel(green) << 8 | channel(blue)
    });
    gfx::Picture { words: words.collect(), width }
}

struct FakeGallery {
    kind: Kind,
    count: usize,
    thumbnail_request: Option<(u32, usize, u64)>,
    last_page: Option<(u32, usize)>,
    picture_request: Option<((usize, u32, bool), u64)>,
    last_picture: Option<(usize, u32, bool)>,
}

impl FakeGallery {
    fn new(kind: Kind, count: usize) -> FakeGallery {
        FakeGallery { kind, count, thumbnail_request: None, last_page: None, picture_request: None, last_picture: None }
    }

    fn size(&self, item: usize) -> (usize, usize) {
        if self.kind == Kind::Photo { PHOTO_SIZE } else { VIDEO_SIZES[item % VIDEO_SIZES.len()] }
    }

    // An item's file, named for its place.
    fn file(&self, item: usize) -> String {
        format!("{item}.{}", if self.kind == Kind::Photo { "jpg" } else { "mp4" })
    }

    fn serve(&mut self, ui: &mut Ui, now: u64) {
        // An item that is deleted is one less in the roll, which is told again; pixel art made anew looks the same here.
        for errand in ui.take_errands() {
            if errand.stamp == Stamp::Trash {
                (self.count, self.thumbnail_request, self.last_page) = (self.count.saturating_sub(1), None, None);
            }
        }
        let Some(request) = ui.gallery_request() else {
            return;
        };
        if self.last_page != Some((request.round, request.page))
            && self.thumbnail_request.is_none_or(|(round, page, _)| (round, page) != (request.round, request.page))
        {
            ui.set_items(self.kind, self.count);
            self.thumbnail_request = Some((request.round, request.page, now));
        }
        if let Some((round, page, _)) = self.thumbnail_request.filter(|(.., since)| now >= since + THUMBNAIL_DELAY_NS) {
            let is_pixel_art = self.kind == Kind::Photo && !request.is_original;
            for item in page.saturating_sub(request.margin)..(page + request.places + request.margin).min(self.count) {
                let picture = fake_picture(item, gfx::to_cover(self.size(item), request.place_size), is_pixel_art);
                let label = if self.kind == Kind::Video { Label::Seconds(7 * item as u64 % 60) } else { Label::None };
                let file = self.file(item);
                ui.set_thumbnail(round, item, Thumbnail { picture, label, size: self.size(item), file });
            }
            (self.last_page, self.thumbnail_request) = (Some((round, page)), None);
        }
        match request.open_item.map(|item| (item, request.turn, request.is_original)) {
            Some(open) if self.last_picture == Some(open) => self.picture_request = None,
            Some(open) if self.picture_request.is_none_or(|(pending, _)| pending != open) => {
                self.picture_request = Some((open, now));
            }
            None => (self.picture_request, self.last_picture) = (None, None),
            _ => {}
        }
        if let Some((open, _)) = self.picture_request.filter(|(_, since)| now >= since + PICTURE_DELAY_NS) {
            let (item, _, is_original) = open;
            let is_pixel_art = self.kind == Kind::Photo && !is_original;
            let mut picture = fake_picture(item, gfx::to_fit(self.size(item), (WIDTH, HEIGHT)), is_pixel_art);
            ui.take_roll_picture((item, self.file(item)), &mut picture);
            (self.last_picture, self.picture_request) = (Some(open), None);
        }
    }
}

fn film(
    out: &str,
    ui: &mut Ui,
    mut serve: impl FnMut(&mut Ui, u64),
    start: u64,
    (from_ms, step_ms, frames): (u64, u64, usize),
    script: &[&str],
) {
    let mut script: Vec<(u64, Cue)> = script
        .iter()
        .map(|entry| {
            let (at, word) = entry.split_once(':').unwrap_or_else(|| panic!("{entry:?} is not MS:EVENT\n{USAGE}"));
            (start + at.parse::<u64>().unwrap() * NANOS_PER_MS, cue(word))
        })
        .collect();
    script.sort_by_key(|(at, _)| *at);
    let shrink = std::env::var(SHRINK_VARIABLE)
        .ok()
        .and_then(|word| word.parse().ok())
        .filter(|shrink| *shrink > 0)
        .unwrap_or(STRIP_SHRINK);
    let (cell_w, cell_h) = (WIDTH / shrink, HEIGHT / shrink);
    let mut sheet = Canvas::new(cell_w * frames, cell_h);
    let mut frame = Canvas::new(WIDTH, HEIGHT);
    let mut pending = script.into_iter().peekable();
    let mut clock = start;
    for index in 0..frames {
        let now = start + (from_ms + index as u64 * step_ms) * NANOS_PER_MS;
        while clock < now {
            clock = (clock + FRAME_NS).min(now);
            while let Some((at, cue)) = pending.next_if(|(at, _)| *at <= clock) {
                ui.tick(at);
                cue.apply(ui, at);
            }
            serve(ui, clock);
            ui.fade(clock);
            ui.tick(clock);
        }
        ui.render_over(&mut frame.surface(), now, None);
        sheet.paste(&frame, index * cell_w, 0, shrink);
    }
    sheet.save(out);
}

fn new_ui() -> Ui {
    let theme = std::env::var(THEME_VARIABLE).ok().and_then(|word| Theme::parse(&word)).unwrap_or(Theme::Stage);
    let mut ui = themed_ui(theme);
    ui.set_time(SAMPLE_TIME);
    ui.set_battery(Some(SAMPLE_LEVEL), Supply::Charging, 0);
    ui
}

fn delayed_answers(
    delay_ns: u64,
    ask: impl Fn(&mut Ui) -> Option<u32>,
    mut answer: impl FnMut(&mut Ui, u32),
) -> impl FnMut(&mut Ui, u64) {
    let mut pending: Option<(u32, u64)> = None;
    move |ui, at| match (ask(ui), pending) {
        (Some(pass), Some((pending_pass, since))) if pass == pending_pass => {
            if at >= since + delay_ns {
                answer(ui, pass);
            }
        }
        (pass, _) => pending = pass.map(|pass| (pass, at)),
    }
}

fn click(ui: &mut Ui, now: u64) {
    ui.handle(Event::PttDown, now);
    ui.handle(Event::PttUp, now);
}

fn run_to_rest(ui: &mut Ui, mut now: u64) -> u64 {
    for _ in 0..SETTLE_FRAMES {
        now += FRAME_NS;
        ui.tick(now);
    }
    now
}

// Up on a stage and on the dial, down on a list.
fn next_step(ui: &Ui) -> i32 {
    if ui.scene().to_words().0 == "STAGE" || ui.settings().theme == Theme::Dial { -1 } else { 1 }
}

fn choose(ui: &mut Ui, now: &mut u64, name: &str, settle: &mut impl FnMut(&mut Ui, &mut u64)) {
    let mut steps = 0;
    while ui.scene().to_words().1 != name {
        assert!(steps < MAX_STEPS, "{:?}: no {name} on the level that shows", ui.scene().to_words());
        ui.handle(Event::Wheel(next_step(ui)), *now);
        settle(ui, now);
        steps += 1;
    }
}

// Where the robot leads, the tree lies behind the way to everything.
fn way_in(theme: Theme) -> Option<&'static str> {
    (theme == Theme::Companion).then_some(EVERYTHING)
}

fn walk(ui: &mut Ui, names: &[&str]) -> u64 {
    let (mut now, mut settle) = (0, |ui: &mut Ui, now: &mut u64| *now = run_to_rest(ui, *now));
    for name in way_in(ui.settings().theme).iter().chain(names) {
        settle(ui, &mut now);
        choose(ui, &mut now, name, &mut settle);
        click(ui, now);
    }
    now
}

fn to_networks(count: usize) -> Vec<Network> {
    let network = |&(name, signal, security): &(&str, i32, Security)| Network {
        address: [2, 0, 0, 0, 0, 1],
        frequency: 2412,
        signal,
        name: name.into(),
        security,
        rsn: Vec::new(),
    };
    NETWORKS.iter().take(count).map(network).collect()
}

fn known_entries() -> Vec<known::Entry> {
    KNOWN_NETWORKS.iter().map(|&(name, is_locked)| known::Entry { name: name.into(), is_locked }).collect()
}

fn fake_notes() -> Vec<Memo> {
    let memo = |(stem, seconds, words, summary): (&str, u64, Option<&str>, Option<&str>)| Memo {
        stem: stem.into(),
        seconds,
        words: words.map(String::from),
        summary: summary.map(String::from),
        rundowns: Vec::new(),
    };
    NOTES.map(memo).into()
}

// A day's edition of those rundowns, each with that text.
fn fake_edition(names: &[&str], text: &str) -> Memo {
    let rundowns = names.iter().map(|name| (String::from(*name), String::from(text))).collect();
    Memo { stem: NEWS_DAY.into(), seconds: 0, words: None, summary: None, rundowns }
}

fn roll(out: &str, kind: Kind, count: usize, timing: (u64, u64, usize), script: &[&str]) {
    let mut ui = new_ui();
    // The roll's icon lies behind the camera's.
    let now = walk(&mut ui, &["CAMERA", if kind == Kind::Video { "VIDEOS" } else { "PICTURES" }]);
    let mut gallery = FakeGallery::new(kind, count);
    film(out, &mut ui, |ui, at| gallery.serve(ui, at), now, timing, script);
}

fn finder(out: &str, timing: (u64, u64, usize), script: &[&str]) {
    let mut ui = new_ui();
    let now = walk(&mut ui, &["CAMERA", "PHOTO"]);
    let size = ui.visible_camera().expect("the photo viewfinder").size;
    let frame = fake_picture(STREAM_TINT, size, false).words;
    // The stream hands a picture every pass, as the camera does every frame, until the photo just taken stands in its place.
    let stream = move |ui: &mut Ui, _: u64| {
        if ui.camera_request().is_some() && !ui.is_reviewing() {
            ui.finder_picture_mut().clone_from(&frame);
            ui.note_finder_frame();
        }
    };
    film(out, &mut ui, stream, now, timing, script);
}

fn wifi(out: &str, count: usize, timing: (u64, u64, usize), script: &[&str]) {
    let mut ui = new_ui();
    ui.set_known(known_entries());
    let now = walk(&mut ui, &["SETTINGS", "WIFI"]);
    let networks = to_networks(count);
    let radio = delayed_answers(
        SCAN_DELAY_NS,
        |ui| ui.radio(),
        |ui, pass| {
            ui.set_scan(pass, Some(&networks));
        },
    );
    film(out, &mut ui, radio, now, timing, script);
}

fn figures() -> Vec<Figure> {
    to_figures(&Ledger::parse(LEDGER), THIS_MONTH.0, THIS_MONTH.1)
}

fn cost(out: &str, timing: (u64, u64, usize), script: &[&str]) {
    let mut ui = new_ui();
    let now = walk(&mut ui, &["SETTINGS", "COST"]);
    let fetcher = delayed_answers(
        COST_DELAY_NS,
        |ui| ui.cost_request(),
        |ui, pass| {
            ui.set_cost(pass, figures());
        },
    );
    film(out, &mut ui, fetcher, now, timing, script);
}

fn clock(out: &str, timing: (u64, u64, usize), script: &[&str]) {
    let mut ui = new_ui();
    let now = walk(&mut ui, &["SETTINGS", "CLOCK"]);
    film(out, &mut ui, |_, _| {}, now, timing, script);
}

fn notes(out: &str, timing: (u64, u64, usize), script: &[&str]) {
    let mut ui = new_ui();
    let now = walk(&mut ui, &["MIC", "AUDIO NOTE"]);
    list(&mut ui, Shelf::Notes, fake_notes(), now);
    let (mut is_recording, mut playing_stem): (bool, Option<String>) = (false, None);
    let keeper = move |ui: &mut Ui, at: u64| {
        for request in ui.take_memo_requests() {
            match request {
                Request::Record => is_recording = !is_recording,
                Request::Play(stem) => {
                    playing_stem = if playing_stem.as_deref() == Some(stem.as_str()) { None } else { Some(stem) }
                }
                Request::Delete(..) => {}
            }
        }
        // The microphone hears a voice that climbs through eight levels and drops back once a second.
        ui.set_microphone(if is_recording { 40 << (at / (NANOS_PER_SECOND / 8) % 8) } else { 0 });
        ui.take_memos_report(
            NotesReport {
                recording: is_recording.then(String::new),
                playing: playing_stem.clone(),
                ..NotesReport::default()
            },
            at,
        );
        ui.update_memo_work(at);
    };
    film(out, &mut ui, keeper, now, timing, script);
}

fn news(out: &str, file: &str, timing: (u64, u64, usize), script: &[&str]) {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{file}: {e}"));
    let mut ui = new_ui();
    let now = walk(&mut ui, &["NEWS"]);
    list(&mut ui, Shelf::News, vec![fake_edition(&["news"], sheet::body(&text))], now);
    film(out, &mut ui, |_, _| {}, now, timing, script);
}

fn talk(out: &str, timing: (u64, u64, usize), script: &[&str]) {
    let mut ui = new_ui();
    let now = walk(&mut ui, &["MIC", "TRANSLATOR"]);
    list(&mut ui, Shelf::Talks, Vec::new(), now);
    let now = run_to_rest(&mut ui, now);
    click(&mut ui, now);
    let mut request: Option<(u32, u64)> = None;
    let session = move |ui: &mut Ui, at: u64| {
        let Some(round) = ui.session_request() else {
            if let Some((round, _)) = request.take() {
                ui.set_session(round, Phase::Idle, at);
            }
            return;
        };
        let since = request.filter(|(pending, _)| *pending == round).get_or_insert((round, at)).1;
        request = Some((round, since));
        if at < since + SESSION_DELAY_NS {
            return;
        }
        let mut remaining = ((at - since - SESSION_DELAY_NS) / CHARACTER_NS) as usize;
        let (mut turns, mut is_speaking) = (Vec::new(), false);
        for (source, translation) in REMARKS {
            let mut part = |text: &str| {
                let prefix: String = text.chars().take(remaining).collect();
                remaining -= prefix.chars().count();
                prefix
            };
            let turn = Turn { source: part(source), translation: part(translation) };
            is_speaking = !turn.translation.is_empty() && turn.translation.len() < translation.len();
            if !turn.source.is_empty() || !turn.translation.is_empty() {
                turns.push(turn);
            }
        }
        ui.set_session(round, if is_speaking { Phase::Speaking } else { Phase::Listening }, at);
        ui.set_remarks(round, &turns, at);
    };
    film(out, &mut ui, session, now, timing, script);
}

fn fake_cell() -> Cell {
    let sample = |at_ms: u64| Sample {
        at_ns: at_ms * NANOS_PER_MS,
        suspend_ms: 0,
        voltage: 3982,
        current: -1234,
        charge: -((at_ms / 1000) as i32),
        temperature: 29,
        charger: Charger::Idle,
    };
    let mut usage = Usage::default();
    for second in 0..=600u64 {
        usage.update(&sample(second * 1000), false, Some(92), Some(1200 + second as i64));
    }
    let figures = Figures { out: Some(1800), capacity: 3600, is_measured: true, current: -1234 };
    Cell { sample: sample(600_000), figures, usage, goal: 80 }
}

fn serve_all(ui: &mut Ui, now: u64) {
    if let SignalView::Conversation(peer) = ui.signal_view() {
        ui.take_signal_report(fake_signal(Some(peer)), now);
    }
    list(ui, Shelf::Notes, fake_notes(), now);
    let talk = Memo { stem: "20261015_101500".into(), seconds: 250, words: None, summary: None, rundowns: Vec::new() };
    list(ui, Shelf::Talks, vec![talk], now);
    list(ui, Shelf::News, vec![fake_edition(&["news", "finance", "motorsport"], MEETING)], now);
    if let Some(number) = ui.cost_request() {
        ui.set_cost(number, figures());
    }
    if ui.is_about_wanted() {
        let sources = device::Sources {
            version: Some(format!("Linux version {RELEASE} (r1metal@r1metal) #197 SMP PREEMPT")),
            meminfo: Some(MEMINFO.into()),
            storage_bytes: Some(STORAGE_BYTES),
            station: Some((ADDRESS, KNOWN_NETWORKS[0].0.into())),
            ..device::Sources::default()
        };
        ui.set_about(device::facts(&sources));
    }
    if let Some(pass) = ui.radio() {
        ui.set_scan(pass, Some(&to_networks(NETWORKS.len())));
    }
    if let Some(round) = ui.session_request() {
        ui.set_session(round, Phase::Listening, now);
        let turns: Vec<Turn> = REMARKS
            .iter()
            .map(|(source, translation)| Turn { source: (*source).into(), translation: (*translation).into() })
            .collect();
        ui.set_remarks(round, &turns, now);
    }
}

fn steps_to(theme: Theme, name: &str) -> u64 {
    let mut ui = themed_ui(theme);
    ui.set_tally(TALLY);
    let (mut steps, mut now) = (0, 0);
    while ui.scene().to_words().1 != name {
        let scene = ui.scene();
        ui.handle(Event::Wheel(next_step(&ui)), now);
        now = run_to_rest(&mut ui, now);
        assert!(ui.scene() != scene, "{theme:?}: no {name} at home");
        steps += 1;
    }
    steps
}

fn list(ui: &mut Ui, shelf: Shelf, memos: Vec<Memo>, now: u64) {
    ui.take_memos_report(NotesReport { listing: Some((shelf, memos)), ..NotesReport::default() }, now);
}

fn motion(folder: &str, step_ms: u64, frames: usize) {
    std::fs::create_dir_all(folder).unwrap_or_else(|e| panic!("{folder}: {e}"));
    let settle_ms = SETTLE_FRAMES as u64 * FRAME_NS / NANOS_PER_MS;
    for theme in THEMES.into_iter().filter(|theme| *theme != Theme::Stage) {
        let mut ui = themed_ui(theme);
        ui.set_time(SAMPLE_TIME);
        ui.set_battery(Some(SAMPLE_LEVEL), Supply::Battery, 0);
        ui.set_tally(TALLY);
        let next = if next_step(&ui) < 0 { "up" } else { "down" };
        let steps = steps_to(theme, way_in(theme).unwrap_or(SETTINGS));
        let mut script: Vec<String> = (0..steps).map(|step| format!("{}:{next}", step * settle_ms)).collect();
        let press_ms = steps * settle_ms + settle_ms;
        script.extend([format!("{press_ms}:press"), format!("{press_ms}:release")]);
        let word = theme.to_word().replace(' ', "_");
        let open_entries: Vec<&str> = script.iter().map(String::as_str).collect();
        film(&format!("{folder}/{word}_in.png"), &mut ui, serve_all, 0, (press_ms, step_ms, frames), &open_entries);
        let back = (press_ms + step_ms * frames as u64 + settle_ms) * NANOS_PER_MS;
        let swipe: Vec<String> =
            (5..=205).step_by(10).map(|x| format!("0:touch:{x}:500")).chain([String::from("0:lift")]).collect();
        let swipe_entries: Vec<&str> = swipe.iter().map(String::as_str).collect();
        film(&format!("{folder}/{word}_back.png"), &mut ui, serve_all, back, (0, step_ms, frames), &swipe_entries);
    }
}

// In mm/s² as the accelerometer reports it, for the device held turned clockwise by that many degrees.
fn gravity(degrees: i32) -> (i32, i32) {
    let (sine, cosine) = f64::from(degrees).to_radians().sin_cos();
    ((sine * f64::from(G)).round() as i32, (cosine * f64::from(G)).round() as i32)
}

fn full_ui(theme: Theme) -> Ui {
    let mut ui = themed_ui(theme);
    ui.set_time(SAMPLE_TIME);
    ui.set_battery(Some(SAMPLE_LEVEL), Supply::Battery, 0);
    ui.set_cell(fake_cell());
    ui.set_link(Some(Way::Wifi));
    ui.set_known(known_entries());
    ui.set_settings(Settings { is_cellular: true, is_modem: true, uses_tunnel: true, ..ui.settings() });
    ui.set_modem(MODEM);
    // A computer on the cable, its packets in the tunnel whose config is kept.
    let router = RouterReport { standing: Some(Standing::Tunnel), has_config: Some(true), store: None };
    ui.take_router_report(&router, 0);
    ui.set_tally(TALLY);
    ui.set_chat_identity(false, Some("Pixel"));
    let entries: Vec<Entry> =
        CHAT.iter().map(|(kind, text)| Entry { kind: *kind, time: "14:02".into(), text: (*text).into() }).collect();
    ui.set_chat_day(NEWS_DAY, entries);
    ui
}

fn fake_link() -> SignalReport {
    let status = ThreadStatus {
        link_status: LinkStatus::Unlinked,
        link_step: LinkStep::Code(LINK_ADDRESS.into()),
        is_connected: false,
        failure: None,
        unread: 0,
        auto: None,
    };
    SignalReport { status, ..SignalReport::default() }
}

fn fake_signal(open_peer: Option<Peer>) -> SignalReport {
    let base = SAMPLE_TIME * 1000;
    let messages = |thread: &[FakeMessage], has_unread: bool| -> Vec<Message> {
        let last = thread.len().saturating_sub(1);
        thread
            .iter()
            .enumerate()
            .map(|(at, (is_owner, body, after, reactions))| Message {
                timestamp: base + after * 1000,
                at: base + after * 1000,
                is_owner: *is_owner,
                body: (*body).into(),
                placeholders: Vec::new(),
                attachments: Vec::new(),
                mentions: Vec::new(),
                quote: None,
                deleter: None,
                status: if *is_owner { Status::Read } else { Status::Received },
                expire: 0,
                started: None,
                is_read: *is_owner || !(has_unread && at == last),
                is_edited: false,
                is_robot: false,
                sender: None,
                reactions: reactions
                    .iter()
                    .enumerate()
                    .map(|(giver, emoji)| MessageReaction {
                        from: ServiceId::Aci([0x80 + giver as u8; 16]),
                        emoji: (*emoji).into(),
                        timestamp: base,
                    })
                    .collect(),
            })
            .collect()
    };
    let summaries = SIGNAL_THREADS
        .iter()
        .map(|(id, name, thread)| Summary {
            peer: Peer::Contact(ServiceId::Aci([*id; 16])),
            name: Some((*name).into()),
            last: messages(thread, *id == 1).pop(),
            unread: usize::from(*id == 1),
            is_marked: false,
            is_pinned: false,
            is_archived: false,
            is_counted: true,
        })
        .collect();
    let conversation = open_peer.and_then(|peer| {
        let (_, name, thread) =
            SIGNAL_THREADS.iter().find(|(id, ..)| Peer::Contact(ServiceId::Aci([*id; 16])) == peer)?;
        let (entries, owner) = (messages(thread, false), ServiceId::Aci([0; 16]));
        let writers = BTreeMap::new();
        Some(ConversationView { peer, name: Some((*name).into()), entries, is_safety_changed: false, writers, owner })
    });
    let status = ThreadStatus { link_status: LinkStatus::Linked, is_connected: true, unread: 1, ..fake_link().status };
    SignalReport { summaries: Some(summaries), conversation, status, ..SignalReport::default() }
}

const fn contact(id: u8) -> Peer {
    Peer::Contact(ServiceId::Aci([id; 16]))
}

fn thread_name(id: u8) -> &'static str {
    SIGNAL_THREADS.iter().find(|thread| thread.0 == id).map_or(DANA_NAME, |thread| thread.1)
}

fn fake_log() -> Vec<CallRow> {
    let row = |&(id, kind, is_outgoing, status, before_s, is_seen): &FakeCall| {
        let (peer, name) = id.map_or((CLIMBING, CLIMBING_NAME), |id| (contact(id), thread_name(id)));
        let at = (SAMPLE_TIME - before_s) * 1000;
        CallRow { peer, name: Some(name.into()), kind, is_outgoing, status, at, is_seen }
    };
    CALL_LOG.iter().map(row).collect()
}

// The contacts of the conversations and the group, then one never written to, by name as Signal's thread sorts them.
fn fake_callees() -> Vec<Contactable> {
    let callee = |peer, name: &str, has_talked| Contactable { peer, name: name.into(), has_talked };
    let contacts = SIGNAL_THREADS.iter().map(|(id, name, _)| callee(contact(*id), name, true));
    contacts.chain([callee(CLIMBING, CLIMBING_NAME, true), callee(contact(DANA), DANA_NAME, false)]).collect()
}

// The call the screen named tells of, as the call layer hands it.
fn fake_call(name: &str) -> Option<CallView> {
    let talking = CallView {
        peer: contact(1),
        name: Some(thread_name(1).into()),
        is_outgoing: true,
        is_video: false,
        phase: CallPhase::Talking,
        since: Some(Duration::ZERO),
        is_muted: false,
        is_video_on: false,
        has_far_video: false,
        members: Vec::new(),
    };
    let video = CallView { is_video: true, is_video_on: true, has_far_video: true, ..talking.clone() };
    let members = MEMBERS
        .iter()
        .map(|&(name, is_speaking, is_muted)| Member {
            name: Some(name.into()),
            is_speaking,
            is_muted,
            has_video: true,
        })
        .collect();
    let in_group = |view: CallView| CallView { peer: CLIMBING, name: Some(CLIMBING_NAME.into()), members, ..view };
    let at = |phase| CallView { phase, since: None, ..talking.clone() };
    Some(match name {
        "call calling" => at(CallPhase::Calling),
        "call ringing" => at(CallPhase::Ringing),
        "call connecting" => at(CallPhase::Connecting),
        "call talking" => talking,
        "call muted" => CallView { is_muted: true, ..talking },
        "call reconnecting" => at(CallPhase::Reconnecting),
        "call ended" => at(CallPhase::Ended(Ending::FarHungUp)),
        "call failed" => at(CallPhase::Ended(Ending::Failed(Cause::NoNetwork))),
        "call group" => in_group(talking),
        "call video calling" => CallView { phase: CallPhase::Calling, since: None, has_far_video: false, ..video },
        "call video" | "call video wide" | "call video 3x4" | "call video 4x3" | CAMERA_FAILED => video,
        "call group video" => in_group(video),
        "call incoming" | ANSWERED_CALL => CallView {
            peer: contact(2),
            name: Some(thread_name(2).into()),
            is_outgoing: false,
            is_video: true,
            ..at(CallPhase::Incoming)
        },
        _ => return None,
    })
}

// The far side's picture of the screen `name` fitted to the room the screen names, frame after frame until that room stands, and this side's as the camera gives it where the call wants it.
fn show_call_pictures(ui: &mut Ui, name: &str) {
    let far = FAR_PICTURES.iter().find(|(shown, _)| *shown == name).unwrap_or(&FAR_PICTURES[0]).1;
    let mut room = (0, 0);
    while room != ui.far_picture_room() {
        room = ui.far_picture_room();
        *ui.far_picture_mut() = fake_picture(FAR_TINT, gfx::to_fit(far, room), false);
        ui.note_far_frame();
    }
    if let Some(viewfinder) = ui.call_camera() {
        *ui.own_picture_mut() = fake_picture(STREAM_TINT, viewfinder.frame_size(), false).words;
        ui.note_own_frame();
    }
}

// A result of the Pokédex's: an entry of its number alone under an artwork.
fn show_article(ui: &mut Ui, now: u64) {
    let mut entry = PokedexEntry::default();
    entry.number = SAMPLE_POKEMON;
    let side = ui.artwork_side();
    let article = Article { entry, picture: Some(fake_picture(0, (side, side), false)) };
    ui.set_pokedex(Phase::Idle, Some(article), now);
}

// A page of the browser's, as the device reads one.
fn show_page(ui: &mut Ui, now: u64) {
    let links = (1..=PAGE_LINKS).map(|number| format!("https://news.example/{number}")).collect();
    let title = "Harbour ferry returns after two years".into();
    let page = Page { address: "https://news.example/ferry".into(), title, text: PAGE.into(), links };
    ui.set_web(Phase::Idle, Some(page), now);
}

// The microphone heard on the screen `name` tells of: its job listens, as its thread reports it, and the card tells the level.
fn listen(ui: &mut Ui, name: &str, now: u64) {
    match name {
        "chat listening" => {
            let mut report = ChatReport::default();
            report.phase = Some(Phase::Listening);
            ui.take_chat_report(report, now);
        }
        "conversation listening" => drop(ui.set_signal_draft(Phase::Listening, None, now)),
        "web page listening" => drop(ui.set_web(Phase::Listening, None, now)),
        "bell listening" => drop(ui.take_bell_report(BellReport { phase: Phase::Listening, changes: Vec::new() }, now)),
        "rundowns listening" => {
            ui.take_rundowns_report(RundownsReport { phase: Phase::Listening, changes: Vec::new() }, now);
        }
        TRANSLATOR => {}
        _ => drop(ui.set_pokedex(Phase::Listening, None, now)),
    }
    ui.set_microphone(SAMPLE_PEAK);
}

fn home_turns(folder: &str, degrees: i32) {
    std::fs::create_dir_all(folder).unwrap_or_else(|e| panic!("{folder}: {e}"));
    let (x, y) = gravity(degrees);
    for theme in THEMES {
        let mut ui = full_ui(theme);
        let mut now = run_to_rest(&mut ui, 0);
        ui.set_tilt(x, y);
        now = run_to_rest(&mut ui, now);
        let mut picture = Canvas::new(WIDTH, HEIGHT);
        ui.render_over(&mut picture.surface(), now, None);
        picture.save(&format!("{folder}/{}_turned_{degrees}.png", theme.to_word().replace(' ', "_")));
    }
}

fn themes(folder: &str) {
    std::fs::create_dir_all(folder).unwrap_or_else(|e| panic!("{folder}: {e}"));
    for theme in THEMES {
        let word = theme.to_word().replace(' ', "_");
        let mut shots = Vec::new();
        // The Pokédex stands in the tree of a build with the extras alone.
        for (name, walk) in
            SCREENS.into_iter().filter(|(name, _)| cfg!(feature = "extras") || !name.starts_with(POKEDEX))
        {
            let mut ui = full_ui(theme);
            ui.take_signal_report(if name == "link" { fake_link() } else { fake_signal(None) }, 0);
            if name.starts_with("home alarm") {
                ui.recall_bell(Bell::parse(&format!("entry clock {SAMPLE_ALARM}\n")));
                ui.set_link((name == "home alarm").then_some(Way::Wifi));
            }
            if name.starts_with("bell") {
                let timer = SAMPLE_TIME + SAMPLE_TIMER_S;
                ui.recall_bell(Bell::parse(&format!(
                    "entry clock {SAMPLE_ALARM}\nentry clock {SAMPLE_REMINDER} dentist at ten\nentry span {timer} tea\n"
                )));
            }
            if name == "home mobile" {
                ui.set_link(Some(Way::Cell));
            }
            if name.starts_with(RUNDOWNS_NAME) || name == FEATURES_NEWS {
                ui.recall_rundowns(Topics::parse(RUNDOWNS));
                let silent = Some(Silent::parse(SILENT_FEEDS));
                ui.take_editor_report(EditorReport { silent, ..EditorReport::default() }, 0);
            }
            if name.starts_with(CALL) {
                ui.take_call_log(Some(fake_log()), Some(fake_callees()));
            }
            let mut now = 0;
            let mut gallery = FakeGallery::new(Kind::Photo, TALLY.photos);
            let mut settle = |ui: &mut Ui, now: &mut u64| {
                for _ in 0..SETTLE_FRAMES {
                    *now += FRAME_NS;
                    serve_all(ui, *now);
                    gallery.serve(ui, *now);
                    ui.tick(*now);
                }
            };
            settle(&mut ui, &mut now);
            let everything = way_in(theme).filter(|_| !walk.is_empty()).map(Open);
            for step in everything.into_iter().chain(walk.iter().copied()) {
                match step {
                    Open(name) | Stand(name) => choose(&mut ui, &mut now, name, &mut settle),
                    Lines(lines) => {
                        for _ in 0..lines {
                            ui.handle(Event::Wheel(next_step(&ui)), now);
                            settle(&mut ui, &mut now);
                        }
                    }
                    Before(lines) => {
                        for _ in 0..lines {
                            ui.handle(Event::Wheel(-next_step(&ui)), now);
                            settle(&mut ui, &mut now);
                        }
                    }
                }
                if !matches!(step, Stand(_)) {
                    click(&mut ui, now);
                }
                settle(&mut ui, &mut now);
            }
            if let Some(call) = fake_call(name) {
                let (is_video, has_ended) = (call.is_video, matches!(call.phase, CallPhase::Ended(_)));
                ui.set_call(Some(call), now);
                if is_video {
                    show_call_pictures(&mut ui, name);
                }
                ui.set_microphone(SAMPLE_PEAK);
                let has_failed = name == CAMERA_FAILED;
                if has_failed {
                    ui.take_outcome(&Outcome::Failed, now);
                }
                if has_ended || has_failed {
                    for _ in 0..ENDING_FRAMES {
                        now += FRAME_NS;
                        serve_all(&mut ui, now);
                        ui.tick(now);
                    }
                } else {
                    settle(&mut ui, &mut now);
                }
            }
            if name == ANSWERED_CALL {
                click(&mut ui, now);
                settle(&mut ui, &mut now);
            }
            if name == "pokedex page listening" {
                show_article(&mut ui, now);
                settle(&mut ui, &mut now);
            }
            if name.starts_with(WEB_PAGE) {
                show_page(&mut ui, now);
                settle(&mut ui, &mut now);
            }
            // A click raises the keys a request is typed on.
            if name == "web page keys" {
                click(&mut ui, now);
                settle(&mut ui, &mut now);
            }
            if name.ends_with(LISTENING) || name == TRANSLATOR {
                listen(&mut ui, name, now);
                settle(&mut ui, &mut now);
            }
            // A deletion a request asks waits on the rundown's page for the click that deletes.
            if name == RUNDOWN_DELETION {
                let changes = vec![Change::Delete(DELETED.into())];
                ui.take_rundowns_report(RundownsReport { phase: Phase::Idle, changes }, now);
                settle(&mut ui, &mut now);
            }
            // The alarm and the timer are due by then, and ring in their window over home.
            if name == "bell rings" {
                ui.ring(SAMPLE_ALARM, now);
                settle(&mut ui, &mut now);
            }
            let mut picture = Canvas::new(WIDTH, HEIGHT);
            ui.render_over(&mut picture.surface(), now, None);
            picture.save(&format!("{folder}/{word}_{name}.png"));
            shots.push(picture);
        }
        let gap = 16;
        let mut sheet = Canvas::new(shots.len() * (WIDTH + gap) + gap, HEIGHT + 2 * gap);
        sheet.pixels.fill(0x28_2828);
        for (at, shot) in shots.iter().enumerate() {
            sheet.paste(shot, gap + at * (WIDTH + gap), gap, 1);
        }
        sheet.save(&format!("{folder}/{word}_sheet.png"));
    }
}

fn life(
    robot: &mut Robot,
    mut now: u64,
    end: u64,
    is_done: impl Fn(&Robot) -> bool,
    mut on_frame: impl FnMut(&Robot, Option<u64>),
) {
    let mut has_frames = false;
    while now < end {
        robot.advance(now);
        if is_done(robot) {
            break;
        }
        if robot.is_moving() {
            on_frame(robot, Some(now));
            has_frames = true;
            now += FRAME_NS;
        } else {
            if std::mem::take(&mut has_frames) {
                on_frame(robot, None);
            }
            now = robot.deadline().map_or(now + FRAME_NS, |due| due.max(now + 1));
        }
    }
}

fn portrait(robot: &Robot) -> Mask {
    let mut mask = Mask::new(ROBOT_BOX, ROBOT_BOX);
    robot.trace(&mut mask, &robot.seat_in(0.0, 0.0, ROBOT_BOX as f32), |_| true);
    mask
}

fn sheet(out: &str, frames: &mut [Option<Mask>]) {
    let mut sheet = Canvas::new(ROBOT_BOX * ROBOT_ROW, ROBOT_BOX * frames.len().div_ceil(ROBOT_ROW).max(1));
    for (index, frame) in frames.iter_mut().enumerate() {
        if let Some(mask) = frame {
            sheet.surface().cover(
                (index % ROBOT_ROW * ROBOT_BOX) as i32,
                (index / ROBOT_ROW * ROBOT_BOX) as i32,
                mask,
                ROLES.robot,
                ROLES.background,
            );
        }
    }
    sheet.save(out);
}

fn robot(out: &str, from_ms: u64, seconds: u64) {
    let now = from_ms * NANOS_PER_MS;
    let end = now + seconds * NANOS_PER_SECOND;
    let mut frames = Vec::new();
    life(&mut Robot::new(), now, end, |_| false, |robot, at| frames.push(at.map(|_| portrait(robot))));
    sheet(out, &mut frames);
}

fn gesture(out: &str, number: usize, every: usize, is_mirrored: bool) {
    let mut robot = Robot::new();
    robot.rehearse(number, is_mirrored, REHEARSAL_NS);
    let (mut frames, mut count) = (Vec::new(), 0);
    life(&mut robot, REHEARSAL_NS, REHEARSAL_NS + REHEARSAL_SPAN_NS, Robot::is_resting, |robot, at| {
        if at.is_none() || count % every.max(1) == 0 {
            frames.push(at.map(|_| portrait(robot)));
        }
        count += usize::from(at.is_some());
    });
    println!("gesture {number} of {}: {count} frames", Robot::gestures());
    sheet(out, &mut frames);
}

fn summary(out: &str, file: &str, subject: Subject<'_>) {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{file}: {e}"));
    std::fs::write(out, to_pages(subject, &text, None, &ROLES)).unwrap_or_else(|e| panic!("{out}: {e}"));
}

fn budget(from_ms: u64, seconds: u64, stir_s: u64) {
    let mut robot = Robot::new();
    let (start, span) = (from_ms * NANOS_PER_MS, seconds * NANOS_PER_SECOND);
    let (mut frames, mut awake_ns, mut awake_end, mut burst, mut longest) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut now = start;
    while now < start + span {
        let end = if stir_s == 0 { start + span } else { (now + stir_s * NANOS_PER_SECOND).min(start + span) };
        life(
            &mut robot,
            now,
            end,
            |_| false,
            |_, at| match at {
                Some(at) => {
                    (frames, burst) = (frames + 1, burst + 1);
                    let light_end = at + FRAME_NS + IDLE_TAIL_NS;
                    awake_ns += light_end - at.max(awake_end);
                    awake_end = light_end;
                    longest = longest.max(burst);
                }
                None => burst = 0,
            },
        );
        now = end;
        if stir_s > 0 {
            robot.stir(now);
        }
    }
    let share = |nanos: u64| nanos as f64 / span as f64 * 100.0;
    println!(
        "{frames} frames in {seconds} s: drawn {:.1} % of the time, display out of its idle mode {:.1} %, longest run {} ms, asleep at the end: {}",
        share(frames * FRAME_NS),
        share(awake_ns),
        longest * FRAME_NS / NANOS_PER_MS,
        robot.is_asleep()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let number = |word: &str| word.parse::<u64>().unwrap_or_else(|_| panic!("{word:?} is not a number\n{USAGE}"));
    let timing = |from_ms, step_ms, frames| (number(from_ms), number(step_ms), number(frames) as usize);
    match words[..] {
        ["debug", out] => debug(out),
        ["themes", folder] => themes(folder),
        ["turned", folder, degrees] => {
            home_turns(folder, degrees.parse().unwrap_or_else(|_| panic!("{degrees:?} is not a number\n{USAGE}")))
        }
        ["motion", folder, step_ms, frames] => motion(folder, number(step_ms), number(frames) as usize),
        ["icons", out, ref files @ ..] => icons(out, files),
        ["robot", out, from_ms, seconds] => robot(out, number(from_ms), number(seconds)),
        ["gesture", out, index, every, ref flags @ ..] => {
            gesture(out, number(index) as usize, number(every) as usize, flags == ["mirrored"])
        }
        ["budget", from_ms, seconds, stir_s] => budget(number(from_ms), number(seconds), number(stir_s)),
        ["summary", out, file, stem, seconds] => summary(out, file, Subject::Note(stem, number(seconds))),
        ["summary", out, file, stem] => summary(out, file, Subject::Talk(stem)),
        ["notes", out, from_ms, step_ms, frames, ref script @ ..] => {
            notes(out, timing(from_ms, step_ms, frames), script)
        }
        ["news", out, file, from_ms, step_ms, frames, ref script @ ..] => {
            news(out, file, timing(from_ms, step_ms, frames), script)
        }
        ["talk", out, from_ms, step_ms, frames, ref script @ ..] => talk(out, timing(from_ms, step_ms, frames), script),
        ["strip", out, from_ms, step_ms, frames, ref script @ ..] => {
            film(out, &mut new_ui(), |_, _| {}, 0, timing(from_ms, step_ms, frames), script)
        }
        ["roll", out, kind @ ("pictures" | "videos"), count, from_ms, step_ms, frames, ref script @ ..] => {
            let kind = if kind == "videos" { Kind::Video } else { Kind::Photo };
            roll(out, kind, number(count) as usize, timing(from_ms, step_ms, frames), script);
        }
        ["finder", out, from_ms, step_ms, frames, ref script @ ..] => {
            finder(out, timing(from_ms, step_ms, frames), script)
        }
        ["wifi", out, count, from_ms, step_ms, frames, ref script @ ..] => {
            wifi(out, number(count) as usize, timing(from_ms, step_ms, frames), script)
        }
        ["cost", out, from_ms, step_ms, frames, ref script @ ..] => cost(out, timing(from_ms, step_ms, frames), script),
        ["clock", out, from_ms, step_ms, frames, ref script @ ..] => {
            clock(out, timing(from_ms, step_ms, frames), script)
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
