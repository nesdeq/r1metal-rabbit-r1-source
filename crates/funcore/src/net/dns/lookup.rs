use super::{Host, NoAddress, Unresolved};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::net::IpAddr;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Stage {
    Checked,
    Waiting(u64),
    Done,
}

// A name's addresses, or why it has none, once known.
type Slot<E> = Option<Result<Vec<IpAddr>, Unresolved<E>>>;

/// The addresses of names in their order: an address or no valid name as it stands, the cache's at once, the rest from an asking, each name asked for once however it is written.
pub struct Lookup<E> {
    // Each name as it is asked for, none for one that is settled as it stands.
    names: Vec<Option<String>>,
    results: Vec<Slot<E>>,
    stage: Stage,
    lines: Vec<String>,
}

impl<E: Clone> Lookup<E> {
    pub(super) fn new(names: &[&str], find: impl FnMut(&str) -> Option<Vec<IpAddr>>) -> Lookup<E> {
        let (names, results) = names
            .iter()
            .map(|name| match Host::parse(name) {
                Host::Address(address) => (None, Some(Ok(vec![address]))),
                Host::Invalid => (None, Some(Err(Unresolved::Invalid))),
                Host::Name(name) => (Some(name), None),
            })
            .unzip();
        let mut lookup = Lookup { names, results, stage: Stage::Checked, lines: Vec::new() };
        lookup.check_cache(find);
        lookup
    }

    pub(super) const fn stage(&self) -> Stage {
        self.stage
    }

    pub(super) const fn await_turn(&mut self, end_count: u64) {
        self.stage = Stage::Waiting(end_count);
    }

    pub(super) const fn note_done(&mut self) {
        self.stage = Stage::Done;
    }

    pub(super) fn add_lines(&mut self, lines: Vec<String>) {
        self.lines.extend(lines);
    }

    /// The log lines since the last call.
    pub fn take_lines(&mut self) -> Vec<String> {
        core::mem::take(&mut self.lines)
    }

    fn missing(&mut self) -> impl Iterator<Item = (&Option<String>, &mut Slot<E>)> {
        self.names.iter().zip(&mut self.results).filter(|(_, result)| result.is_none())
    }

    pub(super) fn check_cache(&mut self, mut find: impl FnMut(&str) -> Option<Vec<IpAddr>>) {
        for (name, result) in self.missing() {
            *result = name.as_deref().and_then(&mut find).map(Ok);
        }
    }

    pub(super) fn has_missing(&self) -> bool {
        self.results.iter().any(Option::is_none)
    }

    // The names to ask for, each once, in the order they first come.
    pub(super) fn missing_names(&self) -> Vec<String> {
        let mut missing: Vec<String> = Vec::new();
        let names = self.names.iter().zip(&self.results).filter(|(_, result)| result.is_none());
        for name in names.filter_map(|(name, _)| name.as_ref()) {
            if !missing.contains(name) {
                missing.push(name.clone());
            }
        }
        missing
    }

    // Takes what an asking for `missing_names` came to: each name's addresses, or why none, in their order; or why the asking could not be made.
    pub(super) fn take_results(&mut self, results: Result<Vec<Result<Vec<IpAddr>, NoAddress>>, E>) {
        let asked = self.missing_names();
        match results {
            Ok(results) => {
                for (asked, result) in asked.iter().zip(results) {
                    let slots = self.missing().filter(|(name, _)| name.as_ref() == Some(asked));
                    slots.for_each(|(_, slot)| *slot = Some(result.clone().map_err(Unresolved::NoAddress)));
                }
            }
            Err(reason) => self.fail(reason),
        }
    }

    /// Fails every name still missing for `reason`: the asking could not be made, or the stay ended while it waited for one.
    pub fn fail(&mut self, reason: E) {
        self.missing().for_each(|(_, slot)| *slot = Some(Err(Unresolved::Failed(reason.clone()))));
        self.note_done();
    }

    /// Each name's addresses in its place, or why it has none; one the asking left out has no answer.
    pub fn into_results(self) -> Vec<Result<Vec<IpAddr>, Unresolved<E>>> {
        let unanswered = || Err(Unresolved::NoAddress(NoAddress::Unanswered));
        self.results.into_iter().map(|result| result.unwrap_or_else(unanswered)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(last: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last])
    }

    #[test]
    fn results_in_place() {
        let names = ["a.example", "B.Example.", "c.example", "d.example", "b.example", "192.0.2.9", "a..b"];
        let mut lookup: Lookup<&str> = Lookup::new(&names, |name| (name == "c.example").then(|| vec![address(3)]));
        assert!(lookup.has_missing());
        assert_eq!(
            lookup.missing_names(),
            ["a.example", "b.example", "d.example"],
            "what is kept is not asked for, nor an address or what is no name, and a name once however it is written"
        );
        lookup.check_cache(|name| (name == "d.example").then(|| vec![address(4)]));
        assert_eq!(lookup.missing_names(), ["a.example", "b.example"], "nor what an asking made meanwhile learned");
        lookup.take_results(Ok(vec![Ok(vec![address(1)]), Err(NoAddress::Absent)]));
        assert!(!lookup.has_missing());
        assert_eq!(
            lookup.into_results(),
            [
                Ok(vec![address(1)]),
                Err(Unresolved::NoAddress(NoAddress::Absent)),
                Ok(vec![address(3)]),
                Ok(vec![address(4)]),
                Err(Unresolved::NoAddress(NoAddress::Absent)),
                Ok(vec![address(9)]),
                Err(Unresolved::Invalid)
            ],
            "each in its place, a name asked for once told wherever it stands"
        );
        let mut short: Lookup<&str> = Lookup::new(&names[..2], |_| None);
        short.take_results(Ok(vec![Ok(vec![address(1)])]));
        assert_eq!(
            short.into_results(),
            [Ok(vec![address(1)]), Err(Unresolved::NoAddress(NoAddress::Unanswered))],
            "a name the asking left out has no answer"
        );
        let mut unasked: Lookup<&str> = Lookup::new(&names[..2], |_| None);
        unasked.take_results(Err("silent"));
        assert_eq!(
            (unasked.stage(), unasked.into_results()),
            (Stage::Done, vec![Err(Unresolved::Failed("silent")), Err(Unresolved::Failed("silent"))]),
            "why an asking could not be made is every name's"
        );
        let cached: Lookup<&str> = Lookup::new(&names[..1], |_| Some(vec![address(9)]));
        assert!(!cached.has_missing() && cached.missing_names().is_empty(), "where all is kept nothing is asked");
        let empty: Lookup<&str> = Lookup::new(&[], |_| None);
        assert!(!empty.has_missing() && empty.into_results().is_empty());
    }
}
