use super::outbox::Order;
use super::{Device, File, Peer};
use crate::signal::Crypto;
use crate::signal::attachment::{Attachment, Download, Finish, Job, Upload, Uploaded, kept_as, sent_as};
use crate::signal::conversation::{OWN, Status};
use crate::signal::service::basic_auth;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

impl Device {
    // The attachments of the owner's messages that wait to go up before their messages go.
    pub(crate) fn uploads_due(&self) -> Vec<Upload> {
        let authorization = basic_auth(&self.account.username(), &self.account.password);
        let mut due = Vec::new();
        for conversation in self.conversations.values() {
            let sending = conversation.entries.iter().filter(|entry| entry.is_owner && entry.status == Status::Sending);
            for entry in sending {
                for (index, held) in entry.attachments.iter().enumerate().filter(|(_, held)| !held.is_located()) {
                    let Some(file) = held.file.clone() else { continue };
                    due.push(Upload {
                        peer: conversation.peer,
                        timestamp: entry.timestamp,
                        index,
                        kept: sent_as(&file, (entry.timestamp, index), &held.content_type),
                        file,
                        keys: held.keys,
                        authorization: authorization.clone(),
                    });
                }
            }
        }
        due
    }

    // The photos the last `window` messages of a conversation draw, and their voice notes, that the R1 does not hold yet, to be fetched, the newest message's first.
    pub(crate) fn downloads_due(&self, peer: &Peer, window: usize) -> Vec<Download> {
        let mut due = Vec::new();
        for entry in self.entries(peer).iter().rev().take(window) {
            for (index, held) in entry.attachments.iter().enumerate() {
                let is_wanted = held.file.is_none() && (held.is_drawn() || held.is_voice);
                let Some(address) = held.address().filter(|_| is_wanted) else { continue };
                due.push(Download {
                    peer: *peer,
                    message: (entry.timestamp, entry.is_owner, entry.sender),
                    index,
                    cdn: held.cdn,
                    address,
                    keys: held.keys,
                    digest: held.digest,
                    size: held.size,
                    file: kept_as(&held.digest, &held.content_type),
                });
            }
        }
        due
    }

    // Takes what transfers ended in: an attachment that went up into its message, which goes once all it carries went up; one refused fails its message; one fetched is held by its message; one its CDN holds no more is asked for no more. The orders of the messages that go, and whether anything changed.
    pub(crate) fn take_finished(
        &mut self,
        crypto: &impl Crypto,
        done: Vec<(Job, Finish)>,
        now: u64,
    ) -> (Vec<Order>, bool) {
        let (mut orders, mut is_changed) = (Vec::new(), false);
        for (job, finish) in done {
            match (job, finish) {
                (Job::Up(upload), Finish::Uploaded(uploaded)) => {
                    let message = (upload.peer, upload.timestamp, upload.index);
                    orders.extend(self.note_attachment_uploaded(crypto, message, &uploaded, now));
                    is_changed = true;
                }
                (Job::Up(upload), Finish::Refused) => {
                    self.note_refused(upload.peer, upload.timestamp);
                    is_changed = true;
                }
                (Job::Down(download), Finish::Fetched) => {
                    is_changed |= self.change_attachment(&download, |held| held.file = Some(download.file.clone()));
                }
                (Job::Down(download), Finish::Gone) => {
                    is_changed |= self.change_attachment(&download, Attachment::note_gone);
                }
                _ => {}
            }
        }
        (orders, is_changed)
    }

    fn note_attachment_uploaded(
        &mut self,
        crypto: &impl Crypto,
        (peer, timestamp, index): (Peer, u64, usize),
        uploaded: &Uploaded,
        now: u64,
    ) -> Option<Order> {
        let entry = self.conversations.get_mut(&peer)?.entry_mut(timestamp, OWN)?;
        let held = entry.attachments.get_mut(index)?;
        held.note_uploaded(uploaded.cdn, uploaded.key.clone(), uploaded.digest, now);
        (held.size, held.file) = (uploaded.size, Some(uploaded.file.clone()));
        let is_ready = entry.status == Status::Sending && entry.attachments.iter().all(Attachment::is_located);
        let entry = is_ready.then(|| entry.clone());
        self.dirty.insert(File::Conversation(peer));
        self.message_order(crypto, peer, &entry?, now)
    }

    fn note_refused(&mut self, peer: Peer, timestamp: u64) {
        if let Some(conversation) = self.conversations.get_mut(&peer) {
            conversation.set_status(timestamp, Status::Failed);
            self.dirty.insert(File::Conversation(peer));
        }
    }

