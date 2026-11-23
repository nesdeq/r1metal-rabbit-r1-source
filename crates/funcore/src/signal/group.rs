//! A group's keys, its state as its conversation keeps it, and its fetch from the groups server.

use super::Crypto;
use super::address::{Peer, ServiceId};
use super::service::{Request, TIMESTAMP_HEADER, basic_auth, storage_headers, storage_url};
use super::zk::Sho;
use super::zk::attributes::{KeyPair, PROFILE_KEY_GENERATORS, UID_GENERATORS, generators};
use super::zk::credential::{AuthCredential, present};
use super::zk::endorsement::{self, Endorsements};
use super::zk::ristretto::Element;
use crate::base::code::hex;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::base::time::MILLIS_PER_SECOND;
use crate::codec::http::{FORBIDDEN, GET, Head, NOT_FOUND, OK};
use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const SECRET_PARAMS: &[u8] = b"Signal_ZKGroup_20200424_GroupMasterKey_GroupSecretParams_DeriveFromMasterKey";
const NONCE_BYTES: usize = 12;
const PADDING_LENGTH_BYTES: usize = 4;
const RESERVED: u8 = 0;
const GROUPS: &str = "/v2/groups";
// A group call's token, Signal-Desktop's `getExternalGroupCredential`.
const TOKEN: &str = "/v2/groups/token";
// `ExternalGroupCredential`'s field of the token.
const TOKEN_FIELD: u32 = 1;

// The token of each group's call asked, none where it was not had.
type Tokens = Vec<(Peer, Option<String>)>;

pub(super) struct Keys {
    id: [u8; 32],
    blob_key: [u8; 32],
    pub(super) uid: KeyPair,
    profile: Element,
}

impl Keys {
    pub(super) fn new(crypto: &impl Crypto, master_key: &[u8; 32]) -> Keys {
        let mut sho = Sho::new(crypto, SECRET_PARAMS);
        sho.absorb(crypto, master_key);
        let (mut id, mut blob_key) = ([0u8; 32], [0u8; 32]);
        sho.squeeze(crypto, &mut id);
        sho.squeeze(crypto, &mut blob_key);
        let uid = KeyPair::derive(crypto, &mut sho, generators(UID_GENERATORS));
        let profile = KeyPair::derive(crypto, &mut sho, generators(PROFILE_KEY_GENERATORS)).public;
        Keys { id, blob_key, uid, profile }
    }

    fn public_params(&self) -> Vec<u8> {
        [&[RESERVED][..], &self.id, &self.uid.public.encode(), &self.profile.encode()].concat()
    }

    fn title(&self, crypto: &impl Crypto, ciphertext: &[u8]) -> Option<String> {
        let attribute = open_attribute(crypto, &self.blob_key, ciphertext)?;
        let title = Message::parse(&attribute)?.text(1)?.trim();
        (!title.is_empty()).then(|| String::from(title))
    }

    fn timer(&self, crypto: &impl Crypto, ciphertext: &[u8]) -> Option<u32> {
        let attribute = open_attribute(crypto, &self.blob_key, ciphertext)?;
        Some(Message::parse(&attribute)?.u32(3).unwrap_or(0))
    }
}

pub(super) fn id_of(crypto: &impl Crypto, master_key: &[u8; 32]) -> [u8; 32] {
    let mut sho = Sho::new(crypto, SECRET_PARAMS);
    sho.absorb(crypto, master_key);
    let mut id = [0u8; 32];
    sho.squeeze(crypto, &mut id);
    id
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Context {
    pub(super) master_key: [u8; 32],
    pub(super) revision: u32,
}

impl core::fmt::Debug for Context {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Context").field("revision", &self.revision).finish_non_exhaustive()
    }
}

impl Context {
    pub(super) fn read(message: &Message) -> Option<Context> {
        Some(Context { master_key: message.bytes(1)?.try_into().ok()?, revision: message.u32(2).unwrap_or(0) })
    }

    pub(super) fn write(master_key: &[u8; 32], revision: u32) -> Writer {
        Writer::new().bytes(1, master_key).varint(2, u64::from(revision))
    }
}

