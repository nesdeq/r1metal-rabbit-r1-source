use super::message::{HEAD, OPTION_HEAD, OPTION_PADDING, QR, TYPE_OPT, add_options, add_padding, skip_name, to_frame};
use crate::base::bytes::be16;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const COUNTS_AT: usize = 4;
const EXTRA_COUNT_AT: usize = 10;
const FLAGS_AT: usize = 2;
const TRUNCATED: u16 = 0x0200;
// What a message over UDP holds at the most where its asker's options name no more (RFC 1035, 4.2.1).
const PLAIN_BYTES: usize = 512;
const RECORD_HEAD: usize = 10;
const SIZE_AT: usize = 2;
const LENGTH_AT: usize = 8;
// The streams of askers kept at once: as many connections as Linux 4.19 lets wait for a listener (SOMAXCONN).
const STREAMS: usize = 128;

/// The log line of the relay's connection made, its times in milliseconds.
pub fn to_relay_line(address: &str, connect_ms: u128, tls: &str, tls_ms: u128) -> String {
    format!("names: the relay reached the resolver at {address} in {connect_ms} ms, {tls} after {tls_ms} ms")
}

fn to_relayed_line(asked: u64, answered: u64) -> String {
    format!("names: the relay's connection ended, {answered} of {asked} questions answered")
}

// Where a message's options record begins behind its name, if it ends with one.
fn find_options(message: &[u8]) -> Option<usize> {
    let count = |at: usize| be16(message, COUNTS_AT + 2 * at).map(usize::from);
    let (questions, extra) = (count(0)?, count(3)?);
    let mut at = HEAD;
    for _ in 0..questions {
        at = skip_name(message, at)? + 4;
    }
    let mut last = None;
    for _ in 0..count(1)? + count(2)? + extra {
        let fields = skip_name(message, at)?;
        last = (be16(message, fields) == Some(TYPE_OPT) && extra > 0).then_some(fields);
        at = fields + RECORD_HEAD + usize::from(be16(message, fields + LENGTH_AT)?);
    }
    last.filter(|_| at == message.len())
}

fn set_count(message: &mut [u8], at: usize, count: u16) {
    message[at..at + 2].copy_from_slice(&count.to_be_bytes());
}

// True if the options that begin at `fields` hold the one that fills a message up.
fn is_padded(message: &[u8], fields: usize) -> bool {
    let mut options = &message[fields + RECORD_HEAD..];
    while let (Some(code), Some(length)) = (be16(options, 0), be16(options, 2)) {
        if code == OPTION_PADDING {
            return true;
        }
        options = options.get(OPTION_HEAD + usize::from(length)..).unwrap_or_default();
    }
    false
}

/// The streams askers reach a relay over, by the number each is given, the oldest first.
#[derive(Default)]
pub struct Streams {
    next: u64,
    open: VecDeque<u64>,
}

impl Streams {
    /// Takes a new stream: its number, and the oldest stream, which goes for it where as many stand as a relay keeps.
    pub fn open(&mut self) -> (u64, Option<u64>) {
        let number = self.next;
        self.next += 1;
        let oldest = if self.open.len() >= STREAMS { self.open.pop_front() } else { None };
        self.open.push_back(number);
        (number, oldest)
    }

    /// Forgets a stream that ended.
    pub fn close(&mut self, number: u64) {
        self.open.retain(|open| *open != number);
    }
}

struct Asked<A> {
    asker: A,
    id: u16,
    // The longest answer the asker takes in one datagram; none over a stream, which carries any.
    limit: Option<usize>,
    has_added: bool,
}

/// A relay of whole messages: each question goes on under a number of the relay's own, filled up to whole blocks, and its answer back to its asker `A`, fitted to what that takes.
pub struct Relay<A> {
    next: u16,
    asked: BTreeMap<u16, Asked<A>>,
    // How many questions went over the resolver's stream that stands, and how many of them it answered.
    sent: u64,
    answered: u64,
}

impl<A> Default for Relay<A> {
    fn default() -> Relay<A> {
        Relay { next: 0, asked: BTreeMap::new(), sent: 0, answered: 0 }
    }
}

