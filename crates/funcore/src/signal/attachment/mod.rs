//! Signal's attachments: a message's pointers to them, the files the R1 keeps their plaintexts in, and their transfers.

mod seal;
mod transfer;

#[cfg(test)]
pub(crate) use transfer::samples;
pub use transfer::{Download, Finish, Job, Step, Took, Transfer, Upload, Uploaded, to_finished_line};

use super::Crypto;
use super::content::{Placeholder, placeholder_of};
use super::service::CDNS;
use crate::base::code::{hex, percent_encoded};
use crate::base::proto::{Message, Writer};
use crate::storage::paths::new_name;
use crate::storage::{PICTURE_EXTENSION, VIDEO_EXTENSION};
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use seal::{DIGEST, sealed_size};

pub(super) use seal::KEYS;

/// The directory the plaintexts of attachments are kept in, with the voice notes the owner says for Signal.
pub const ATTACHMENTS: &str = ".r1metal/signal/attachments";
// The pointer's flag of a voice note (`AttachmentPointer.Flags.VOICE_MESSAGE`).
const VOICE: u64 = 1;
const PHOTO_KIND: &str = "image/jpeg";
const PNG_KIND: &str = "image/png";
const VIDEO_KIND: &str = "video/mp4";
// The most an attachment's ciphertext holds for Signal's apps to send it (Signal Desktop's default of `global.attachments.maxBytes` and of `global.videoAttachments.transcodeTargetBytes`).
const MOST_SEALED: u64 = 100 << 20;
// The kind of a voice note the R1 says, as Signal's apps send theirs, and the ending of the recorder's files.
const VOICE_KIND: &str = "audio/aac";
const SOUND_ENDING: &str = "m4a";
const PNG_ENDING: &str = "png";
const UNKNOWN_ENDING: &str = "bin";
// The bytes `encodeURIComponent` leaves as they are beside letters and digits.
const URI_COMPONENT_KEPT: &[u8] = b"-_.!~*'()";

// Where a CDN holds an attachment: by the first CDN's number, or by the key an upload form gave.
#[derive(Clone, PartialEq, Eq)]
enum Locator {
    Id(u64),
    Key(String),
}

/// An attachment as a message carries it and as the R1 holds it.
#[derive(Clone, PartialEq, Eq)]
pub struct Attachment {
    pub(super) content_type: String,
    pub(in crate::signal) keys: [u8; KEYS],
    pub(in crate::signal) digest: [u8; DIGEST],
    pub(in crate::signal) size: u32,
    pub(in crate::signal) cdn: u32,
    locator: Option<Locator>,
    uploaded_at: u64,
    pub(crate) is_voice: bool,
    width: u32,
    height: u32,
    // The file that holds its plaintext, once the R1 holds one.
    pub(crate) file: Option<String>,
}

impl core::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Attachment")
            .field("content_type", &self.content_type)
            .field("size", &self.size)
            .field("voice", &self.is_voice)
            .field("located", &self.locator.is_some())
            .field("held", &self.file.is_some())
            .finish_non_exhaustive()
    }
}

// A file the owner sends with a message, before it goes up.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Enclosure {
    // The file that holds it, from the data partition's root; empty for a voice note just said.
    pub(crate) file: String,
    content_type: String,
    pub(crate) is_voice: bool,
    width: u32,
    height: u32,
    sound: Vec<u8>,
}

impl core::fmt::Debug for Enclosure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Enclosure")
            .field("content_type", &self.content_type)
            .field("voice", &self.is_voice)
            .finish_non_exhaustive()
    }
}

impl Enclosure {
    fn new(file: String, kind: &str, is_voice: bool) -> Enclosure {
        Enclosure { file, content_type: String::from(kind), is_voice, width: 0, height: 0, sound: Vec::new() }
    }

    // A photo of the roll, by its file from the data partition's root and its size.
    pub(crate) fn photo(file: String, (width, height): (usize, usize)) -> Enclosure {
        let side = |pixels: usize| u32::try_from(pixels).unwrap_or(0);
        Enclosure { width: side(width), height: side(height), ..Enclosure::new(file, PHOTO_KIND, false) }
    }

    // A video of the roll, by its file from the data partition's root.
    pub(crate) fn video(file: String) -> Enclosure {
        Enclosure::new(file, VIDEO_KIND, false)
    }

