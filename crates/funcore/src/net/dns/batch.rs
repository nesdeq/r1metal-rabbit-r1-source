use super::message::{Answer, TYPE_A, TYPE_AAAA, parse_answer, question, to_frame};
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

const MAX_NAMES: usize = 1 << 15;

/// Why names are not asked for in one batch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BatchError {
    TooMany,
    InvalidName,
}

impl BatchError {
    /// The error text.
    pub const fn message(self) -> &'static str {
        match self {
            BatchError::TooMany => "more names than one asking has room for",
            BatchError::InvalidName => "not a name a server can have",
        }
    }
}

/// Why a name has no addresses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoAddress {
    Unanswered,
    ServerFailed,
    Absent,
}

impl NoAddress {
    /// The error text.
    pub const fn message(self) -> &'static str {
        match self {
            NoAddress::Unanswered => "a name without its two answers",
            NoAddress::ServerFailed => "the resolver could not say",
            NoAddress::Absent => "the name has no address",
        }
    }
}

fn interleave(v4: &Answer, v6: &Answer) -> Vec<IpAddr> {
    let turns = v4.addresses.len().max(v6.addresses.len());
    (0..turns).flat_map(|turn| [v6.addresses.get(turn), v4.addresses.get(turn)]).flatten().copied().collect()
}

fn to_addresses(both: &[Option<Answer>]) -> Result<(Vec<IpAddr>, u32), NoAddress> {
    let [Some(v4), Some(v6)] = both else {
        return Err(NoAddress::Unanswered);
    };
    let addresses = interleave(v4, v6);
    if addresses.is_empty() {
        return Err(if v4.is_failed || v6.is_failed { NoAddress::ServerFailed } else { NoAddress::Absent });
    }
    let with_addresses = [v4, v6].into_iter().filter(|answer| !answer.addresses.is_empty());
    Ok((addresses, with_addresses.map(|answer| answer.seconds).min().unwrap_or(0)))
}

/// Names asked for in one go: a name's A question carries twice its index as its number, its AAAA one that plus one.
pub struct Batch {
    answers: Vec<Option<Answer>>,
    remaining: usize,
}

impl Batch {
    /// The questions for the addresses of names, in one piece as they go over a stream, and the batch that gathers their answers.
    pub fn new(names: &[String]) -> Result<(Vec<u8>, Batch), BatchError> {
        if names.len() > MAX_NAMES {
            return Err(BatchError::TooMany);
        }
        let mut questions = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let number = 2 * index as u16;
            let both = (question(number, name, TYPE_A), question(number + 1, name, TYPE_AAAA));
            let (Some(a), Some(aaaa)) = both else {
                return Err(BatchError::InvalidName);
            };
            questions.extend(to_frame(&a));
            questions.extend(to_frame(&aaaa));
        }
        let count = 2 * names.len();
        Ok((questions, Batch { answers: alloc::vec![None; count], remaining: count }))
    }

    /// Takes a message: an answer nobody asked for, or a second one, is skipped; none for what is no answer.
    pub fn take_message(&mut self, message: &[u8]) -> Option<()> {
        let answer = parse_answer(message)?;
        if let Some(slot) = self.answers.get_mut(usize::from(answer.id)).filter(|slot| slot.is_none()) {
            (*slot, self.remaining) = (Some(answer), self.remaining - 1);
        }
        Some(())
    }

    /// True once every question has its answer.
    pub const fn is_answered(&self) -> bool {
        self.remaining == 0
    }

    /// What the two answers for each name come to, in the order of the names.
    pub fn results(&self) -> Vec<Result<(Vec<IpAddr>, u32), NoAddress>> {
        self.answers.chunks(2).map(to_addresses).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::message::samples::reply_message;
    use super::*;
    use alloc::vec;

    #[test]
    fn batch_gathers_answers() {
        let names = ["a.example", "b.example", "c.example"].map(String::from);
        let (questions, mut batch) = Batch::new(&names).unwrap();
        let frame = |id: u16, name: &str, kind: u16| to_frame(&question(id, name, kind).unwrap());
        assert_eq!(
            questions,
            [
                frame(0, "a.example", TYPE_A),
                frame(1, "a.example", TYPE_AAAA),
                frame(2, "b.example", TYPE_A),
                frame(3, "b.example", TYPE_AAAA),
                frame(4, "c.example", TYPE_A),
                frame(5, "c.example", TYPE_AAAA)
            ]
            .concat(),
            "all questions in one piece, a name's IPv4 one first"
        );
        let v6 = [0x20, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7];
        let v4: [&[u8]; 3] = [&[192, 0, 2, 1], &[192, 0, 2, 2], &[192, 0, 2, 3]];
        assert_eq!(batch.take_message(&reply_message(3, 0x8180, &[])), Some(()), "b has no IPv6 address");
        assert_eq!(batch.take_message(&reply_message(1, 0x8180, &[(28, 30, &v6)])), Some(()));
        assert_eq!(
            batch.take_message(&reply_message(1, 0x8180, &[])),
            Some(()),
            "a second answer to a question is passed over"
        );
        assert_eq!(
            batch.take_message(&reply_message(9, 0x8180, &[(1, 60, v4[0])])),
            Some(()),
            "and one to a question nobody asked"
        );
        assert_eq!(batch.take_message(&reply_message(0, 0x0100, &[])), None, "a question is no answer");
        assert_eq!(batch.results()[0], Err(NoAddress::Unanswered), "a name whose answers have not all come has none");
        batch.take_message(&reply_message(0, 0x8180, &[(1, 60, v4[0]), (1, 60, v4[1]), (1, 60, v4[2])]));
        batch.take_message(&reply_message(2, 0x8182, &[]));
        batch.take_message(&reply_message(4, 0x8180, &[]));
        assert!(!batch.is_answered());
        batch.take_message(&reply_message(5, 0x8183, &[]));
        assert!(batch.is_answered());
        let expected = [IpAddr::from(v6), IpAddr::from([192, 0, 2, 1]), [192, 0, 2, 2].into(), [192, 0, 2, 3].into()];
        assert_eq!(
            batch.results(),
            [Ok((expected.to_vec(), 30)), Err(NoAddress::ServerFailed), Err(NoAddress::Absent)],
            "a family in turn, an IPv6 one first, kept as long as the answer that allows less; a name without an address has none, whether the resolver says so or cannot say"
        );
        let mut single = Batch::new(&names[..1]).unwrap().1;
        single.take_message(&reply_message(0, 0x8180, &[(1, 60, v4[0])]));
        single.take_message(&reply_message(1, 0x8180, &[]));
        assert_eq!(
            single.results(),
            [Ok((vec![IpAddr::from([192, 0, 2, 1])], 60))],
            "an answer without addresses allows any time"
        );
        assert_eq!(Batch::new(&[]).map(|(questions, batch)| (questions.len(), batch.is_answered())), Ok((0, true)));
        assert_eq!(Batch::new(&["a..b".into()]).err(), Some(BatchError::InvalidName));
        assert_eq!(Batch::new(&vec![String::from("a.example"); MAX_NAMES + 1]).err(), Some(BatchError::TooMany));
    }

    #[test]
    fn error_texts() {
        assert_eq!(
            [BatchError::TooMany, BatchError::InvalidName].map(BatchError::message),
            ["more names than one asking has room for", "not a name a server can have"]
        );
        assert_eq!(
            [NoAddress::Unanswered, NoAddress::ServerFailed, NoAddress::Absent].map(NoAddress::message),
            ["a name without its two answers", "the resolver could not say", "the name has no address"]
        );
    }
}