impl<A> Relay<A> {
    /// Takes a question, come over a stream or in a datagram: what goes to the resolver's stream; none for what is no question, for one too long for a stream's message once filled up, or while as many wait as there are numbers.
    pub fn ask(&mut self, asker: A, is_stream: bool, question: &[u8]) -> Option<Vec<u8>> {
        if question.len() < HEAD || be16(question, FLAGS_AT)? & QR != 0 {
            return None;
        }
        let number = (0..=u16::MAX).map(|step| self.next.wrapping_add(step)).find(|id| !self.asked.contains_key(id))?;
        let mut message = question.to_vec();
        let options = find_options(&message);
        let named = options.and_then(|fields| be16(&message, fields + SIZE_AT)).map(usize::from);
        match options {
            Some(fields) if !is_padded(&message, fields) => {
                add_padding(&mut message);
                let length = (message.len() - fields - RECORD_HEAD) as u16;
                set_count(&mut message, fields + LENGTH_AT, length);
            }
            Some(_) => {}
            None => {
                let extra = be16(&message, EXTRA_COUNT_AT)?;
                set_count(&mut message, EXTRA_COUNT_AT, extra.checked_add(1)?);
                add_options(&mut message);
            }
        }
        // Two bytes count a stream's message (RFC 1035, 4.2.2): one that outgrew them filled up would go out under a length it has not.
        if message.len() > usize::from(u16::MAX) {
            return None;
        }
        let limit = (!is_stream).then_some(named.map_or(PLAIN_BYTES, |named| named.max(PLAIN_BYTES)));
        let asked = Asked { asker, id: be16(question, 0)?, limit, has_added: options.is_none() };
        self.next = number.wrapping_add(1);
        self.asked.insert(number, asked);
        self.sent += 1;
        set_count(&mut message, 0, number);
        Some(to_frame(&message))
    }

    /// Takes an answer off the resolver's stream: its asker, and what goes back to it, a datagram or a message of a stream; none for an answer to nothing asked.
    pub fn answer(&mut self, message: &[u8]) -> Option<(A, Vec<u8>)> {
        let asked = self.asked.remove(&be16(message, 0).filter(|_| message.len() >= HEAD)?)?;
        self.answered += 1;
        let mut message = message.to_vec();
        set_count(&mut message, 0, asked.id);
        // An asker that sent no options gets none back (RFC 6891, 6.1.1).
        if let Some(fields) = find_options(&message).filter(|_| asked.has_added) {
            message.truncate(fields - 1);
            let extra = be16(&message, EXTRA_COUNT_AT).unwrap_or_default();
            set_count(&mut message, EXTRA_COUNT_AT, extra.saturating_sub(1));
        }
        match asked.limit {
            None => Some((asked.asker, to_frame(&message))),
            Some(limit) if message.len() <= limit => Some((asked.asker, message)),
            Some(_) => {
                // Cut to its questions and marked so, which sends its asker to a stream (RFC 2181, 9).
                let questions = be16(&message, COUNTS_AT).map_or(0, usize::from);
                let end = (0..questions).try_fold(HEAD, |at, _| Some(skip_name(&message, at)? + 4));
                message.truncate(end.filter(|end| *end <= message.len()).unwrap_or(HEAD));
                let flags = be16(&message, FLAGS_AT).unwrap_or_default() | TRUNCATED;
                set_count(&mut message, FLAGS_AT, flags);
                (1..4).for_each(|count| set_count(&mut message, COUNTS_AT + 2 * count, 0));
                if end.is_none() {
                    set_count(&mut message, COUNTS_AT, 0);
                }
                Some((asked.asker, message))
            }
        }
    }

    /// Forgets the questions of the askers for which `is_gone` holds, whose answers nobody waits for.
    pub fn forget(&mut self, is_gone: impl Fn(&A) -> bool) {
        self.asked.retain(|_, asked| !is_gone(&asked.asker));
    }

