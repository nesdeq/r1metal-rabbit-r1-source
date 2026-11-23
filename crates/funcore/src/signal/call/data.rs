use super::Hangup;
use crate::base::proto::{self, Writer};
use alloc::vec::Vec;

// RingRTC's rtp_data.proto: `Message`'s fields and those of its parts.
const ACCEPTED: u32 = 1;
const HANGUP: u32 = 2;
const SENDER_STATUS: u32 = 3;
const SEQNUM: u32 = 4;
const RECEIVER_STATUS: u32 = 5;
const ID: u32 = 1;
const HANGUP_TYPE: u32 = 2;
const HANGUP_DEVICE: u32 = 3;
const VIDEO_ENABLED: u32 = 2;
const SHARING_SCREEN: u32 = 3;
const AUDIO_ENABLED: u32 = 4;
const MAX_BITRATE: u32 = 2;

// What a side sends of itself, each where it says so (`SenderStatus`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Sending {
    pub(crate) is_video_on: Option<bool>,
    pub(crate) is_sharing_screen: Option<bool>,
    pub(crate) is_audio_on: Option<bool>,
}

// What a side of a one-to-one call tells the other over the media path, each part with the call's id.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Message {
    pub(crate) accepted: Option<u64>,
    // How the call ended, and for a device of the callee's that did not take it, the device that did.
    pub(crate) hangup: Option<(u64, Hangup, Option<u32>)>,
    pub(crate) sending: Option<(u64, Sending)>,
    // A later message carries a larger number.
    pub(crate) seqnum: Option<u64>,
    // The most the side wants sent to it, in bits a second (`ReceiverStatus`).
    pub(crate) max_bitrate: Option<(u64, u64)>,
}

impl Message {
    // None for what is no protobuf; a part without its id is passed over.
    pub(crate) fn read(bytes: &[u8]) -> Option<Message> {
        let message = proto::Message::parse(bytes)?;
        let part = |field| message.message(field).and_then(|part| Some((part.varint(ID)?, part)));
        Some(Message {
            accepted: part(ACCEPTED).map(|(id, _)| id),
            hangup: part(HANGUP).map(|(id, hangup)| {
                (id, Hangup::read(hangup.varint(HANGUP_TYPE).unwrap_or(0)), hangup.u32(HANGUP_DEVICE))
            }),
            sending: part(SENDER_STATUS).map(|(id, status)| {
                let sending = Sending {
                    is_video_on: status.bool(VIDEO_ENABLED),
                    is_sharing_screen: status.bool(SHARING_SCREEN),
                    is_audio_on: status.bool(AUDIO_ENABLED),
                };
                (id, sending)
            }),
            seqnum: message.varint(SEQNUM),
            max_bitrate: part(RECEIVER_STATUS).and_then(|(id, status)| Some((id, status.varint(MAX_BITRATE)?))),
        })
    }

    pub(crate) fn write(&self) -> Vec<u8> {
        let with_id = |id: u64| Writer::new().varint(ID, id);
        let flag = |field: u32, value: Option<bool>| value.map(|value| move |w: Writer| w.bool(field, value));
        let accepted = self.accepted.map(|id| move |w: Writer| w.message(ACCEPTED, with_id(id)));
        let hangup = self.hangup.map(|(id, kind, device)| {
            let named = device.map(|device| move |w: Writer| w.varint(HANGUP_DEVICE, u64::from(device)));
            move |w: Writer| w.message(HANGUP, with_id(id).varint(HANGUP_TYPE, kind as u64).maybe(named))
        });
        let sending = self.sending.map(|(id, sending)| {
            let status = with_id(id)
                .maybe(flag(VIDEO_ENABLED, sending.is_video_on))
                .maybe(flag(SHARING_SCREEN, sending.is_sharing_screen))
                .maybe(flag(AUDIO_ENABLED, sending.is_audio_on));
            move |w: Writer| w.message(SENDER_STATUS, status)
        });
        let seqnum = self.seqnum.map(|seqnum| move |w: Writer| w.varint(SEQNUM, seqnum));
        let receiving = self
            .max_bitrate
            .map(|(id, rate)| move |w: Writer| w.message(RECEIVER_STATUS, with_id(id).varint(MAX_BITRATE, rate)));
        Writer::new().maybe(accepted).maybe(hangup).maybe(sending).maybe(seqnum).maybe(receiving).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let whole = Message {
            accepted: Some(u64::MAX),
            hangup: Some((7, Hangup::Accepted, Some(3))),
            sending: Some((
                7,
                Sending { is_video_on: Some(true), is_sharing_screen: Some(false), is_audio_on: Some(true) },
            )),
            seqnum: Some(300),
            max_bitrate: Some((7, 2_000_000)),
        };
        let partly = Message {
            hangup: Some((7, Hangup::Normal, None)),
            sending: Some((7, Sending { is_video_on: Some(false), ..Sending::default() })),
            ..Message::default()
        };
        for message in [whole, partly, Message::default()] {
            assert_eq!(Message::read(&message.write()), Some(message), "{message:?}");
        }
        assert_eq!(
            Message { accepted: Some(300), seqnum: Some(2), ..Message::default() }.write(),
            [0x0A, 0x03, 0x08, 0xAC, 0x02, 0x20, 0x02],
            "the accepted call in field 1 by its id, the sequence number in field 4"
        );
    }

    #[test]
    fn unreadable_parts() {
        let without_id = Writer::new()
            .message(ACCEPTED, Writer::new())
            .message(HANGUP, Writer::new().varint(HANGUP_TYPE, 2))
            .message(RECEIVER_STATUS, Writer::new().varint(ID, 7))
            .varint(SEQNUM, 9);
        assert_eq!(
            Message::read(&without_id.finish()),
            Some(Message { seqnum: Some(9), ..Message::default() }),
            "a part without its call's id, and a receiver's status without a rate, are passed over"
        );
        let unknown = Writer::new().message(HANGUP, Writer::new().varint(ID, 7).varint(HANGUP_TYPE, 9));
        assert_eq!(
            Message::read(&unknown.finish()).and_then(|message| message.hangup),
            Some((7, Hangup::Normal, None)),
            "a hang-up of a kind not known is a plain one"
        );
        assert_eq!(Message::read(&[0x0A, 0x05, 1]), None, "what is no protobuf");
    }
}