fn open_attribute(crypto: &impl Crypto, key: &[u8; 32], bytes: &[u8]) -> Option<Vec<u8>> {
    let (_, body) = bytes.split_last()?;
    let (ciphertext, nonce) = body.split_at_checked(body.len().checked_sub(NONCE_BYTES)?)?;
    let plain = crypto.siv_decrypt(key, nonce.try_into().ok()?, ciphertext)?;
    let (padding, rest) = plain.split_at_checked(PADDING_LENGTH_BYTES)?;
    let padding = usize::try_from(u32::from_be_bytes(padding.try_into().ok()?)).ok()?;
    Some(rest[..rest.len().checked_sub(padding)?].to_vec())
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum GroupUpdate {
    State(State),
    Gone,
}

#[derive(Clone, PartialEq, Eq, Default)]
pub(super) struct State {
    pub(super) revision: u32,
    pub(super) title: Option<String>,
    pub(super) members: Vec<ServiceId>,
    pub(super) admins: Vec<ServiceId>,
    pub(super) timer: u32,
    pub(super) announcements_only: bool,
    pub(super) terminated: bool,
    pub(super) endorsements: Option<Endorsements>,
}

// As `Group` tells it: the group's name and its endorsements are left out.
impl core::fmt::Debug for State {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("State")
            .field("revision", &self.revision)
            .field("members", &self.members.len())
            .field("admins", &self.admins.len())
            .field("timer", &self.timer)
            .finish_non_exhaustive()
    }
}

const ADMINISTRATOR: u64 = 2;

fn parse_state(crypto: &impl Crypto, keys: &Keys, answer: &[u8], (root, now): (&Element, u64)) -> Option<GroupUpdate> {
    let response = Message::parse(answer)?;
    let group = response.message(1)?;
    let title = group.bytes(2).and_then(|ciphertext| keys.title(crypto, ciphertext));
    let (mut members, mut admins) = (Vec::new(), Vec::new());
    for member in group.repeated(7).filter_map(Message::parse) {
        let Some(id) = member.bytes(1).and_then(|id| keys.uid.decrypt_id(crypto, id)) else { continue };
        if member.varint(2) == Some(ADMINISTRATOR) {
            admins.push(id);
        }
        members.push(id);
    }
    let timer = group.bytes(4).and_then(|ciphertext| keys.timer(crypto, ciphertext)).unwrap_or(0);
    let (announcements_only, terminated) = (group.bool(12).unwrap_or(false), group.bool(14).unwrap_or(false));
    let revision = group.u32(6).unwrap_or(0);
    let endorsements = response
        .bytes(2)
        .filter(|response| !response.is_empty())
        .and_then(|response| endorsement::parse_endorsements(crypto, response, &members, (&keys.uid, root), now));
    Some(GroupUpdate::State(State {
        revision,
        title,
        members,
        admins,
        timer,
        announcements_only,
        terminated,
        endorsements,
    }))
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SenderKey {
    pub(super) created: u64,
    pub(super) distribution: [u8; 16],
    pub(super) devices: Vec<(ServiceId, u32, u32)>,
}

impl core::fmt::Debug for SenderKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SenderKey")
            .field("created", &self.created)
            .field("devices", &self.devices.len())
            .finish_non_exhaustive()
    }
}

impl SenderKey {
    fn write(&self) -> Writer {
        self.devices.iter().fold(
            Writer::new().varint(1, self.created).bytes(2, &self.distribution),
            |writer, (account, device, registration)| {
                let device = Writer::new()
                    .bytes(1, &account.to_binary())
                    .varint(2, u64::from(*device))
                    .varint(3, u64::from(*registration));
                writer.message(3, device)
            },
        )
    }

    fn read(message: &Message) -> Option<SenderKey> {
        let devices = message
            .repeated(3)
            .filter_map(Message::parse)
            .filter_map(|device| Some((ServiceId::from_binary(device.bytes(1)?)?, device.u32(2)?, device.u32(3)?)))
            .collect();
        Some(SenderKey { created: message.varint(1)?, distribution: message.bytes(2)?.try_into().ok()?, devices })
    }
}

fn write_endorsements(endorsements: &Endorsements) -> Writer {
    endorsements.members.iter().fold(Writer::new().varint(1, endorsements.expiration), |writer, (member, point)| {
        writer.message(2, Writer::new().bytes(1, &member.to_binary()).bytes(2, point))
    })
}