    // A note recorded, by its file from the data partition's root: a voice note, as one just said is.
    pub(crate) fn note(file: String) -> Enclosure {
        Enclosure::new(file, VOICE_KIND, true)
    }

    // A voice note just said, as the recorder's file whole.
    pub(crate) fn said(sound: Vec<u8>) -> Enclosure {
        Enclosure { sound, ..Enclosure::new(String::new(), VOICE_KIND, true) }
    }

    // What a conversation tells it by where it does not show it.
    pub(crate) fn placeholder(&self) -> Placeholder {
        placeholder_of(&self.content_type, self.is_voice)
    }

    // Its size in pixels, noughts where it is no photo.
    pub(crate) const fn size(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }

    // Names the sound of a voice note just said by its hash, so that no file another holds is written over, and takes it out to be kept under that name in the attachments' directory.
    pub(super) fn take_sound(&mut self, crypto: &impl Crypto) -> Option<(String, Vec<u8>)> {
        if self.sound.is_empty() {
            return None;
        }
        let name = format!("{}.{SOUND_ENDING}", hex(&crypto.sha256(&[&self.sound])));
        self.file = format!("{ATTACHMENTS}/{name}");
        Some((name, core::mem::take(&mut self.sound)))
    }

    pub(in crate::signal) fn attach(&self, keys: [u8; KEYS]) -> Attachment {
        Attachment {
            content_type: self.content_type.clone(),
            keys,
            digest: [0; DIGEST],
            size: 0,
            cdn: 0,
            locator: None,
            uploaded_at: 0,
            is_voice: self.is_voice,
            width: self.width,
            height: self.height,
            file: Some(self.file.clone()),
        }
    }
}

// True where a file of so many bytes is one Signal's apps send (Signal Desktop's `isAttachmentTooLarge`).
pub(crate) fn fits(bytes: u64) -> bool {
    sealed_size(bytes) <= MOST_SEALED
}

fn ending(kind: &str) -> &'static str {
    match kind {
        PHOTO_KIND => PICTURE_EXTENSION,
        PNG_KIND => PNG_ENDING,
        VIDEO_KIND => VIDEO_EXTENSION,
        kind if kind.starts_with("audio/") => SOUND_ENDING,
        _ => UNKNOWN_ENDING,
    }
}

// The file a fetched attachment is kept in, from the data partition's root: its digest in hex, which no two attachments share, and the ending of its kind.
pub(in crate::signal) fn kept_as(digest: &[u8; DIGEST], kind: &str) -> String {
    format!("{ATTACHMENTS}/{}.{}", hex(digest), ending(kind))
}

// The file an owner's attachment is kept in once it went up: one outside the attachments' directory under its message's timestamp and its place, which no two attachments of the R1's share; one inside it keeps its own, and so does a video, which no conversation shows.
pub(in crate::signal) fn sent_as(file: &str, (timestamp, index): (u64, usize), kind: &str) -> String {
    let is_inside = file.strip_prefix(ATTACHMENTS).is_some_and(|rest| rest.starts_with('/'));
    if is_inside || kind == VIDEO_KIND {
        String::from(file)
    } else {
        format!("{ATTACHMENTS}/{timestamp}-{index}.{}", ending(kind))
    }
}

/// The files of the attachments' directory, as its listing names them, that nothing holds: not among `held`, the names from the data partition's root, nor half written to take the place of one that is.
pub fn unheld(listed: Vec<String>, held: &BTreeSet<String>) -> Vec<String> {
    let holds = |path: &str| held.contains(path) || held.iter().any(|kept| new_name(kept) == path);
    listed.into_iter().filter(|name| !holds(&format!("{ATTACHMENTS}/{name}"))).collect()
}

// A pointer's kind and whether it is a voice note, which stand in for it wherever it is not had.
pub(super) fn read_kind<'a>(pointer: &'a Message) -> (&'a str, bool) {
    (pointer.text(2).unwrap_or_default(), pointer.varint(8).unwrap_or(0) & VOICE != 0)
}