    // Changes the attachment a download was of, its conversation's file marked; false where its message holds it no more.
    fn change_attachment(&mut self, download: &Download, change: impl FnOnce(&mut Attachment)) -> bool {
        let (timestamp, is_owner, sender) = download.message;
        let Some(conversation) = self.conversations.get_mut(&download.peer) else { return false };
        let found = conversation.entry_mut(timestamp, (is_owner, sender));
        let Some(held) = found.and_then(|entry| entry.attachments.get_mut(download.index)) else { return false };
        change(held);
        self.dirty.insert(File::Conversation(download.peer));
        true
    }

    /// The files that hold the plaintexts of the attachments of every message held.
    pub fn held_files(&self) -> BTreeSet<String> {
        let entries = self.conversations.values().flat_map(|conversation| conversation.entries.iter());
        entries.flat_map(|entry| entry.files().map(String::from)).collect()
    }

    /// True once after messages went, which may leave files of their attachments nothing holds.
    pub fn take_dropped(&mut self) -> bool {
        core::mem::take(&mut self.is_dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::proto::{Message, Writer};
    use crate::signal::address::{Address, ServiceId};
    use crate::signal::attachment::samples::uploaded;
    use crate::signal::attachment::{ATTACHMENTS, Enclosure};
    use crate::signal::content::{self, Content, Text};
    use crate::signal::tests::Standin;
    use alloc::{format, vec};

    const ALICE: ServiceId = ServiceId::Aci([4; 16]);
    const PEER: Peer = Peer::Contact(ALICE);

    fn pointer(kind: &str, flags: u64) -> Attachment {
        let bytes = Writer::new()
            .text(2, kind)
            .bytes(3, &[3; 64])
            .varint(4, 15)
            .varint(8, flags)
            .bytes(6, &[6; 32])
            .text(15, "k")
            .finish();
        Attachment::read(&Message::parse(&bytes).unwrap()).unwrap()
    }

    #[test]
    fn photo_upload_then_message() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let photo = Enclosure::photo("camera/pictures/20261029_101500.jpg".into(), (4, 3));
        assert!(device.compose(&crypto, PEER, ("", &[photo]), 1_000, false).is_none(), "it waits for its photo");
        assert!(device.unsent_orders(&crypto, 2_000).is_empty(), "and a restart does not send it either");
        let upload = device.uploads_due().remove(0);
        assert_eq!((upload.timestamp, upload.index), (1_000, 0));
        assert_eq!(upload.kept, format!("{ATTACHMENTS}/1000-0.jpg"), "its plaintext is kept beside the others");
        assert_eq!(device.entries(&PEER)[0].status, Status::Sending);
        let job = Job::Up(upload.clone());
        let (orders, is_changed) =
            device.take_finished(&crypto, vec![(job, Finish::Uploaded(uploaded(&upload.kept)))], 3_000);
        assert!(is_changed && orders.len() == 1, "the message goes once its photo is up");
        let Some(Content::Text(text)) = content::parse_content(&orders[0].content, 1_000, false) else {
            panic!("a message")
        };
        assert_eq!(text.attachments.len(), 1, "it carries the photo's pointer");
        assert!(device.uploads_due().is_empty(), "nothing more to go up");
        assert_eq!(device.held_files(), BTreeSet::from([upload.kept]), "held in the file beside the others");
        assert_eq!(device.unsent_orders(&crypto, 4_000).len(), 1, "and a restart sends it with its photo");
        assert!(device.take_dirty().contains(&File::Conversation(PEER)));
    }

    #[test]
    fn video_and_note_uploads() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let video = Enclosure::video("camera/videos/20261029_101500.mp4".into());
        let note = Enclosure::note("audio/notes/20261029_101500/20261029_101500.m4a".into());
        assert!(device.compose(&crypto, PEER, ("", &[video, note]), 1_000, false).is_none());
        let uploads = device.uploads_due();
        assert_eq!(
            uploads.iter().map(|upload| (upload.file.as_str(), upload.kept.as_str())).collect::<Vec<_>>(),
            [
                ("camera/videos/20261029_101500.mp4", "camera/videos/20261029_101500.mp4"),
                ("audio/notes/20261029_101500/20261029_101500.m4a", ".r1metal/signal/attachments/1000-1.m4a")
            ],
            "the video is held where the roll keeps it, the note in a file beside the other attachments"
        );
        let done = uploads.iter().map(|upload| (Job::Up(upload.clone()), Finish::Uploaded(uploaded(&upload.kept))));
        let (orders, _) = device.take_finished(&crypto, done.collect(), 3_000);
        let Some(Content::Text(text)) = content::parse_content(&orders[0].content, 1_000, false) else {
            panic!("a message")
        };
        assert_eq!(text.placeholders, [content::Placeholder::Video, content::Placeholder::VoiceNote]);
        assert!(text.attachments[1].is_voice && !text.attachments[0].is_voice, "the note goes as a voice note");
        assert_eq!(
            device.held_files(),
            BTreeSet::from([uploads[0].kept.clone(), uploads[1].kept.clone()]),
            "its message holds each in its file"
        );
    }