fn read_endorsements(message: &Message) -> Option<Endorsements> {
    let members = message
        .repeated(2)
        .filter_map(Message::parse)
        .filter_map(|member| Some((ServiceId::from_binary(member.bytes(1)?)?, member.bytes(2)?.try_into().ok()?)))
        .collect();
    Some(Endorsements { expiration: message.varint(1)?, members })
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub(super) master_key: [u8; 32],
    revision: Option<u32>,
    message_revision: u32,
    pub(super) title: Option<String>,
    pub(super) members: Vec<ServiceId>,
    pub(super) admins: Vec<ServiceId>,
    pub(super) timer: u32,
    pub(super) announcements_only: bool,
    pub(super) terminated: bool,
    pub(super) is_sharing: bool,
    pub(super) response: u32,
    pub(super) sender_key: Option<SenderKey>,
    pub(super) endorsements: Option<Endorsements>,
    pub(super) needs_refresh: bool,
}

impl core::fmt::Debug for Group {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Group")
            .field("held", &self.revision)
            .field("named", &self.message_revision)
            .field("members", &self.members.len())
            .finish_non_exhaustive()
    }
}

impl Group {
    pub(super) const fn new(master_key: [u8; 32]) -> Group {
        Group {
            master_key,
            revision: None,
            message_revision: 0,
            title: None,
            members: Vec::new(),
            admins: Vec::new(),
            timer: 0,
            announcements_only: false,
            terminated: false,
            is_sharing: false,
            response: 0,
            sender_key: None,
            endorsements: None,
            needs_refresh: false,
        }
    }

    pub(super) fn note_revision(&mut self, revision: u32) {
        self.message_revision = self.message_revision.max(revision);
    }

    pub(super) fn due_revision(&self) -> Option<u32> {
        self.revision.is_none_or(|revision| self.message_revision > revision).then_some(self.message_revision)
    }

    pub(super) fn usable_endorsements(&self, now: u64) -> Option<&Endorsements> {
        let endorsements = self.endorsements.as_ref()?;
        (endorsements.is_usable(now / MILLIS_PER_SECOND) && self.members.iter().all(|member| endorsements.has(member)))
            .then_some(endorsements)
    }

    pub(super) fn is_behind(&self, revision: u32) -> bool {
        self.revision.is_none_or(|current| revision > current)
    }

    pub(super) fn latest_revision(&self) -> u32 {
        self.revision.map_or(self.message_revision, |revision| revision.max(self.message_revision))
    }

    pub(super) fn apply(&mut self, update: GroupUpdate) {
        match update {
            GroupUpdate::State(state) => {
                (self.revision, self.title, self.members) = (Some(state.revision), state.title, state.members);
                (self.admins, self.timer) = (state.admins, state.timer);
                (self.announcements_only, self.terminated) = (state.announcements_only, state.terminated);
                if state.endorsements.is_some() {
                    self.endorsements = state.endorsements;
                }
            }
            GroupUpdate::Gone => {
                (self.revision, self.members, self.admins) = (Some(self.message_revision), Vec::new(), Vec::new());
                self.endorsements = None;
            }
        }
        self.needs_refresh = false;
    }

    pub(super) fn write(&self) -> Writer {
        let mut writer = Writer::new()
            .bytes(1, &self.master_key)
            .maybe(self.revision.map(|revision| move |w: Writer| w.varint(2, u64::from(revision))))
            .maybe(self.title.as_deref().map(|title| move |w: Writer| w.text(3, title)))
            .varint(5, u64::from(self.message_revision))
            .varint(6, u64::from(self.timer))
            .bool(8, self.announcements_only)
            .bool(9, self.terminated)
            .bool(10, self.is_sharing)
            .varint(11, u64::from(self.response))
            .maybe(self.sender_key.as_ref().map(|key| move |w: Writer| w.message(12, key.write())))
            .maybe(
                self.endorsements
                    .as_ref()
                    .map(|endorsements| move |w: Writer| w.message(13, write_endorsements(endorsements))),
            )
            .bool(14, self.needs_refresh);
        for member in &self.members {
            writer = writer.bytes(4, &member.to_binary());
        }
        for admin in &self.admins {
            writer = writer.bytes(7, &admin.to_binary());
        }
        writer
    }