impl Attachment {
    // An attachment a DataMessage's pointer names (`AttachmentPointer`); none where it lacks its keys, digest, size or where a CDN holds it, without which it cannot be had and checked.
    pub(super) fn read(pointer: &Message) -> Option<Attachment> {
        let locator = match (pointer.fixed64(1), pointer.text(15).filter(|key| !key.is_empty())) {
            (_, Some(key)) => Locator::Key(String::from(key)),
            (Some(id), None) => Locator::Id(id),
            (None, None) => return None,
        };
        let (kind, is_voice) = read_kind(pointer);
        Some(Attachment {
            content_type: String::from(kind),
            keys: pointer.bytes(3)?.try_into().ok()?,
            digest: pointer.bytes(6)?.try_into().ok()?,
            size: pointer.u32(4)?,
            cdn: pointer.u32(14).unwrap_or(0),
            locator: Some(locator),
            uploaded_at: pointer.varint(13).unwrap_or(0),
            is_voice,
            width: pointer.u32(9).unwrap_or(0),
            height: pointer.u32(10).unwrap_or(0),
            file: None,
        })
    }

    // The pointer a message of the R1's carries, as Signal Desktop writes one (`uploadAttachment`); none before the attachment went up.
    pub(super) fn pointer(&self) -> Option<Writer> {
        let Some(Locator::Key(key)) = &self.locator else { return None };
        Some(
            Writer::new()
                .text(2, &self.content_type)
                .bytes(3, &self.keys)
                .varint(4, u64::from(self.size))
                .bytes(6, &self.digest)
                .maybe(self.is_voice.then_some(|w: Writer| w.varint(8, VOICE)))
                .maybe((self.width > 0).then_some(|w: Writer| w.varint(9, u64::from(self.width))))
                .maybe((self.height > 0).then_some(|w: Writer| w.varint(10, u64::from(self.height))))
                .varint(13, self.uploaded_at)
                .varint(14, u64::from(self.cdn))
                .text(15, key),
        )
    }

    pub(super) fn write_line(&self) -> Writer {
        let located = |w: Writer| match &self.locator {
            Some(Locator::Id(id)) => w.varint(6, *id),
            Some(Locator::Key(key)) => w.text(7, key),
            None => w,
        };
        located(
            Writer::new()
                .text(1, &self.content_type)
                .bytes(2, &self.keys)
                .bytes(3, &self.digest)
                .varint(4, u64::from(self.size))
                .varint(5, u64::from(self.cdn)),
        )
        .varint(8, self.uploaded_at)
        .bool(9, self.is_voice)
        .varint(10, u64::from(self.width))
        .varint(11, u64::from(self.height))
        .maybe(self.file.as_deref().map(|file| move |w: Writer| w.text(12, file)))
    }

    pub(super) fn from_line(line: &Message) -> Option<Attachment> {
        let locator = match (line.varint(6), line.text(7)) {
            (_, Some(key)) => Some(Locator::Key(String::from(key))),
            (Some(id), None) => Some(Locator::Id(id)),
            (None, None) => None,
        };
        Some(Attachment {
            content_type: String::from(line.text(1).unwrap_or_default()),
            keys: line.bytes(2)?.try_into().ok()?,
            digest: line.bytes(3)?.try_into().ok()?,
            size: line.u32(4)?,
            cdn: line.u32(5).unwrap_or(0),
            locator,
            uploaded_at: line.varint(8).unwrap_or(0),
            is_voice: line.bool(9).unwrap_or(false),
            width: line.u32(10).unwrap_or(0),
            height: line.u32(11).unwrap_or(0),
            file: line.text(12).map(String::from),
        })
    }

    // Where its CDN serves it, as Signal Desktop asks for one (`getAttachment`): the address of the CDN of its number, the first's for a number it does not know, and its key or id under `/attachments/`.
    pub(in crate::signal) fn address(&self) -> Option<String> {
        let base = CDNS.iter().find(|(number, _)| *number == self.cdn).unwrap_or(&CDNS[0]).1;
        Some(match self.locator.as_ref()? {
            Locator::Id(id) => format!("{base}/attachments/{id}"),
            Locator::Key(key) => format!("{base}/attachments/{}", percent_encoded(key, URI_COMPONENT_KEPT)),
        })
    }

    // It holds the file an earlier attachment of its digest is held in.
    pub(super) fn inheriting(mut self, earlier: &[Attachment]) -> Attachment {
        self.file = earlier.iter().find(|held| held.digest == self.digest).and_then(|held| held.file.clone());
        self
    }