    /// Forgets every question, as the resolver's stream that carried them ended; the log line of what it carried.
    pub fn end(&mut self) -> String {
        self.asked.clear();
        to_relayed_line(core::mem::take(&mut self.sent), core::mem::take(&mut self.answered))
    }
}

#[cfg(test)]
mod tests {
    use super::super::message::{TYPE_A, question};
    use super::*;
    use alloc::vec;

    // A question as a plain stub resolver sends it: no options.
    fn plain(id: u16) -> Vec<u8> {
        let mut message = id.to_be_bytes().to_vec();
        message.extend([1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        message.extend(b"\x07example\x03org\x00\x00\x41\x00\x01");
        message
    }

    fn with_options(id: u16, size: u16, options: &[u8]) -> Vec<u8> {
        let mut message = plain(id);
        message[11] = 1;
        message.extend([0, 0, 41]);
        message.extend(size.to_be_bytes());
        message.extend([0, 0, 0x80, 0]);
        message.extend((options.len() as u16).to_be_bytes());
        message.extend(options);
        message
    }

    // The answer a resolver gives the message it was sent: the same number and question, records of `bytes` in all, and the options sent back.
    fn answer_to(sent: &[u8], bytes: usize) -> Vec<u8> {
        let message = &sent[2..];
        let mut answer = message[..29].to_vec();
        answer[2..4].copy_from_slice(&[0x81, 0x80]);
        answer[6..8].copy_from_slice(&[0, 1]);
        answer.extend([0xC0, 12, 0, 16, 0, 1, 0, 0, 0, 60]);
        answer.extend((bytes as u16).to_be_bytes());
        answer.extend(vec![b'x'; bytes]);
        answer.extend([0, 0, 41, 4, 0xD0, 0, 0, 0, 0, 0, 0]);
        answer
    }

    #[test]
    fn question_numbered_and_padded() {
        let mut relay = Relay::default();
        let sent = relay.ask('a', false, &plain(0x1234)).expect("a question goes on");
        assert_eq!(
            (&sent[..2], &sent[2..4], sent.len(), sent[13]),
            (&[0, 128][..], &[0, 0][..], 130, 1),
            "a message of a stream, under the relay's first number, filled up to a block, with one record more"
        );
        assert_eq!(&sent[14..31], &plain(0)[12..], "the question as it came, of whatever kind");
        assert_eq!(
            sent[31..46],
            [0, 0, 41, 0x04, 0xD0, 0, 0, 0, 0, 0, 88, 0, 12, 0, 84],
            "the options the firmware's own questions carry"
        );
        let own = relay.ask('b', false, &with_options(7, 4096, &[0, 10, 0, 2, 9, 9])).unwrap();
        assert_eq!((&own[2..4], own.len(), be16(&own, 2 + 29 + 9)), (&[0, 1][..], 130, Some(6 + 4 + 78)));
        assert_eq!(own[2 + 46..2 + 50], [0, 12, 0, 78], "an asker's own options take the padding behind them");
        let padded = with_options(8, 1232, &[0, 12, 0, 3, 0, 0, 0]);
        assert_eq!(relay.ask('c', false, &padded).unwrap()[4..], padded[2..], "padding an asker made stands");
        let mut reply = plain(9);
        reply[2] |= 0x80;
        assert_eq!((relay.ask('d', false, &reply), relay.ask('d', false, &plain(9)[..11])), (None, None));
        assert_eq!(relay.asked.len(), 3, "each waits for its answer");
        let long = |bytes: usize| {
            let mut question = plain(10);
            question.resize(bytes, 0);
            question
        };
        assert!(
            relay.ask('e', true, &long(65394)).is_none(),
            "filled up it would be longer than the two bytes before a stream's message count"
        );
        assert_eq!((relay.asked.len(), relay.next), (3, 3), "and takes no number");
        let longest = relay.ask('e', true, &long(65393)).expect("the longest that fills up to a message of a stream");
        assert_eq!((&longest[..2], longest.len()), (&[0xFF, 0x80][..], 2 + 65408));
    }

    #[test]
    fn answer_fitted_to_its_asker() {
        let mut relay = Relay::default();
        let plain_sent = relay.ask('a', false, &plain(0x1234)).unwrap();
        let (asker, datagram) = relay.answer(&answer_to(&plain_sent, 100)).expect("its asker's");
        assert_eq!((asker, &datagram[..2], datagram.len(), datagram[11]), ('a', &[0x12, 0x34][..], 29 + 12 + 100, 0));
        assert!(datagram.ends_with(&[b'x'; 100]), "under the asker's number, without the options it did not send");
        assert_eq!(relay.answer(&answer_to(&plain_sent, 100)), None, "once");
        let sent = relay.ask('a', false, &plain(5)).unwrap();
        let (_, cut) = relay.answer(&answer_to(&sent, 500)).unwrap();
        assert_eq!(
            (cut.len(), &cut[..4], &cut[4..12]),
            (29, &[0, 5, 0x83, 0x80][..], &[0, 1, 0, 0, 0, 0, 0, 0][..]),
            "past 512 bytes: the question alone, marked as cut"
        );
        let sent = relay.ask('b', false, &with_options(6, 1232, &[])).unwrap();
        let (_, whole) = relay.answer(&answer_to(&sent, 500)).unwrap();
        assert_eq!(
            (whole.len(), whole[11]),
            (29 + 12 + 500 + 11, 1),
            "an asker that takes more gets it, its options with it"
        );
        let sent = relay.ask('b', false, &with_options(6, 100, &[])).unwrap();
        assert_eq!(relay.answer(&answer_to(&sent, 480)).unwrap().1.len(), 29, "no asker takes less than 512");
        let sent = relay.ask('s', true, &plain(7)).unwrap();
        let (_, framed) = relay.answer(&answer_to(&sent, 2000)).unwrap();
        assert_eq!((&framed[..2], framed.len()), (&(2041u16).to_be_bytes()[..], 2043), "over a stream, whole");
        assert!(relay.asked.is_empty());
    }

    #[test]
    fn numbers_kept_apart_and_forgotten() {
        let mut relay = Relay::default();
        let first = relay.ask(1, false, &plain(9)).unwrap();
        let second = relay.ask(2, false, &plain(9)).unwrap();
        assert_ne!(first[2..4], second[2..4], "two askers' same number");
        assert_eq!(relay.answer(&answer_to(&second, 10)).map(|(asker, _)| asker), Some(2));
        relay.forget(|asker| *asker == 1);
        assert_eq!(relay.answer(&answer_to(&first, 10)), None, "an asker that left is answered nothing");
        relay.ask(3, true, &plain(9));
        assert_eq!(
            relay.end(),
            "names: the relay's connection ended, 1 of 3 questions answered",
            "what the stream carried, told as it ends"
        );
        assert!(relay.asked.is_empty());
        for _ in 0..=u16::MAX {
            assert!(relay.ask(4, false, &plain(1)).is_some());
        }
        assert_eq!(relay.ask(4, false, &plain(1)), None, "as many as there are numbers");
        assert_eq!(
            (to_relay_line("1.1.1.1:853", 20, "TLS 1.3", 61), to_relayed_line(12, 11)),
            (
                String::from("names: the relay reached the resolver at 1.1.1.1:853 in 20 ms, TLS 1.3 after 61 ms"),
                String::from("names: the relay's connection ended, 11 of 12 questions answered")
            )
        );
        assert!(question(1, "example.org", TYPE_A).is_some_and(|own| find_options(&own).is_some()));
    }

    #[test]
    fn streams_bounded() {
        let (mut streams, most) = (Streams::default(), STREAMS as u64);
        for number in 0..most {
            assert_eq!(streams.open(), (number, None), "as many as a relay keeps stand side by side");
        }
        streams.close(1);
        assert_eq!(streams.open(), (most, None), "one that ended leaves room");
        assert_eq!(streams.open(), (most + 1, Some(0)), "past that the oldest goes for the new one");
        assert_eq!(streams.open(), (most + 2, Some(2)), "then the next oldest, one that ended passed over");
    }
}