    pub(super) fn read(message: &Message) -> Option<Group> {
        Some(Group {
            master_key: message.bytes(1)?.try_into().ok()?,
            revision: message.u32(2),
            message_revision: message.u32(5).unwrap_or(0),
            title: message.text(3).map(String::from),
            members: message.repeated(4).filter_map(ServiceId::from_binary).collect(),
            admins: message.repeated(7).filter_map(ServiceId::from_binary).collect(),
            timer: message.u32(6).unwrap_or(0),
            announcements_only: message.bool(8).unwrap_or(false),
            terminated: message.bool(9).unwrap_or(false),
            is_sharing: message.bool(10).unwrap_or(false),
            response: message.u32(11).unwrap_or(0),
            sender_key: message.message(12).and_then(|key| SenderKey::read(&key)),
            endorsements: message.message(13).and_then(|endorsements| read_endorsements(&endorsements)),
            needs_refresh: message.bool(14).unwrap_or(false),
        })
    }
}

/// A fetch from the groups server under the day's credential, a request a group: the states of groups, then the tokens of groups' calls.
pub struct GroupFetch {
    credential: AuthCredential,
    groups: Vec<(Peer, [u8; 32])>,
    tokens: Vec<(Peer, [u8; 32])>,
    next: usize,
    keys: Option<Keys>,
    updates: Vec<(Peer, GroupUpdate)>,
    given: Vec<(Peer, Option<String>)>,
    refusals: Vec<u16>,
    failure: Option<String>,
}

impl core::fmt::Debug for GroupFetch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Fetching")
            .field("groups", &self.groups.len())
            .field("tokens", &self.tokens.len())
            .field("asked", &self.next)
            .finish_non_exhaustive()
    }
}

impl GroupFetch {
    pub(super) const fn new(credential: AuthCredential, groups: Vec<(Peer, [u8; 32])>) -> GroupFetch {
        GroupFetch {
            credential,
            groups,
            tokens: Vec::new(),
            next: 0,
            keys: None,
            updates: Vec::new(),
            given: Vec::new(),
            refusals: Vec::new(),
            failure: None,
        }
    }

    // The fetch with the tokens of these groups' calls asked behind their states.
    pub(super) fn with_tokens(self, tokens: Vec<(Peer, [u8; 32])>) -> GroupFetch {
        GroupFetch { tokens, ..self }
    }

    // The group asked next, and whether its call's token is asked rather than its state.
    fn asked(&self, at: usize) -> Option<(&(Peer, [u8; 32]), bool)> {
        match self.groups.get(at) {
            Some(group) => Some((group, false)),
            None => self.tokens.get(at - self.groups.len()).map(|group| (group, true)),
        }
    }