    pub(in crate::signal) const fn is_located(&self) -> bool {
        self.locator.is_some()
    }

    // It went up to a CDN of that number under that key at `at`, ms, encrypted to that digest.
    pub(in crate::signal) fn note_uploaded(&mut self, cdn: u32, key: String, digest: [u8; DIGEST], at: u64) {
        (self.cdn, self.locator, self.digest, self.uploaded_at) = (cdn, Some(Locator::Key(key)), digest, at);
    }

    // Its CDN holds it no more: its place there is let go, so that it is asked for no more (Signal Desktop's `markAttachmentAsErrored`).
    pub(in crate::signal) fn note_gone(&mut self) {
        self.locator = None;
    }

    // What a conversation tells it by where it does not show it.
    pub(in crate::signal) fn placeholder(&self) -> Placeholder {
        placeholder_of(&self.content_type, self.is_voice)
    }

    // True for a picture the R1 draws: a JPEG or a PNG.
    pub(crate) fn is_drawn(&self) -> bool {
        matches!(self.content_type.as_str(), PHOTO_KIND | PNG_KIND)
    }

    // Its size in pixels as its pointer tells it, noughts where the pointer tells none.
    pub(crate) const fn size(&self) -> (usize, usize) {
        (self.width as usize, self.height as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::tests::Standin;

    #[test]
    fn pointer_round_trip() {
        let mut outgoing = Enclosure::photo("photo.jpg".into(), (1_024, 768)).attach([3; KEYS]);
        outgoing.size = 12_345;
        assert!(outgoing.pointer().is_none() && outgoing.address().is_none(), "nothing to point to before its upload");
        outgoing.note_uploaded(3, String::from("abc/def"), [4; DIGEST], 1_700);
        let pointer = outgoing.pointer().unwrap().finish();
        let read = Attachment::read(&Message::parse(&pointer).unwrap()).unwrap();
        assert_eq!(read, Attachment { file: None, ..outgoing.clone() }, "all but the file the R1 holds it in");
        assert_eq!(
            (Enclosure::photo("photo.jpg".into(), (1_024, 768)).size(), read.size()),
            ((1_024, 768), (1_024, 768)),
            "a photo's size goes with its pointer"
        );
        assert_eq!(read.address().as_deref(), Some("https://cdn3.signal.org/attachments/abc%2Fdef"));
        let voice = Writer::new()
            .bytes(3, &[5; KEYS])
            .bytes(6, &[6; DIGEST])
            .varint(4, 9)
            .varint(8, VOICE)
            .text(2, "audio/aac")
            .finish();
        let fixed = [&voice[..], &[1 << 3 | 1], &7u64.to_le_bytes()].concat();
        let old = Attachment::read(&Message::parse(&fixed).unwrap()).unwrap();
        assert!(
            old.is_voice && old.address().as_deref() == Some("https://cdn.signal.org/attachments/7"),
            "the first CDN's id"
        );
        assert_eq!(old.size(), (0, 0), "a pointer that tells no size");
        assert_eq!(Attachment::read(&Message::parse(&voice).unwrap()), None, "no CDN holds it");
        let keyless = Writer::new().varint(4, 9).text(15, "k").bytes(6, &[6; DIGEST]).finish();
        assert_eq!(Attachment::read(&Message::parse(&keyless).unwrap()), None, "no keys");
        for held in [outgoing, old] {
            let line = held.write_line().finish();
            assert_eq!(Attachment::from_line(&Message::parse(&line).unwrap()), Some(held), "a line keeps all of it");
        }
        assert_eq!(
            format!("{read:?}"),
            "Attachment { content_type: \"image/jpeg\", size: 12345, voice: false, located: true, held: false, .. }"
        );
        assert!(read.is_drawn() && !Attachment { content_type: "image/webp".into(), ..read }.is_drawn());
    }

    #[test]
    fn kept_files() {
        let digest = [0xAB; DIGEST];
        let hex = "ab".repeat(DIGEST);
        assert_eq!(kept_as(&digest, "image/jpeg"), format!("{ATTACHMENTS}/{hex}.jpg"));
        assert_eq!(
            [kept_as(&digest, "image/png"), kept_as(&digest, "audio/mp4"), kept_as(&digest, "video/mp4")],
            [
                format!("{ATTACHMENTS}/{hex}.png"),
                format!("{ATTACHMENTS}/{hex}.m4a"),
                format!("{ATTACHMENTS}/{hex}.mp4")
            ]
        );
        assert_eq!(kept_as(&digest, "application/pdf"), format!("{ATTACHMENTS}/{hex}.bin"), "a kind it knows none of");
        assert_eq!(sent_as("camera/pictures/a.jpg", (5, 1), "image/jpeg"), format!("{ATTACHMENTS}/5-1.jpg"));
        assert_eq!(
            sent_as("audio/notes/n/n.m4a", (5, 1), "audio/aac"),
            format!("{ATTACHMENTS}/5-1.m4a"),
            "a note recorded is kept beside the others, to play where its message stands"
        );
        let own = format!("{ATTACHMENTS}/1700.m4a");
        assert_eq!(sent_as(&own, (5, 1), "audio/aac"), own, "a file inside keeps its own");
        assert_eq!(
            sent_as("camera/videos/a.mp4", (5, 1), "video/mp4"),
            "camera/videos/a.mp4",
            "and so does a video, of which no second copy is made"
        );
    }

    #[test]
    fn unheld_files() {
        let held: BTreeSet<String> = [format!("{ATTACHMENTS}/5-0.jpg"), format!("{ATTACHMENTS}/ab.aac")].into();
        let listed = ["5-0.jpg", "ab.aac", "ab.aac.new", "stray.jpg", "stray.jpg.new"].map(String::from).to_vec();
        assert_eq!(
            unheld(listed, &held),
            ["stray.jpg", "stray.jpg.new"],
            "one half written to take a held one's place stays"
        );
    }

    #[test]
    fn voice_note_just_said() {
        let crypto = Standin::default();
        let mut said = Enclosure::said(b"sound".to_vec());
        let name = format!("{}.m4a", hex(&crypto.sha256(&[b"sound"])));
        assert_eq!(said.take_sound(&crypto), Some((name.clone(), b"sound".to_vec())), "named by its sound's hash");
        assert_eq!(
            (said.file.as_str(), said.is_voice, said.content_type.as_str()),
            (format!("{ATTACHMENTS}/{name}").as_str(), true, "audio/aac")
        );
        assert_eq!(said.take_sound(&crypto), None, "taken once");
        let mut photo = Enclosure::photo("camera/pictures/a.jpg".into(), (4, 3));
        assert_eq!((photo.take_sound(&crypto), photo.file.as_str()), (None, "camera/pictures/a.jpg"));
        assert_eq!(format!("{photo:?}"), "Enclosure { content_type: \"image/jpeg\", voice: false, .. }");
    }

    #[test]
    fn roll_and_note_files() {
        let crypto = Standin::default();
        let mut video = Enclosure::video("camera/videos/a.mp4".into());
        let mut note = Enclosure::note("audio/notes/n/n.m4a".into());
        assert_eq!(
            [video.take_sound(&crypto), note.take_sound(&crypto)],
            [None, None],
            "each goes up from its own file"
        );
        assert_eq!(
            [&video, &note, &Enclosure::photo("a.jpg".into(), (4, 3)), &Enclosure::said(b"sound".to_vec())]
                .map(Enclosure::placeholder),
            [Placeholder::Video, Placeholder::VoiceNote, Placeholder::Photo, Placeholder::VoiceNote]
        );
        let (sent, voice) = (video.attach([3; KEYS]), note.attach([3; KEYS]));
        assert_eq!(
            (sent.content_type.as_str(), sent.is_voice, sent.is_drawn(), sent.file.as_deref()),
            ("video/mp4", false, false, Some("camera/videos/a.mp4"))
        );
        assert_eq!(
            (voice.content_type.as_str(), voice.is_voice, voice.file.as_deref()),
            ("audio/aac", true, Some("audio/notes/n/n.m4a")),
            "a note recorded goes as a voice note just said does"
        );
    }

    #[test]
    fn size_signal_sends() {
        // The largest padded size whose ciphertext is no more than 100 MiB: the padding's next step, 107,335,737 bytes, is over it.
        const LARGEST: u64 = 102_224_512;
        assert!(fits(0) && fits(LARGEST), "as Desktop pads and encrypts it, 102,224,576 bytes of the 104,857,600");
        assert!(!fits(LARGEST + 1) && !fits(u64::from(u32::MAX)));
    }
}