    #[test]
    fn refused_upload() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        device.compose(&crypto, PEER, ("", &[Enclosure::photo("gone.jpg".into(), (0, 0))]), 1_000, false);
        let upload = device.uploads_due().remove(0);
        let broken = device.take_finished(&crypto, vec![(Job::Up(upload.clone()), Finish::Broke)], 1_500);
        assert_eq!((broken.0.len(), broken.1), (0, false), "broken, it changes nothing: it is tried again");
        let (orders, is_changed) = device.take_finished(&crypto, vec![(Job::Up(upload), Finish::Refused)], 2_000);
        assert!(orders.is_empty() && is_changed);
        assert_eq!(device.entries(&PEER)[0].status, Status::Failed, "refused, its message is not sent");
        assert!(device.uploads_due().is_empty());
    }

    #[test]
    fn photo_and_voice_note_downloads() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let from = Address { service: ALICE, device: 1 };
        let text = Text {
            timestamp: 9,
            attachments: vec![pointer("image/jpeg", 0), pointer("video/mp4", 0), pointer("audio/aac", 1)],
            ..Text::default()
        };
        device.apply(&crypto, Content::Text(text), &from, 3_000);
        let due = device.downloads_due(&PEER, 1);
        assert_eq!(
            due.iter().map(|download| (download.index, download.size)).collect::<Vec<_>>(),
            [(0, 15), (2, 15)],
            "a picture the R1 draws and a voice note, not a video"
        );
        assert!(
            device.downloads_due(&Peer::Contact(ServiceId::Aci([9; 16])), 1).is_empty(),
            "none for another conversation"
        );
        let job = due[0].clone();
        let (_, is_changed) = device.take_finished(&crypto, vec![(Job::Down(job.clone()), Finish::Fetched)], 4_000);
        assert!(is_changed && device.held_files() == BTreeSet::from([job.file.clone()]));
        assert_eq!(device.downloads_due(&PEER, 1).len(), 1, "fetched once");
        let note = device.downloads_due(&PEER, 1).remove(0);
        assert!(!device.take_finished(&crypto, vec![(Job::Down(note.clone()), Finish::Refused)], 4_100).1);
        assert_eq!(device.downloads_due(&PEER, 1).len(), 1, "refused, it is asked again in a later stay");
        assert!(device.take_finished(&crypto, vec![(Job::Down(note), Finish::Gone)], 4_200).1);
        assert!(device.downloads_due(&PEER, 1).is_empty(), "at its CDN no more, it is asked for no more");
        let copy = Device::read(
            &[File::Account, File::Conversation(PEER)]
                .into_iter()
                .map(|file| (file.clone(), device.file_bytes(&file).expect("written")))
                .collect(),
        );
        assert!(copy.expect("a device").downloads_due(&PEER, 1).is_empty(), "nor after a start");
        device.apply(&crypto, Content::Delete { target: 9, group: None }, &from, 5_000);
        assert!(
            device.take_dropped() && device.held_files().is_empty(),
            "deleted for everyone, its file is held no more"
        );
        assert!(!device.take_dropped(), "once");
    }

    #[test]
    fn download_window_newest_first() {
        let mut device = Device::unkeyed();
        for (timestamp, digest) in [(9, 6), (10, 7), (11, 8)] {
            device.add_incoming_photo(ALICE, (timestamp, digest), 1_000 * timestamp);
        }
        let stamps = |window| {
            let due = device.downloads_due(&PEER, window);
            due.iter().map(|download| download.message.0).collect::<Vec<_>>()
        };
        assert_eq!(stamps(3), [11, 10, 9], "the newest message's photo is fetched first");
        assert_eq!((stamps(2), stamps(0)), (vec![11, 10], vec![]), "and none of a message the view holds no more");
    }
}