    /// The next request, `GET /v2/groups` or, for a call's token, `GET /v2/groups/token`, under the group's public params and a presentation of the credential; none once every group was asked or the fetch failed.
    pub fn next_request(&mut self, crypto: &impl Crypto) -> Option<Request<'static>> {
        if self.failure.is_some() {
            return None;
        }
        let ((_, master_key), is_token) = self.asked(self.next)?;
        let keys = Keys::new(crypto, master_key);
        let mut randomness = [0u8; 32];
        let presentation = if crypto.random(&mut randomness) {
            present(crypto, &self.credential, &keys.uid, &randomness)
        } else {
            None
        };
        let Some(presentation) = presentation else {
            self.failure = Some(String::from("no presentation of the credential"));
            return None;
        };
        let headers = storage_headers(basic_auth(&hex(&keys.public_params()), &hex(&presentation)));
        self.next += 1;
        self.keys = Some(keys);
        let path = if is_token { TOKEN } else { GROUPS };
        Some(Request::new(GET, storage_url(path), headers, Cow::Borrowed(&[])))
    }

    /// Takes the groups server's answer at `now`, ms, to the last request.
    pub fn take_answer(&mut self, crypto: &impl Crypto, head: &Head, body: &[u8], now: u64) {
        let Some(keys) = self.keys.take() else { return };
        let Some((&(peer, _), is_token)) = self.asked(self.next - 1) else { return };
        if is_token {
            let token = Message::parse(body).and_then(|answer| answer.text(TOKEN_FIELD).map(String::from));
            let token = token.filter(|_| head.status == OK);
            if token.is_none() {
                self.refusals.push(head.status);
            }
            return self.given.push((peer, token));
        }
        let is_from_signal = head.header_values(TIMESTAMP_HEADER).next().is_some();
        let update = match head.status {
            OK => parse_state(crypto, &keys, body, (&self.credential.issuer.endorsement_root, now / MILLIS_PER_SECOND)),
            // The owner is no member, or the group is no more.
            FORBIDDEN | NOT_FOUND if is_from_signal => Some(GroupUpdate::Gone),
            _ => None,
        };
        match update {
            Some(update) => self.updates.push((peer, update)),
            None => self.refusals.push(head.status),
        }
    }

    /// Ends the fetch, a request having failed for this reason.
    pub fn abort(&mut self, reason: String) {
        self.failure = Some(reason);
    }

    // The states taken, each call's token asked, none where it was not had, and the log line.
    pub(super) fn finish(self) -> (Vec<(Peer, GroupUpdate)>, Tokens, String) {
        let states = self.updates.iter().filter(|(_, update)| matches!(update, GroupUpdate::State(_))).count();
        let mut line =
            format!("{states} groups from the groups server, {} the owner is in no more", self.updates.len() - states);
        let mut given = self.given;
        if !self.tokens.is_empty() {
            let had = given.iter().filter(|(_, token)| token.is_some()).count();
            let _ = write!(line, ", {had} of {} calls' tokens", self.tokens.len());
            given.extend(self.tokens[given.len()..].iter().map(|(peer, _)| (*peer, None)));
        }
        for status in self.refusals {
            let _ = write!(line, ", one answered {status}");
        }
        if let Some(reason) = self.failure {
            let _ = write!(line, ", broken off: {reason}");
        }
        (self.updates, given, line)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::tests::{Exact, Standin};
    use super::super::zk::credential::parse_credential;
    use super::super::zk::credential::tests::{Server, answer};
    use super::super::zk::endorsement::tests::issue;
    use super::*;
    use crate::base::time::SECONDS_PER_DAY;

    fn seal_attribute(key: &[u8; 32], attribute: Writer, padding: usize) -> Vec<u8> {
        let plain = [&(padding as u32).to_be_bytes()[..], &attribute.finish(), &alloc::vec![0; padding]].concat();
        [Standin::seal(key, &plain), alloc::vec![5; NONCE_BYTES], alloc::vec![0]].concat()
    }

    pub(in crate::signal) fn state_answer(master_key: &[u8; 32], state: &State) -> Vec<u8> {
        endorsed_answer(master_key, state, None)
    }

    pub(in crate::signal) fn endorsed_answer(
        master_key: &[u8; 32],
        state: &State,
        endorsements: Option<&[u8]>,
    ) -> Vec<u8> {
        let crypto = Standin::default();
        let keys = Keys::new(&crypto, master_key);
        let title = state.title.as_deref().unwrap_or_default();
        let mut group = Writer::new()
            .bytes(1, &keys.public_params())
            .bytes(2, &seal_attribute(&keys.blob_key, Writer::new().text(1, title), 3))
            .bytes(4, &seal_attribute(&keys.blob_key, Writer::new().varint(3, u64::from(state.timer)), 2))
            .varint(6, u64::from(state.revision))
            .bool(12, state.announcements_only)
            .bool(14, state.terminated);
        for member in &state.members {
            let role = if state.admins.contains(member) { ADMINISTRATOR } else { 1 };
            group = group.message(7, Writer::new().bytes(1, &keys.uid.encrypt_id(&crypto, member)).varint(2, role));
        }
        Writer::new()
            .message(1, group)
            .maybe(endorsements.map(|response| move |w: Writer| w.bytes(2, response)))
            .finish()
    }

    pub(in crate::signal) fn endorsement_server(crypto: &Standin) -> (Server, u64) {
        (Server::new(crypto, &[1; 32]), (20_000 * SECONDS_PER_DAY + 3_600) * MILLIS_PER_SECOND)
    }

    pub(in crate::signal) fn endorsements_of(
        crypto: &Standin,
        server: &Server,
        master_key: &[u8; 32],
        members: &[ServiceId],
        now: u64,
    ) -> Vec<u8> {
        let key = Keys::new(crypto, master_key).uid;
        let expiration = (now / MILLIS_PER_SECOND / SECONDS_PER_DAY + 1) * SECONDS_PER_DAY;
        issue(crypto, server, (members, &key), expiration, &[3; 32])
    }

    pub(in crate::signal) fn group_answer(
        master_key: &[u8; 32],
        revision: u32,
        title: &str,
        members: &[ServiceId],
    ) -> Vec<u8> {
        let state = State { revision, title: Some(title.into()), members: members.to_vec(), ..State::default() };
        state_answer(master_key, &state)
    }

    pub(in crate::signal) fn head(status: u16, is_from_signal: bool) -> Head {
        let stamp = if is_from_signal { "X-Signal-Timestamp: 1700000000000\r\n" } else { "" };
        Head::parse(format!("HTTP/1.1 {status} X\r\n{stamp}Content-Length: 0\r\n\r\n").as_bytes()).unwrap().unwrap().0
    }

    pub(in crate::signal) fn credential(crypto: &Standin) -> AuthCredential {
        let server = Server::new(crypto, &[1; 32]);
        let (aci, pni) = ([0xA1; 16], [0xB2; 16]);
        let day = 20_000 * SECONDS_PER_DAY;
        let bytes = server.issue(crypto, (&ServiceId::Aci(aci), &ServiceId::Pni(pni)), day, &[2; 32]);
        parse_credential(crypto, &answer(&bytes, day, Some(pni)), (aci, None), day, &server.issuer)
            .expect("the credential")
    }

    #[test]
    fn group_id_and_public_params() {
        let crypto = Exact::default();
        let keys = Keys::new(&crypto, &[4; 32]);
        assert_eq!(keys.id, id_of(&crypto, &[4; 32]), "the id is squeezed first");
        assert_ne!(keys.id, keys.blob_key, "then the key of the attributes");
        let public = keys.public_params();
        assert_eq!((public.len(), public[0], &public[1..33]), (97, 0, &keys.id[..]));
    }

    #[test]
    fn group_state_and_revision() {
        let crypto = Standin::default();
        let (anna, boris) = (ServiceId::Aci([1; 16]), ServiceId::Aci([2; 16]));
        let mut group = Group::new([4; 32]);
        assert_eq!(group.due_revision(), Some(0), "none held");
        let keys = Keys::new(&crypto, &group.master_key);
        let root = super::super::zk::credential::Issuer::production().endorsement_root;
        let state = |keys: &Keys, answer: &[u8]| parse_state(&crypto, keys, answer, (&root, 0));
        let update = state(&keys, &group_answer(&group.master_key, 5, " Climbing ", &[anna, boris])).unwrap();
        group.apply(update);
        assert_eq!(
            (group.title.as_deref(), &group.members[..], group.due_revision()),
            (Some("Climbing"), &[anna, boris][..], None)
        );
        group.note_revision(4);
        assert_eq!((group.due_revision(), group.latest_revision()), (None, 5), "an earlier revision named");
        group.note_revision(6);
        assert_eq!((group.due_revision(), group.latest_revision()), (Some(6), 6), "a later one");
        let other = Keys::new(&crypto, &[9; 32]);
        let Some(GroupUpdate::State(State { title, members, .. })) =
            state(&other, &group_answer(&[4; 32], 5, "Climbing", &[anna]))
        else {
            panic!("a state")
        };
        assert_eq!((title, members), (None, Vec::new()), "what other keys do not open is left out");
        let expected = State {
            revision: 6,
            title: Some("Climbing".into()),
            members: alloc::vec![anna, boris],
            admins: alloc::vec![boris],
            timer: 3600,
            announcements_only: true,
            terminated: true,
            endorsements: None,
        };
        assert_eq!(
            state(&keys, &state_answer(&group.master_key, &expected)),
            Some(GroupUpdate::State(expected.clone())),
            "the administrators by their role, the timer opened with the group's key, announcements only, ended with its members"
        );
        assert_eq!(
            format!("{:?}", GroupUpdate::State(expected.clone())),
            "State(State { revision: 6, members: 2, admins: 1, timer: 3600, .. })",
            "a group's name is told nowhere it is formatted"
        );
        group.apply(GroupUpdate::State(expected));
        assert_eq!(
            (group.timer, &group.admins[..], group.announcements_only, group.terminated),
            (3600, &[boris][..], true, true)
        );
        (group.is_sharing, group.response, group.needs_refresh) = (true, 1, true);
        group.sender_key =
            Some(SenderKey { created: 7, distribution: [3; 16], devices: alloc::vec![(anna, 2, 0x3FFF)] });
        group.endorsements = Some(Endorsements { expiration: 86_400, members: alloc::vec![(anna, [5; 32])] });
        let parsed = Group::read(&Message::parse(&group.write().finish()).unwrap());
        assert_eq!(
            parsed.as_ref(),
            Some(&group),
            "kept as written, its administrators, timer, announcements, end, sharing, answer, sender key, endorsements and refresh too"
        );
        assert!(
            !group.is_behind(6) && group.is_behind(7),
            "a message of a later revision than the one held waits for it"
        );
        group.apply(GroupUpdate::Gone);
        assert_eq!((group.members.len(), group.due_revision(), group.title.as_deref()), (0, None, Some("Climbing")));
        assert!(
            group.endorsements.is_none() && !group.needs_refresh && group.sender_key.is_some(),
            "no endorsements left, the sender key kept"
        );
        group.note_revision(7);
        assert_eq!(group.due_revision(), Some(7), "named anew, it is fetched again");
        let parsed = Group::read(&Message::parse(&group.write().finish()).unwrap());
        assert_eq!(parsed, Some(group), "kept as written");
    }

    #[test]
    fn group_endorsements() {
        let crypto = Standin::default();
        let (own, anna, boris) = (ServiceId::Aci([0xA1; 16]), ServiceId::Aci([1; 16]), ServiceId::Aci([2; 16]));
        let (server, now) = endorsement_server(&crypto);
        let state = State { revision: 2, members: alloc::vec![own, anna], ..State::default() };
        let response = endorsements_of(&crypto, &server, &[4; 32], &[own, anna], now);
        let answer = endorsed_answer(&[4; 32], &state, Some(&response));
        let peer = Peer::Group(id_of(&crypto, &[4; 32]));
        let mut fetch = GroupFetch::new(credential(&crypto), alloc::vec![(peer, [4; 32])]);
        fetch.next_request(&crypto).expect("the group asked");
        fetch.take_answer(&crypto, &head(200, true), &answer, now);
        let (mut updates, _, _) = fetch.finish();
        let Some((_, GroupUpdate::State(state))) = updates.pop() else { panic!("its state") };
        let endorsements = state.endorsements.clone().expect("endorsements that check under the server's root key");
        assert!(endorsements.has(&own) && endorsements.has(&anna) && !endorsements.has(&boris));
        let other = Server::new(&crypto, &[2; 32]).issuer.endorsement_root;
        let Some(GroupUpdate::State(other_state)) =
            parse_state(&crypto, &Keys::new(&crypto, &[4; 32]), &answer, (&other, now / MILLIS_PER_SECOND))
        else {
            panic!("a state")
        };
        assert_eq!(
            (other_state.endorsements, other_state.members),
            (None, alloc::vec![own, anna]),
            "under another root key: none, the state taken"
        );
        let mut group = Group::new([4; 32]);
        group.needs_refresh = true;
        group.apply(GroupUpdate::State(state));
        assert!(group.usable_endorsements(now).is_some() && !group.needs_refresh, "used, no longer to be asked anew");
        let hour = 60 * 60 * MILLIS_PER_SECOND;
        assert!(
            group.usable_endorsements(now + 21 * hour).is_none(),
            "two hours or less before they run out, as Signal-Desktop refuses them"
        );
        group.apply(GroupUpdate::State(State {
            revision: 3,
            members: alloc::vec![own, anna, boris],
            ..State::default()
        }));
        assert!(
            group.endorsements.is_some() && group.usable_endorsements(now).is_none(),
            "kept where none came, but a member has none"
        );
    }

    #[test]
    fn fetch_requests_and_refusals() {
        let crypto = Standin::default();
        let anna = ServiceId::Aci([1; 16]);
        let groups =
            [[4u8; 32], [5; 32], [6; 32], [7; 32]].map(|master| (Peer::Group(id_of(&crypto, &master)), master));
        let mut fetch = GroupFetch::new(credential(&crypto), groups.to_vec());
        let request = fetch.next_request(&crypto).expect("the first");
        assert_eq!((request.method, request.url.as_str()), ("GET", "https://storage.signal.org/v2/groups"));
        let (name, authorization) = &request.headers[0];
        let text = crate::base::code::unbase64(authorization.strip_prefix("Basic ").unwrap()).unwrap();
        let (public, presentation) = core::str::from_utf8(&text).unwrap().split_once(':').unwrap();
        assert_eq!(
            (name.as_str(), public, presentation.len(), &presentation[..2]),
            ("Authorization", hex(&Keys::new(&crypto, &[4; 32]).public_params()).as_str(), 2 * 633, "03"),
            "the group's public params and a presentation of 633 bytes, in hex"
        );
        fetch.take_answer(&crypto, &head(200, true), &group_answer(&[4; 32], 3, "Climbing", &[anna]), 0);
        fetch.next_request(&crypto).expect("the second");
        fetch.take_answer(&crypto, &head(403, true), b"", 0);
        fetch.next_request(&crypto).expect("the third");
        fetch.take_answer(&crypto, &head(403, false), b"", 0);
        fetch.next_request(&crypto).expect("the fourth");
        fetch.take_answer(&crypto, &head(500, true), b"", 0);
        assert!(fetch.next_request(&crypto).is_none(), "every group asked");
        let (updates, _, line) = fetch.finish();
        assert_eq!(
            updates,
            [
                (
                    groups[0].0,
                    GroupUpdate::State(State {
                        revision: 3,
                        title: Some("Climbing".into()),
                        members: alloc::vec![anna],
                        ..State::default()
                    })
                ),
                (groups[1].0, GroupUpdate::Gone),
            ],
            "a 403 counts only from Signal's own servers"
        );
        assert_eq!(
            line,
            "1 groups from the groups server, 1 the owner is in no more, one answered 403, one answered 500"
        );
        let mut failed = GroupFetch::new(credential(&crypto), groups.to_vec());
        failed.next_request(&crypto);
        failed.abort(String::from("timed out"));
        assert!(failed.next_request(&crypto).is_none());
        assert_eq!(
            failed.finish().2,
            "0 groups from the groups server, 0 the owner is in no more, broken off: timed out"
        );
    }

    #[test]
    fn call_tokens() {
        let crypto = Standin::default();
        let anna = ServiceId::Aci([1; 16]);
        let [state, first, second, third] =
            [[4u8; 32], [5; 32], [6; 32], [7; 32]].map(|master| (Peer::Group(id_of(&crypto, &master)), master));
        let mut fetch =
            GroupFetch::new(credential(&crypto), alloc::vec![state]).with_tokens(alloc::vec![first, second]);
        let request = fetch.next_request(&crypto).expect("the state first");
        assert_eq!(request.url, "https://storage.signal.org/v2/groups", "a group's state before any token");
        fetch.take_answer(&crypto, &head(200, true), &group_answer(&[4; 32], 3, "Climbing", &[anna]), 0);
        let request = fetch.next_request(&crypto).expect("then the first token");
        let headers = request.headers.clone();
        let text = crate::base::code::unbase64(headers[0].1.strip_prefix("Basic ").unwrap()).unwrap();
        let (public, _) = core::str::from_utf8(&text).unwrap().split_once(':').unwrap();
        assert_eq!(
            (request.url.as_str(), public, headers[1].1.as_str()),
            (
                "https://storage.signal.org/v2/groups/token",
                hex(&Keys::new(&crypto, &[5; 32]).public_params()).as_str(),
                "application/x-protobuf"
            ),
            "under the group's public params and a presentation, as a state is asked"
        );
        fetch.take_answer(&crypto, &head(200, true), &Writer::new().text(TOKEN_FIELD, "a:b").finish(), 0);
        fetch.next_request(&crypto).expect("the second token");
        fetch.take_answer(&crypto, &head(403, true), b"", 0);
        assert!(fetch.next_request(&crypto).is_none(), "every one asked");
        let (updates, given, line) = fetch.finish();
        assert_eq!(
            (updates.len(), given),
            (1, alloc::vec![(first.0, Some(String::from("a:b"))), (second.0, None)]),
            "the state taken, each token asked answered, none where it was refused"
        );
        assert_eq!(
            line,
            "1 groups from the groups server, 0 the owner is in no more, 1 of 2 calls' tokens, one answered 403"
        );
        let mut broken = GroupFetch::new(credential(&crypto), alloc::vec![state]).with_tokens(alloc::vec![third]);
        broken.next_request(&crypto);
        broken.abort(String::from("timed out"));
        assert_eq!(broken.finish().1, [(third.0, None)], "a token not asked is answered all the same");
        assert_eq!(
            format!("{:?}", GroupFetch::new(credential(&crypto), alloc::vec![]).with_tokens(alloc::vec![third])),
            "Fetching { groups: 0, tokens: 1, asked: 0, .. }",
            "counts alone where formatted"
        );
    }
}
