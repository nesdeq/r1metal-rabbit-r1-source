//! One stay on the account's socket: its frames, errands, keepalive, pre-key upkeep and fetches, timed by the paced clock.

use super::Crypto;
use super::account::Identity;
use super::address::{Address, Peer};
use super::attachment::{Finish, Job};
use super::command::Command;
use super::content::DecryptionError;
use super::device::{Access, Device, DropReason, Event, Order};
use super::errand::{Errand, Outcome, Step};
use super::group::GroupFetch;
use super::keys::CHECK_RETRY;
use super::service::{self, Frame};
use super::zk::credential::{AuthCredential, Issuer, parse_credential};
use crate::base::time::MILLIS_PER_SECOND;
use crate::base::time::SECONDS_PER_DAY;
use crate::codec::http::{GET, OK, PUT, SUCCESSFUL};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const KEEPALIVE_PERIOD: u64 = 30_000;
const KEEPALIVE_TIMEOUT: u64 = 30_000;
const ANSWER_TIMEOUT: u64 = 60_000;
const RETRY_DELAY: u64 = 60_000;
const STORAGE_PERIOD: u64 = SECONDS_PER_DAY * MILLIS_PER_SECOND;
const RESEND_UNIT: u64 = 1_000;
const RESEND_SPREAD: u8 = 5;
/// Why a stay ends where the socket has stopped answering.
pub const SILENT: &str = "Silent";

/// The log's line on a delivery that was dropped.
pub fn to_dropped_line(reason: &DropReason) -> String {
    format!("signal: a delivery dropped: {reason:?}")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    Errand,
    Keepalive,
    Counts(Identity),
    Upload(Identity, (Option<u32>, Option<u32>)),
    Capabilities,
    StorageAuth,
    GroupAuth(u64),
    Certificate(bool),
    Relays,
}

/// What a stay wants of the shell.
pub enum Action {
    Frame(Vec<u8>),
    // A frame of the unauthenticated socket, which the shell opens for it, handing its answers back as the account's socket's.
    Anonymous(Vec<u8>),
    // The storage service is to be asked under this authorization (`storage::StorageRead`).
    Names(String),
    // The groups server is to be asked (`group::GroupFetch`).
    Groups(Box<GroupFetch>),
}

impl core::fmt::Debug for Action {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Action::Frame(_) => "Frame",
            Action::Anonymous(_) => "Anonymous",
            Action::Names(_) => "Names",
            Action::Groups(_) => "Groups",
        })
    }
}

/// What a stay's connection tells after a round.
pub struct Status {
    pub(crate) errands: usize,
    pub(crate) is_busy: bool,
    pub(crate) is_caught_up: bool,
    // Time until something of its own is due, ms.
    pub(crate) due_in: u64,
}

/// What a stay knows.
#[derive(Default)]
pub struct Connection {
    next_id: u64,
    pending: BTreeMap<u64, (Pending, u64, bool)>,
    errands: VecDeque<Errand>,
    log_lines: Vec<String>,
    keepalive_at: u64,
    is_caught_up: bool,
    catch_up_limit: u64,
    has_sent_capabilities: bool,
    checked_at: Option<u64>,
    has_check_failed: bool,
    has_asked_names: bool,
    issuer: Issuer,
    credential: Option<AuthCredential>,
    credential_day: Option<u64>,
    fetched_revisions: BTreeMap<Peer, u32>,
    refreshed_groups: BTreeSet<Peer>,
    is_ready: bool,
    owed_resends: Vec<ResendRequest>,
    resends: Vec<(Address, DecryptionError, Option<[u8; 32]>)>,
    sync_requests: usize,
    sync_requests_sent: usize,
    is_certificate_asked: [bool; 2],
    is_changed: bool,
    // What the stay has for the call layer that no frame brought: a signal not sent, relays held, a token not had.
    events: Vec<Event>,
    is_relays_wanted: bool,
    // The groups whose calls' tokens are wanted, by their master keys.
    tokens: Vec<(Peer, [u8; 32])>,
}

struct ResendRequest {
    sender: Address,
    error: DecryptionError,
    group: Option<[u8; 32]>,
    due: Option<u64>,
}

impl Connection {
    /// A stay begun at `now` by the wall clock and `paced` by the paced one, with the messages that wait and what a stay before it put off.
    pub fn new(device: &mut Device, crypto: &impl Crypto, (now, paced): (u64, u64)) -> Connection {
        let deferred = core::mem::take(&mut device.deferred_orders).into_iter().filter(|(_, order)| !order.is_message);
        let mut connection = Connection {
            keepalive_at: paced + KEEPALIVE_PERIOD,
            catch_up_limit: paced + ANSWER_TIMEOUT,
            ..Connection::default()
        };
        connection.queue(device.unsent_orders(crypto, now).into_iter().chain(deferred.map(|(_, order)| order)));
        connection
    }

    // Takes orders to send, each an errand with nothing of it done.
    fn queue(&mut self, orders: impl IntoIterator<Item = Order>) {
        self.errands.extend(orders.into_iter().map(Errand::begin));
    }

    /// Begins a round: what the conversations show is changed from the start where `is_due`.
    pub const fn begin_round(&mut self, is_due: bool) {
        self.is_changed = is_due;
    }

    /// Notes that the round changed what the conversations show.
    pub const fn note_changed(&mut self) {
        self.is_changed = true;
    }

    /// True if the round changed what the conversations show.
    pub const fn is_changed(&self) -> bool {
        self.is_changed
    }

    /// Lets the messages go that have disappeared at `now`.
    pub fn expire(&mut self, device: &mut Device, now: u64) {
        self.is_changed |= !device.expire(now).is_empty();
    }

    // Asks the service at `paced`, on the account's socket or on the unauthenticated one, and waits for the answer.
    fn request(
        &mut self,
        out: &mut Vec<Action>,
        pending: Pending,
        (verb, path): (&str, &str),
        headers: &[(&str, &str)],
        body: &[u8],
        (paced, is_anonymous): (u64, bool),
    ) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, (pending, paced, is_anonymous));
        let frame = service::write_request(id, verb, path, headers, body);
        out.push(if is_anonymous { Action::Anonymous(frame) } else { Action::Frame(frame) });
    }

    fn errand_request(
        &mut self,
        out: &mut Vec<Action>,
        (verb, path): (&str, &str),
        (body, kind): (&[u8], (&str, &str)),
        access: Option<&Access>,
        paced: u64,
    ) {
        let header = access.map(Access::header);
        let mut headers: Vec<(&str, &str)> = header.iter().map(|(name, value)| (*name, value.as_str())).collect();
        if !body.is_empty() {
            headers.push(kind);
        }
        self.request(out, Pending::Errand, (verb, path), &headers, body, (paced, access.is_some()));
    }

    /// True while a request on the unauthenticated socket waits for its answer.
    pub fn is_anonymous_pending(&self) -> bool {
        self.pending.values().any(|(_, _, is_anonymous)| *is_anonymous)
    }

    /// Has what waited on the unauthenticated socket tried again later.
    pub fn note_anonymous_lost(&mut self, device: &mut Device) {
        let lost: Vec<u64> =
            self.pending.iter().filter(|(_, (_, _, is_anonymous))| *is_anonymous).map(|(id, _)| *id).collect();
        for id in lost {
            if let Some((Pending::Errand, ..)) = self.pending.remove(&id)
                && let Some(errand) = self.errands.front_mut()
            {
                errand.note_lost(device);
            }
        }
    }

    /// Takes what the screens and the call layer asked; a message goes into its conversation at once, and the voice notes just said come back to be kept (`Device::keep_send`).
    pub fn take_commands(
        &mut self,
        device: &mut Device,
        crypto: &impl Crypto,
        commands: Vec<Command>,
        now: u64,
    ) -> Vec<(String, Vec<u8>)> {
        self.is_changed |= !commands.is_empty();
        let mut sounds = Vec::new();
        for command in commands {
            match command {
                Command::Send(send) => {
                    let (order, said) = device.keep_send(crypto, send, now);
                    self.queue(order);
                    sounds.extend(said);
                }
                Command::Read(peer) => self.queue(device.mark_read(crypto, peer, now)),
                Command::Auto(_) => {}
                Command::Signal(outbound) => self.queue(device.signal_orders(outbound, now)),
                Command::Course(note, course) => self.queue(device.take_course(crypto, (&note, course), now)),
                Command::Seen => self.queue(device.see_calls(crypto, now)),
                Command::Relays => match device.cached_relays(now) {
                    Some(relays) => self.events.push(Event::Relays(relays)),
                    None => self.is_relays_wanted = true,
                },
                Command::Token(peer) => match device.master_key(&peer) {
                    Some(master_key) => self.tokens.push((peer, master_key)),
                    None => self.events.push(Event::Token(peer, None)),
                },
            }
        }
        sounds
    }

    /// What the stay has for the call layer that no frame brought, since this was last asked, which changes what the screens are told.
    pub fn take_events(&mut self) -> Vec<Event> {
        self.is_changed |= !self.events.is_empty();
        core::mem::take(&mut self.events)
    }

    /// Ends the stay: what it held for the call layer, every call's signal it did not send among it, every token it did not have as none had, and relays asked as none.
    pub fn end(&mut self) -> Vec<Event> {
        let is_relays_asked = self.is_relays_wanted || self.is_asked(Pending::Relays);
        let relays = is_relays_asked.then(|| Event::Relays(Vec::new()));
        let errands = core::mem::take(&mut self.errands);
        let unsent =
            errands.into_iter().filter_map(|errand| errand.order.call).map(|call| Event::Unsent(Box::new(call)));
        let tokens = self.tokens.drain(..).map(|(peer, _)| Event::Token(peer, None));
        let mut events = core::mem::take(&mut self.events);
        events.extend(unsent.chain(tokens).chain(relays));
        events
    }

    // Puts an order off until `at` by the paced clock; a call's signal, which is worth nothing late, goes no more, and the call layer is told.
    fn put_off(&mut self, device: &mut Device, order: Order, at: u64) {
        match order.call {
            Some(outbound) => self.events.push(Event::Unsent(Box::new(outbound))),
            None => device.deferred_orders.push((at, order)),
        }
    }

    fn is_asked(&self, asked: Pending) -> bool {
        self.pending.values().any(|(pending, ..)| *pending == asked)
    }

    /// Takes what the transfers ended in: a message whose attachments all went up goes.
    pub fn take_transfers(&mut self, device: &mut Device, crypto: &impl Crypto, done: Vec<(Job, Finish)>, now: u64) {
        let (orders, is_changed) = device.take_finished(crypto, done, now);
        self.queue(orders);
        self.is_changed |= is_changed;
    }

    /// Takes a frame of the unauthenticated socket: an answer as the account's socket's is taken; a request, which the service makes none of there, comes to the log's line of it.
    pub fn take_anonymous_frame(
        &mut self,
        device: &mut Device,
        crypto: &impl Crypto,
        frame: Frame,
        times: (u64, u64),
    ) -> Result<Vec<Action>, &'static str> {
        match frame {
            Frame::Response { .. } => Ok(self.take_frame(device, crypto, frame, times).0),
            Frame::Request { .. } => Err(service::ANONYMOUS_REQUEST_LINE),
        }
    }

    /// Takes a frame of the service's at `now` by the wall clock and `paced` by the paced one; what it did, for the screens.
    pub fn take_frame(
        &mut self,
        device: &mut Device,
        crypto: &impl Crypto,
        frame: Frame,
        (now, paced): (u64, u64),
    ) -> (Vec<Action>, Vec<Event>, Option<DropReason>) {
        let mut out = Vec::new();
        let mut events = Vec::new();
        let mut drop_reason = None;
        match frame {
            Frame::Request { id, verb, path, body, delivered } => {
                if verb == PUT && path == service::MESSAGE {
                    let delivery = device.deliver(crypto, &body, (now, delivered));
                    self.queue(device.take_hangups(now));
                    if let Some((sender, timestamp)) = delivery.message_id {
                        self.owed_resends.retain(|owed| (owed.sender, owed.error.timestamp) != (sender, timestamp));
                    }
                    match delivery.result {
                        Ok(effects) => {
                            events = effects.events;
                            self.resends.extend(effects.resend);
                        }
                        Err(reason) => {
                            if let DropReason::Unopened { sender, timestamp, ratchet, group, .. } = reason
                                && !self
                                    .owed_resends
                                    .iter()
                                    .any(|owed| (owed.sender, owed.error.timestamp) == (sender, timestamp))
                            {
                                let due = self.is_caught_up.then(|| paced + resend_delay(crypto));
                                let error = DecryptionError { ratchet, timestamp, device: sender.device };
                                self.owed_resends.push(ResendRequest { sender, error, group, due });
                            }
                            drop_reason = Some(reason);
                        }
                    }
                } else if verb == PUT && path == service::QUEUE_EMPTY {
                    self.is_caught_up = true;
                }
                out.push(Action::Frame(service::write_ok(id)));
            }
            Frame::Response { id, status, body, .. } => match self.pending.remove(&id) {
                Some((Pending::Errand, ..)) => {
                    if let Some(errand) = self.errands.front_mut() {
                        errand.take_answer(device, crypto, status, &body, now);
                    }
                }
                Some((Pending::Counts(identity), ..)) => {
                    let counts = core::str::from_utf8(&body).ok().and_then(service::parse_counts);
                    match (status, counts) {
                        (OK, Some(counts)) => {
                            if let Some((upload, ids)) = device.renew_keys(crypto, identity, counts, now) {
                                let path = service::keys_path(identity.name());
                                self.request(
                                    &mut out,
                                    Pending::Upload(identity, ids),
                                    (PUT, &path),
                                    &[service::JSON],
                                    upload.as_bytes(),
                                    (paced, false),
                                );
                            }
                        }
                        _ => self.has_check_failed = true,
                    }
                }
                Some((Pending::Upload(identity, ids), ..)) => {
                    if SUCCESSFUL.contains(&status) {
                        device.note_uploaded(identity, ids, now);
                    } else {
                        self.has_check_failed = true;
                    }
                }
                Some((Pending::Certificate(has_number), ..)) => {
                    let document = core::str::from_utf8(&body).unwrap_or_default();
                    if status != OK || !device.take_certificate(has_number, document, now) {
                        self.log_lines.push(format!("no sender certificate: the service answered {status}"));
                    }
                }
                Some((Pending::StorageAuth, ..)) => {
                    if let (OK, Some((username, password))) =
                        (status, core::str::from_utf8(&body).ok().and_then(super::storage::parse_credentials))
                    {
                        out.push(Action::Names(service::basic_auth(&username, &password)));
                    }
                }
                Some((Pending::GroupAuth(day), ..)) => {
                    let account = &device.account;
                    let id = (account.aci, account.credential_salt.as_deref());
                    let document = core::str::from_utf8(&body).ok().filter(|_| status == OK);
                    self.credential =
                        document.and_then(|document| parse_credential(crypto, document, id, day, &self.issuer));
                    match (&self.credential, status) {
                        (Some(_), _) => {}
                        (None, OK) => {
                            self.log_lines.push(String::from("a credential for the groups server that does not check"))
                        }
                        (None, status) => {
                            self.log_lines
                                .push(format!("no credential for the groups server: the service answered {status}"));
                        }
                    }
                }
                Some((Pending::Relays, ..)) => {
                    let document = core::str::from_utf8(&body).ok().filter(|_| status == OK);
                    let relays = document.map(service::parse_relays).unwrap_or_default();
                    if relays.is_empty() {
                        self.log_lines.push(format!("no relays for a call: the service answered {status}"));
                    }
                    device.keep_relays(&relays, now);
                    events.push(Event::Relays(relays));
                }
                Some((Pending::Keepalive | Pending::Capabilities, ..)) | None => {}
            },
        }
        self.is_changed |= !events.is_empty();
        (out, events, drop_reason)
    }

    /// What is due at `now` by the wall clock and `paced` by the paced one; none where the socket has stopped answering, which ends the stay.
    pub fn tick(
        &mut self,
        device: &mut Device,
        crypto: &impl Crypto,
        (now, paced): (u64, u64),
        is_asleep: bool,
    ) -> Option<Vec<Action>> {
        let mut out = Vec::new();
        if self.pending.values().any(|(pending, at, _)| match pending {
            Pending::Keepalive => at + KEEPALIVE_TIMEOUT <= paced,
            _ => at + ANSWER_TIMEOUT <= paced,
        }) {
            return None;
        }
        self.is_caught_up |= is_asleep && paced >= self.catch_up_limit;
        if self.is_caught_up {
            self.after_catch_up(device, crypto, now, paced);
        }
        if !self.has_sent_capabilities {
            self.has_sent_capabilities = true;
            let capabilities = service::write_capabilities(device.account.pni.is_some());
            self.request(
                &mut out,
                Pending::Capabilities,
                (PUT, service::CAPABILITIES),
                &[service::JSON],
                capabilities.as_bytes(),
                (paced, false),
            );
        }
        if paced >= self.keepalive_at {
            self.keepalive_at = paced + KEEPALIVE_PERIOD;
            self.request(&mut out, Pending::Keepalive, (GET, service::KEEPALIVE), &[], &[], (paced, false));
        }
        if core::mem::take(&mut self.is_relays_wanted) && !self.is_asked(Pending::Relays) {
            self.request(&mut out, Pending::Relays, (GET, service::RELAYS), &[], &[], (paced, false));
        }
        // The keys are checked once the catch-up is done, or at once where they are too old to send, and again five minutes after a check that failed.
        let is_stale = device.keys_out_of_date(now);
        if (self.is_caught_up || is_stale)
            && !self.is_checking_keys()
            && self.checked_at.is_none_or(|at| self.has_check_failed && at + CHECK_RETRY <= paced)
        {
            let identities = device.upkeep_due(now);
            if !identities.is_empty() {
                (self.checked_at, self.has_check_failed) = (Some(paced), false);
            }
            for identity in identities {
                let path = service::keys_path(identity.name());
                self.request(&mut out, Pending::Counts(identity), (GET, &path), &[], &[], (paced, false));
            }
        }
        // While the keys are too old to send, as Signal-Desktop sends nothing then, the errands wait for their check, and once it has ended without renewing them, for the next, as errands put off wait.
        if is_stale && !self.is_checking_keys() && !self.is_asked(Pending::Errand) {
            let retry = self.checked_at.unwrap_or(paced) + CHECK_RETRY;
            for errand in core::mem::take(&mut self.errands) {
                self.put_off(device, errand.order, retry);
            }
        }
        // The storage service is read once a day, at once where no reading has listed the groups, and again whenever the phone says it holds something new (`FetchLatest`).
        let is_reading = self.is_asked(Pending::StorageAuth);
        let is_due =
            !self.has_asked_names && (device.storage_read_at + STORAGE_PERIOD <= now || !device.has_listed_groups);
        if self.is_caught_up && !is_reading && (is_due || device.is_storage_due) {
            (self.has_asked_names, device.is_storage_due) = (true, false);
            self.request(&mut out, Pending::StorageAuth, (GET, service::STORAGE_AUTH), &[], &[], (paced, false));
        }
        let (due, later): (Vec<_>, Vec<_>) = device.deferred_orders.drain(..).partition(|(at, _)| *at <= paced);
        device.deferred_orders = later;
        self.queue(due.into_iter().map(|(_, order)| order));
        let day = now / MILLIS_PER_SECOND / SECONDS_PER_DAY * SECONDS_PER_DAY;
        while !self.is_asked(Pending::Errand) && !is_stale {
            self.request_certificates(device, &mut out, (now, paced));
            if self.errands.front().is_some_and(|errand| self.errand_waits(device, errand, (day, now))) {
                break;
            }
            let Some(errand) = self.errands.front_mut() else { break };
            match errand.next(device, crypto, now) {
                Step::Keys { path, access } => {
                    self.errand_request(&mut out, (GET, &path), (&[], service::JSON), access.as_ref(), paced);
                }
                Step::Submit { path, body, access } => {
                    let body = (body.as_bytes(), service::JSON);
                    self.errand_request(&mut out, (PUT, &path), body, access.as_ref(), paced);
                }
                Step::Multi { path, body, token } => {
                    let access = Access::Token(token);
                    self.errand_request(
                        &mut out,
                        (PUT, &path),
                        (&body, service::MULTI_RECIPIENT),
                        Some(&access),
                        paced,
                    );
                }
                Step::Wait => break,
                Step::Done(Outcome::Later) => {
                    let later = self.errands.pop_front().expect("the errand at the head");
                    self.put_off(device, later.order, paced + RETRY_DELAY);
                }
                Step::Done(outcome) => {
                    let done = self.errands.pop_front().expect("the errand at the head");
                    self.log_lines.extend(done.failure_line());
                    // A call's signal counts as sent where any recipient took it, as Signal-Desktop's group send counts it.
                    match (done.order.call.clone(), done.rest()) {
                        (Some(outbound), _) if outcome != Outcome::Sent => {
                            self.events.push(Event::Unsent(Box::new(outbound)));
                        }
                        (Some(_), _) | (None, None) => {}
                        (None, Some(rest)) => device.deferred_orders.push((paced + RETRY_DELAY, rest)),
                    }
                    if done.order.is_sync_request && outcome == Outcome::Sent {
                        self.sync_requests_sent += 1;
                        if self.sync_requests_sent == self.sync_requests {
                            device.note_synced();
                        }
                    }
                    if done.order.is_message {
                        self.is_changed = true;
                        let sent = (done.order.timestamp, &done.sealed_recipients[..]);
                        let status = outcome.to_status();
                        self.queue(
                            status.and_then(|status| device.note_sent(crypto, done.order.peer, sent, status, now)),
                        );
                    }
                }
            }
        }
        self.request_groups(device, &mut out, day, paced);
        self.is_ready = !self.is_asked(Pending::Errand)
            && !is_stale
            && self.errands.front().is_some_and(|errand| !self.errand_waits(device, errand, (day, now)));
        Some(out)
    }

    fn is_checking_keys(&self) -> bool {
        self.pending.values().any(|(pending, ..)| matches!(pending, Pending::Counts(_) | Pending::Upload(..)))
    }

    fn request_certificates(&mut self, device: &Device, out: &mut Vec<Action>, (now, paced): (u64, u64)) {
        let certificates = self.errands.front().map(|errand| errand.certificates(device)).unwrap_or_default();
        for has_number in certificates {
            if device.certificate(has_number, now).is_none() && !self.is_certificate_asked[usize::from(has_number)] {
                self.is_certificate_asked[usize::from(has_number)] = true;
                let path = service::certificate_path(has_number);
                self.request(out, Pending::Certificate(has_number), (GET, path), &[], &[], (paced, false));
            }
        }
    }

    fn after_catch_up(&mut self, device: &mut Device, crypto: &impl Crypto, now: u64, paced: u64) {
        self.owed_resends.iter_mut().for_each(|owed| {
            owed.due.get_or_insert(paced);
        });
        let (due, later): (Vec<_>, Vec<_>) =
            self.owed_resends.drain(..).partition(|owed| owed.due.is_some_and(|due| due <= paced));
        self.owed_resends = later;
        for owed in due {
            self.queue(device.request_resend(owed.sender, &owed.error, owed.group, now));
        }
        for (requester, error, group) in core::mem::take(&mut self.resends) {
            self.queue(device.resend(crypto, requester, &error, group, now));
        }
        self.queue(device.take_delivery_receipts(now));
        self.queue(device.take_profile_key_responses(now));
        if !device.is_synced && self.sync_requests == 0 {
            let requests = device.sync_requests(crypto, now);
            self.sync_requests = requests.len();
            self.queue(requests);
        }
    }

    fn is_group_wanted(&self, peer: &Peer, (revision, needs_refresh): (Option<u32>, bool)) -> bool {
        revision.is_some_and(|revision| self.fetched_revisions.get(peer) != Some(&revision))
            || (needs_refresh && !self.refreshed_groups.contains(peer))
    }

    fn is_asking_credential(&self) -> bool {
        self.pending.values().any(|(pending, ..)| matches!(pending, Pending::GroupAuth(_)))
    }

    // The groups' states due, and the tokens of groups' calls wanted, fetched under the day's credential, which is asked first; a token is answered none where the credential could not be had.
    fn request_groups(&mut self, device: &mut Device, out: &mut Vec<Action>, day: u64, paced: u64) {
        if core::mem::take(&mut device.is_credential_stale) {
            (self.credential, self.credential_day) = (None, None);
        }
        let due: Vec<(Peer, [u8; 32], Option<u32>, bool)> = device
            .groups_due()
            .into_iter()
            .filter(|(peer, _, revision, needs_refresh)| self.is_group_wanted(peer, (*revision, *needs_refresh)))
            .collect();
        if due.is_empty() && self.tokens.is_empty() {
            return;
        }
        match &self.credential {
            Some(credential) if credential.redemption == day => {
                let groups = due.iter().map(|(peer, master_key, ..)| (*peer, *master_key)).collect();
                let fetch = GroupFetch::new(credential.clone(), groups).with_tokens(core::mem::take(&mut self.tokens));
                out.push(Action::Groups(Box::new(fetch)));
                for (peer, _, revision, needs_refresh) in due {
                    self.fetched_revisions.extend(revision.map(|revision| (peer, revision)));
                    if needs_refresh {
                        self.refreshed_groups.insert(peer);
                    }
                }
            }
            _ if self.credential_day != Some(day) => {
                self.credential_day = Some(day);
                let path = service::group_credentials_path(day);
                self.request(out, Pending::GroupAuth(day), (GET, &path), &[], &[], (paced, false));
            }
            _ if self.is_asking_credential() => {}
            _ => self.events.extend(self.tokens.drain(..).map(|(peer, _)| Event::Token(peer, None))),
        }
    }

    fn errand_waits(&self, device: &Device, errand: &Errand, (day, now): (u64, u64)) -> bool {
        let is_due = errand.awaited_group().is_some_and(|peer| {
            let (revision, needs_refresh) = device.group_due(&peer);
            self.is_group_wanted(&peer, (revision.filter(|_| errand.order.is_message), needs_refresh))
        });
        let is_fetching = self.credential.as_ref().is_some_and(|credential| credential.redemption == day)
            || self.credential_day != Some(day)
            || self.is_asking_credential();
        let is_certifying =
            errand.certificates(device).into_iter().any(|has_number| device.certificate(has_number, now).is_none())
                && self.pending.values().any(|(pending, ..)| matches!(pending, Pending::Certificate(_)));
        (is_due && is_fetching) || is_certifying
    }

    /// The lines the log tells of what the stay did not get done since this was last asked.
    pub fn take_log_lines(&mut self) -> Vec<String> {
        core::mem::take(&mut self.log_lines)
    }

    fn is_idle(&self) -> bool {
        self.errands.is_empty()
            && self.owed_resends.is_empty()
            && self.resends.is_empty()
            && self.pending.values().all(|(pending, ..)| *pending == Pending::Keepalive)
    }

    fn due_in(&self, device: &Device, (now, paced): (u64, u64)) -> u64 {
        if self.is_ready {
            return 0;
        }
        let deferred = device.deferred_orders.iter().map(|(at, _)| at.saturating_sub(paced));
        let resends = self.owed_resends.iter().filter_map(|owed| owed.due.map(|due| due.saturating_sub(paced)));
        let expiry = device.next_expiry(now);
        let keepalive = self.keepalive_at.saturating_sub(paced).min(KEEPALIVE_PERIOD);
        deferred.chain(resends).chain(expiry).fold(keepalive, u64::min)
    }

    /// What the stay tells the thread's state after a round (`State::take_round`).
    pub fn status(&self, device: &Device, (now, paced): (u64, u64)) -> Status {
        Status {
            errands: self.errands.len(),
            is_busy: !self.is_idle(),
            is_caught_up: self.is_caught_up,
            due_in: self.due_in(device, (now, paced)),
        }
    }
}

fn resend_delay(crypto: &impl Crypto) -> u64 {
    let choices = RESEND_SPREAD + 1;
    let mut byte = [0u8];
    // A byte below the largest multiple of the choices, so that each is as likely.
    while crypto.random(&mut byte) && byte[0] >= u8::MAX - u8::MAX % choices {}
    RESEND_UNIT * (1 + u64::from(byte[0] % choices))
}

#[cfg(test)]
mod tests {
    use super::super::address::ServiceId;
    use super::super::command::Outgoing;
    use super::super::tests::Standin;
    use super::*;
    use alloc::vec;

    fn answer(connection: &mut Connection, device: &mut Device, (id, status): (u64, u16), now: u64) {
        connection.take_frame(
            device,
            &Standin::default(),
            Frame::Response { id, status, body: Vec::new() },
            (now, now),
        );
        connection.tick(device, &Standin::default(), (now, now), false).expect("the socket answers");
    }

    fn new_unsealed(device: &mut Device, (now, paced): (u64, u64)) -> Connection {
        Connection { is_certificate_asked: [true; 2], ..Connection::new(device, &Standin::default(), (now, paced)) }
    }

    fn with_message(device: &mut Device, recipient: ServiceId) -> Connection {
        let mut connection = new_unsealed(device, (0, 0));
        let hi =
            Outgoing { peer: Peer::Contact(recipient), body: "hi".into(), is_robot: false, enclosures: Vec::new() };
        connection.take_commands(device, &Standin::default(), vec![Command::Send(hi)], 0);
        connection
    }

    #[test]
    fn round_changes() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let mut connection = new_unsealed(&mut device, (0, 0));
        assert!(!connection.is_changed());
        connection.begin_round(true);
        assert!(connection.is_changed(), "another screen shows");
        connection.begin_round(false);
        connection.take_commands(&mut device, &crypto, Vec::new(), 0);
        connection.expire(&mut device, 0);
        assert!(!connection.is_changed(), "nothing asked, nothing gone");
        connection.take_commands(&mut device, &crypto, vec![Command::Read(Peer::Group([1; 32]))], 0);
        assert!(connection.is_changed(), "what the screens asked");
        connection.begin_round(false);
        connection.note_changed();
        assert!(connection.is_changed(), "what names and groups brought");
    }

    #[test]
    fn said_note_in_a_stay() {
        use super::super::attachment::{ATTACHMENTS, Enclosure, Job};
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let mut connection = new_unsealed(&mut device, (0, 0));
        let note = Outgoing {
            peer: Peer::Contact(ServiceId::Aci([4; 16])),
            body: String::new(),
            is_robot: false,
            enclosures: vec![Enclosure::said(b"sound".to_vec())],
        };
        let sounds = connection.take_commands(&mut device, &crypto, vec![Command::Send(note)], 0);
        let name = format!("{}.m4a", crate::base::code::hex(&crypto.sha256(&[b"sound"])));
        assert_eq!(sounds, [(name.clone(), b"sound".to_vec())], "the sound to be kept, as before a stay");
        let uploads = device.uploads_due();
        assert_eq!(
            uploads.iter().map(|upload| String::from(Job::Up(upload.clone()).writes())).collect::<Vec<_>>(),
            [format!("{ATTACHMENTS}/{name}")],
            "it goes up from the file it is kept in"
        );
    }

    #[test]
    fn deferred_errands_carry_over() {
        let (mut device, recipient) = (Device::unkeyed(), ServiceId::Aci([4; 16]));
        let mut connection = with_message(&mut device, recipient);
        connection.queue([Order::new(recipient, vec![1], 0, false)]);
        connection.tick(&mut device, &Standin::default(), (0, 0), false).expect("the capabilities, then the pre-keys");
        assert_eq!((connection.errands.len(), connection.is_idle()), (2, false));
        answer(&mut connection, &mut device, (0, 200), 0);
        answer(&mut connection, &mut device, (1, 429), 0);
        answer(&mut connection, &mut device, (2, 429), 0);
        assert_eq!((connection.errands.len(), connection.is_idle()), (0, true), "the stay may end with both put off");
        let next = Connection::new(&mut device, &Standin::default(), (1, 1));
        assert_eq!(
            next.errands.iter().map(|errand| errand.order.is_message).collect::<Vec<_>>(),
            [true, false],
            "the next stay sends both at once"
        );
    }

    #[test]
    fn deferred_retry_and_catch_up_limit() {
        let mut device = Device::unkeyed();
        let mut connection = with_message(&mut device, ServiceId::Aci([4; 16]));
        connection.tick(&mut device, &Standin::default(), (0, 0), false).expect("the capabilities, then the pre-keys");
        answer(&mut connection, &mut device, (0, 200), 0);
        answer(&mut connection, &mut device, (1, 429), 0);
        connection
            .tick(&mut device, &Standin::default(), (KEEPALIVE_PERIOD, KEEPALIVE_PERIOD), false)
            .expect("the keepalive");
        answer(&mut connection, &mut device, (2, 200), ANSWER_TIMEOUT);
        assert!(
            !connection.is_caught_up && connection.errands.len() == 1,
            "awake the service may still be sending what waited a minute after the stay began, and the errand put off is tried again"
        );
        connection.tick(&mut device, &Standin::default(), (ANSWER_TIMEOUT, ANSWER_TIMEOUT), true).expect("asleep");
        assert!(connection.is_caught_up, "asleep the catch-up then counts as done");
    }

    #[test]
    fn stale_keys_hold_errands() {
        use super::super::keys::MAX_CONFIRMED_AGE;
        let crypto = Standin::default();
        let mut device = Device::keyed(&crypto, 0);
        let now = MAX_CONFIRMED_AGE;
        let mut connection = with_message(&mut device, ServiceId::Aci([4; 16]));
        let paths = |out: Vec<Action>| -> Vec<String> {
            out.into_iter()
                .filter_map(|out| match out {
                    Action::Frame(frame) => match Frame::read(&frame) {
                        Some(Frame::Request { path, .. }) => Some(path),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        };
        let out = connection.tick(&mut device, &crypto, (now, 0), false).expect("the socket answers");
        assert_eq!(
            paths(out),
            [service::CAPABILITIES, &service::keys_path("aci")],
            "the keys checked before the catch-up is done, nothing sent"
        );
        assert_eq!(connection.status(&device, (now, 0)).due_in, KEEPALIVE_PERIOD, "and the check's answer waited for");
        connection.take_frame(&mut device, &crypto, Frame::Response { id: 0, status: 200, body: Vec::new() }, (now, 1));
        let refusal = Frame::Response { id: 1, status: 500, body: Vec::new() };
        connection.take_frame(&mut device, &crypto, refusal, (now, 1));
        connection.tick(&mut device, &crypto, (now, 1), false).expect("the socket answers");
        assert_eq!(
            (connection.errands.len(), device.deferred_orders.iter().map(|(at, _)| *at).collect::<Vec<_>>()),
            (0, vec![CHECK_RETRY]),
            "the check failed: the message waits for the next, five minutes on"
        );
        let out = connection.tick(&mut device, &crypto, (now, CHECK_RETRY), false).expect("the socket answers");
        assert!(paths(out).contains(&service::keys_path("aci")), "checked again");
        let counts = Frame::Response { id: 3, status: 200, body: br#"{"count":50,"pqCount":50}"#.to_vec() };
        let (upload, _, _) = connection.take_frame(&mut device, &crypto, counts, (now, CHECK_RETRY));
        assert_eq!(paths(upload), [service::keys_path("aci")], "the old keys renewed");
        let out = connection.tick(&mut device, &crypto, (now, CHECK_RETRY), false).expect("the socket answers");
        assert!(paths(out).is_empty() && connection.errands.len() == 1, "the message waits for the upload");
        assert!(connection.status(&device, (now, CHECK_RETRY)).due_in > 0, "which the stay waits for, not spinning");
        connection.take_frame(
            &mut device,
            &crypto,
            Frame::Response { id: 4, status: 200, body: Vec::new() },
            (now, CHECK_RETRY),
        );
        let out = connection.tick(&mut device, &crypto, (now, CHECK_RETRY), false).expect("the socket answers");
        assert_eq!(paths(out), [service::bundles_path(&ServiceId::Aci([4; 16]))], "taken, the message goes");
    }

    fn requests(out: &[Action]) -> Vec<(u64, String)> {
        out.iter()
            .filter_map(|out| match out {
                Action::Frame(frame) => match Frame::read(frame) {
                    Some(Frame::Request { id, path, .. }) => Some((id, path)),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn catch_up_errands() {
        use super::super::device::tests::session_with;
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let (own, alice) = (device.account.aci(), ServiceId::Aci([4; 16]));
        session_with(&mut device, &crypto, Address { service: own, device: 1 }, 7);
        session_with(&mut device, &crypto, Address { service: alice, device: 1 }, 8);
        device.owed_receipts.push((alice, Peer::Contact(alice), 7));
        let mut connection = new_unsealed(&mut device, (0, 0));
        connection.tick(&mut device, &crypto, (0, 0), false).expect("the capabilities");
        assert_eq!(connection.errands.len(), 0, "nothing goes before the catch-up is done");
        let empty = Frame::Request {
            id: 9,
            verb: "PUT".into(),
            path: service::QUEUE_EMPTY.into(),
            body: Vec::new(),
            delivered: None,
        };
        connection.take_frame(&mut device, &crypto, empty, (0, 0));
        let mut sent = Vec::new();
        let mut out = connection.tick(&mut device, &crypto, (0, 0), false).expect("the errands");
        assert_eq!(connection.errands.len(), 3, "the receipt and the two requests");
        while let Some((id, path)) = requests(&out).into_iter().find(|(_, path)| path.starts_with("/v1/messages/")) {
            sent.push(path);
            connection.take_frame(&mut device, &crypto, Frame::Response { id, status: 200, body: Vec::new() }, (0, 0));
            out = connection.tick(&mut device, &crypto, (0, 0), false).expect("the next");
        }
        assert_eq!(sent, [service::messages_path(&alice), service::messages_path(&own), service::messages_path(&own)]);
        assert!(
            device.is_synced && device.take_dirty().contains(&super::super::device::File::Seen),
            "asked, which is kept"
        );
        let mut next = Connection { is_caught_up: true, ..Connection::new(&mut device, &crypto, (0, 0)) };
        next.tick(&mut device, &crypto, (0, 0), false).expect("the socket answers");
        assert_eq!(next.errands.len(), 0, "never asked again");
    }

    #[test]
    fn sealed_errand_certificate() {
        use super::super::device::tests::certificate_answer;
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let recipient = ServiceId::Aci([4; 16]);
        let mut connection = Connection::new(&mut device, &crypto, (0, 0));
        let hi =
            Outgoing { peer: Peer::Contact(recipient), body: "hi".into(), is_robot: false, enclosures: Vec::new() };
        connection.take_commands(&mut device, &crypto, vec![Command::Send(hi)], 0);
        let out = connection.tick(&mut device, &crypto, (0, 0), false).expect("the socket answers");
        assert_eq!(
            requests(&out),
            [(0, String::from(service::CAPABILITIES)), (1, String::from(service::certificate_path(false)))],
            "the certificate without the number asked, the message waiting for it"
        );
        let certificate = certificate_answer(10 * 60 * 60 * 1000, false).into_bytes();
        connection.take_frame(&mut device, &crypto, Frame::Response { id: 1, status: 200, body: certificate }, (0, 0));
        let out = connection.tick(&mut device, &crypto, (0, 0), false).expect("the socket answers");
        let Some(Action::Anonymous(frame)) = out.last() else { panic!("a request of the unauthenticated socket") };
        let Some(Frame::Request { path, .. }) = Frame::read(frame) else { panic!("a request") };
        assert!(
            path == service::bundles_path(&recipient) && connection.is_anonymous_pending(),
            "the pre-keys asked under the access key"
        );
        let headers = crate::base::proto::Message::parse(frame).unwrap().message(2).unwrap();
        let header: Vec<&str> = headers.repeated(5).map(|line| core::str::from_utf8(line).unwrap()).collect();
        assert_eq!(header, ["unidentified-access-key: AAAAAAAAAAAAAAAAAAAAAA=="], "noughts: no profile key held");
        let request = Frame::Request {
            id: 7,
            verb: "PUT".into(),
            path: service::MESSAGE.into(),
            body: Vec::new(),
            delivered: None,
        };
        assert_eq!(
            connection.take_anonymous_frame(&mut device, &crypto, request, (0, 0)).err(),
            Some("signal: a request on the unauthenticated socket"),
            "a request there is told and not taken"
        );
        let stray = Frame::Response { id: 99, status: 200, body: Vec::new() };
        assert!(
            connection
                .take_anonymous_frame(&mut device, &crypto, stray, (0, 0))
                .is_ok_and(|actions| actions.is_empty())
                && connection.is_anonymous_pending(),
            "an answer is taken as the account's socket's, one to no request changing nothing"
        );
        connection.note_anonymous_lost(&mut device);
        connection.tick(&mut device, &crypto, (0, 0), false).expect("the socket answers");
        assert_eq!(
            (connection.is_anonymous_pending(), connection.errands.len(), device.deferred_orders.len()),
            (false, 0, 1),
            "the socket lost, the message is tried again later"
        );
    }

    #[test]
    fn group_message_token() {
        use super::super::conversation::Status;
        use super::super::device::tests::{certificate_answer, member};
        use super::super::group::tests::endorsement_server;
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        device.account.is_number_shared = true;
        let (server, now) = endorsement_server(&crypto);
        let hour = 60 * 60 * 1000;
        let (own, anna, boris) = (device.account.aci(), ServiceId::Aci([1; 16]), ServiceId::Aci([2; 16]));
        member(&mut device, &crypto, anna, &[(1, 7)], now);
        member(&mut device, &crypto, boris, &[(1, 8)], now);
        let group = device.add_endorsed_group(&crypto, &[9; 32], &[own, anna, boris], (&server, now));
        device.take_certificate(true, &certificate_answer(now + 10 * hour, true), now);
        let mut connection = Connection { issuer: server.issuer, ..Connection::new(&mut device, &crypto, (now, 0)) };
        connection.take_commands(
            &mut device,
            &crypto,
            vec![Command::Send(Outgoing { peer: group, body: "hi".into(), is_robot: false, enclosures: Vec::new() })],
            now,
        );
        let out = connection.tick(&mut device, &crypto, (now, 0), false).expect("the socket answers");
        assert_eq!(
            requests(&out),
            [(0, String::from(service::CAPABILITIES)), (1, String::from(service::certificate_path(false)))],
            "the number's certificate held, the one without it asked, which the sender key's message goes under"
        );
        let certificate = certificate_answer(now + 10 * hour, false).into_bytes();
        connection.take_frame(
            &mut device,
            &crypto,
            Frame::Response { id: 1, status: 200, body: certificate },
            (now, 0),
        );
        let mut paths = Vec::new();
        let headers = loop {
            let out = connection.tick(&mut device, &crypto, (now, 0), false).expect("the socket answers");
            let Some(Action::Anonymous(frame)) = out.last() else { panic!("a request of the unauthenticated socket") };
            let Some(Frame::Request { id, path, .. }) = Frame::read(frame) else { panic!("a request") };
            if path.starts_with("/v1/messages/multi_recipient") {
                let request = crate::base::proto::Message::parse(frame).unwrap().message(2).unwrap();
                let headers: Vec<String> =
                    request.repeated(5).map(|line| String::from(core::str::from_utf8(line).unwrap())).collect();
                connection.take_frame(
                    &mut device,
                    &crypto,
                    Frame::Response { id, status: 200, body: b"{}".to_vec() },
                    (now, 0),
                );
                break headers;
            }
            paths.push(path);
            connection.take_frame(
                &mut device,
                &crypto,
                Frame::Response { id, status: 200, body: Vec::new() },
                (now, 0),
            );
        };
        assert_eq!(
            paths,
            [service::messages_path(&anna), service::messages_path(&boris)],
            "the sender key distributed, sealed"
        );
        let token = device.group_token(&crypto, &group, &[anna, boris], now).unwrap();
        assert_eq!(
            headers,
            [
                format!("group-send-token: {}", crate::base::code::base64(&token)),
                String::from("content-type: application/vnd.signal-messenger.mrm")
            ],
            "under the group's token, as libsignal sends a group's message"
        );
        connection.begin_round(false);
        connection.tick(&mut device, &crypto, (now, 0), false).expect("the socket answers");
        assert!(connection.is_changed() && device.entries(&group)[0].status == Status::Sent, "sent, which shows");
    }

    #[test]
    fn call_signals_now_or_never() {
        use super::super::call::Signal;
        use super::super::command::{Outbound, Target};
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let busy = |call| Outbound {
            target: Target::Contact(ServiceId::Aci([4; 16])),
            device: None,
            signal: Signal::Busy { call },
        };
        let unsent = |call| Event::Unsent(Box::new(busy(call)));
        let mut connection = new_unsealed(&mut device, (0, 0));
        connection.take_commands(&mut device, &crypto, vec![Command::Signal(busy(1))], 0);
        connection.tick(&mut device, &crypto, (0, 0), false).expect("the capabilities, then the pre-keys");
        answer(&mut connection, &mut device, (0, 200), 0);
        answer(&mut connection, &mut device, (1, 429), 0);
        assert_eq!(
            (connection.take_events(), device.deferred_orders.len()),
            (vec![unsent(1)], 0),
            "a wait asked: put off to no later stay, and the call layer told"
        );
        assert!(connection.is_changed(), "which is news");
        connection.take_commands(&mut device, &crypto, vec![Command::Signal(busy(2))], 0);
        connection.tick(&mut device, &crypto, (0, 0), false).expect("the pre-keys");
        answer(&mut connection, &mut device, (2, 404), 0);
        assert_eq!(connection.take_events(), [unsent(2)], "not on Signal: not sent");
        assert!(
            connection.take_log_lines().iter().any(|line| line.starts_with("a call's signal not sent, Unregistered")),
            "told in the log by its kind"
        );
        connection.take_commands(&mut device, &crypto, vec![Command::Signal(busy(3))], 0);
        assert_eq!(connection.end(), [unsent(3)], "one the stay ends before");
    }

    #[test]
    fn relays_asked_once() {
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let mut connection = new_unsealed(&mut device, (1_000, 0));
        connection.take_commands(&mut device, &crypto, vec![Command::Relays], 1_000);
        let out = connection.tick(&mut device, &crypto, (1_000, 0), false).expect("the socket answers");
        assert_eq!(
            requests(&out),
            [(0, String::from(service::CAPABILITIES)), (1, String::from(service::RELAYS))],
            "asked on the account's socket"
        );
        let body = br#"{"relays":[{"username":"u","password":"p","urls":["turn:a"],"ttl":60}]}"#.to_vec();
        let (_, events, _) =
            connection.take_frame(&mut device, &crypto, Frame::Response { id: 1, status: 200, body }, (1_000, 0));
        assert!(matches!(&events[..], [Event::Relays(relays)] if relays.len() == 1), "handed on");
        connection.take_commands(&mut device, &crypto, vec![Command::Relays], 60_999);
        let out = connection.tick(&mut device, &crypto, (60_999, 1), false).expect("the socket answers");
        assert!(
            requests(&out).is_empty() && matches!(connection.take_events()[..], [Event::Relays(_)]),
            "a second call within their time asks nothing (`#getIceServers`)"
        );
        connection.take_commands(&mut device, &crypto, vec![Command::Relays], 61_000);
        let out = connection.tick(&mut device, &crypto, (61_000, 2), false).expect("the socket answers");
        let [(id, _)] = requests(&out)[..] else { panic!("asked anew once their time is over") };
        let refusal = Frame::Response { id, status: 500, body: Vec::new() };
        let (_, events, _) = connection.take_frame(&mut device, &crypto, refusal, (61_000, 2));
        assert_eq!(events, [Event::Relays(Vec::new())], "none had, which is told");
        assert_eq!(
            connection.take_log_lines(),
            ["no relays for a call: the service answered 500"],
            "the log tells the service's answer"
        );
        connection.take_commands(&mut device, &crypto, vec![Command::Relays], 61_000);
        assert_eq!(connection.end(), [Event::Relays(Vec::new())], "asked as the stay ends: none");
    }

    #[test]
    fn call_tokens_fetched() {
        use super::super::zk::credential::tests::{Server, answer};
        let (crypto, mut device) = (Standin::default(), Device::unkeyed());
        let own = device.account.aci();
        let day = 20_000 * SECONDS_PER_DAY;
        let now = (day + 3_600) * MILLIS_PER_SECOND;
        let group = device.add_group(&crypto, &[9; 32], &[own]);
        let server = Server::new(&crypto, &[1; 32]);
        let mut connection = Connection { issuer: server.issuer, ..new_unsealed(&mut device, (now, 0)) };
        let unknown = Peer::Group([1; 32]);
        connection.take_commands(&mut device, &crypto, vec![Command::Token(unknown), Command::Token(group)], now);
        assert_eq!(connection.take_events(), [Event::Token(unknown, None)], "of a group not known: none");
        let out = connection.tick(&mut device, &crypto, (now, 0), false).expect("the socket answers");
        assert!(requests(&out).contains(&(1, service::group_credentials_path(day))), "the day's credential first");
        let bytes = server.issue(&crypto, (&own, &ServiceId::Pni([0xB2; 16])), day, &[2; 32]);
        let document = answer(&bytes, day, Some([0xB2; 16]));
        connection.take_frame(
            &mut device,
            &crypto,
            Frame::Response { id: 1, status: 200, body: document.into_bytes() },
            (now, 0),
        );
        let out = connection.tick(&mut device, &crypto, (now, 0), false).expect("the socket answers");
        let fetches: Vec<String> = out
            .iter()
            .filter_map(|out| match out {
                Action::Groups(fetch) => Some(format!("{fetch:?}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            fetches,
            ["Fetching { groups: 0, tokens: 1, asked: 0, .. }"],
            "the token asked as a group's state is"
        );
        let mut refused = Connection { credential_day: Some(day), ..new_unsealed(&mut device, (now, 0)) };
        refused.take_commands(&mut device, &crypto, vec![Command::Token(group)], now);
        refused.tick(&mut device, &crypto, (now, 0), false).expect("the socket answers");
        assert_eq!(refused.take_events(), [Event::Token(group, None)], "the day's credential not had: none");
    }

    #[test]
    fn storage_fetch_request() {
        let mut device = Device::unkeyed();
        (device.storage_read_at, device.has_listed_groups) = (1_000, true);
        let mut connection =
            Connection { is_caught_up: true, ..Connection::new(&mut device, &Standin::default(), (2_000, 0)) };
        let asks = |connection: &mut Connection, device: &mut Device| {
            let out = connection.tick(device, &Standin::default(), (2_000, 0), false).expect("the socket answers");
            requests(&out).iter().any(|(_, path)| path == service::STORAGE_AUTH)
        };
        assert!(!asks(&mut connection, &mut device), "read a moment ago");
        device.is_storage_due = true;
        assert!(asks(&mut connection, &mut device), "the phone said so");
        assert!(!device.is_storage_due && !asks(&mut connection, &mut device), "once");
    }

    #[test]
    fn wall_clock_steps() {
        let (mut device, wall) = (Device::unkeyed(), 1_800_000_000_000);
        let mut connection = Connection::new(&mut device, &Standin::default(), (wall, 0));
        let mut tick = |now| connection.tick(&mut device, &Standin::default(), now, false);
        let first = tick((wall, 0)).expect("the capabilities asked");
        let forward = tick((wall + 2 * ANSWER_TIMEOUT, 1)).expect("set forward, the clock ends no stay");
        let back = tick((wall - STORAGE_PERIOD, KEEPALIVE_PERIOD)).expect("set back, it holds no keepalive off");
        assert_eq!((first.len(), forward.len(), back.len()), (1, 0, 1), "the keepalive goes at its time");
        assert!(tick((wall, ANSWER_TIMEOUT)).is_none(), "a request unanswered a minute by the paced clock ends it");
    }

    #[test]
    fn storage_period() {
        let mut device = Device::unkeyed();
        device.storage_read_at = 1_000;
        let asks = |device: &mut Device| {
            let mut connection =
                Connection { is_caught_up: true, ..Connection::new(device, &Standin::default(), (2_000, 0)) };
            let out = connection.tick(device, &Standin::default(), (2_000, 0), false).expect("the socket answers");
            out.iter().any(|out| {
                matches!(out, Action::Frame(frame) if matches!(Frame::read(frame), Some(Frame::Request { ref path, .. }) if path == service::STORAGE_AUTH))
            })
        };
        assert!(asks(&mut device), "a moment after the last reading, which listed no groups");
        device.has_listed_groups = true;
        assert!(!asks(&mut device), "once a reading has listed them, a day after the last");
    }

    #[test]
    fn due_in_expiry() {
        use super::super::content::{Content, Text};
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let alice = ServiceId::Aci([4; 16]);
        let connection = new_unsealed(&mut device, (0, 0));
        assert_eq!(connection.status(&device, (25_000, 0)).due_in, KEEPALIVE_PERIOD, "the keepalive at the latest");
        let text = Text { timestamp: 1, body: "hi".into(), expire: 30, expire_version: 2, ..Text::default() };
        device.apply(&crypto, Content::Text(text), &Address { service: alice, device: 1 }, 0);
        device.mark_read(&crypto, Peer::Contact(alice), 1_000);
        assert_eq!(
            connection.status(&device, (25_000, 0)).due_in,
            6_000,
            "read at a second, it disappears thirty on, whatever the paced clock says"
        );
    }

    #[test]
    fn stale_credential() {
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let day = 20_000 * SECONDS_PER_DAY;
        let now = (day + 3_600) * MILLIS_PER_SECOND;
        device.note_group_revision(&crypto, &[9; 32], 4);
        let mut connection = Connection { credential_day: Some(day), ..new_unsealed(&mut device, (now, 0)) };
        let asks = |connection: &mut Connection, device: &mut Device| {
            let out = connection.tick(device, &crypto, (now, 0), false).expect("the socket answers");
            requests(&out).iter().any(|(_, path)| *path == service::group_credentials_path(day))
        };
        assert!(!asks(&mut connection, &mut device), "asked for once a day");
        device.is_credential_stale = true;
        assert!(
            asks(&mut connection, &mut device) && !device.is_credential_stale,
            "the one had names the number before"
        );
    }

    #[test]
    fn group_state_wait() {
        use super::super::group::State;
        use super::super::group::tests::{endorsed_answer, endorsements_of, head};
        use super::super::zk::credential::tests::{Server, answer};
        let (crypto, mut device) = (Standin::default(), Device::unkeyed());
        let (own, anna) = (device.account.aci(), ServiceId::Aci([1; 16]));
        let day = 20_000 * SECONDS_PER_DAY;
        let now = (day + 3_600) * MILLIS_PER_SECOND;
        let group = device.note_group_revision(&crypto, &[9; 32], 4);
        let server = Server::new(&crypto, &[1; 32]);
        let mut connection = Connection { issuer: server.issuer, ..new_unsealed(&mut device, (now, 0)) };
        let hi = Outgoing { peer: group, body: "hi".into(), is_robot: false, enclosures: Vec::new() };
        connection.take_commands(&mut device, &crypto, vec![Command::Send(hi)], now);
        let out = connection.tick(&mut device, &crypto, (now, 0), false).expect("the capabilities and more");
        let paths: Vec<String> = out
            .iter()
            .filter_map(|out| match out {
                Action::Frame(frame) => match Frame::read(frame) {
                    Some(Frame::Request { path, .. }) => Some(path),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(paths[1], service::group_credentials_path(day), "the day's credential asked");
        assert!(
            !connection.is_asked(Pending::Errand) && connection.errands.len() == 1,
            "the message waits for its group"
        );
        let bytes = server.issue(&crypto, (&own, &ServiceId::Pni([0xB2; 16])), day, &[2; 32]);
        let document = answer(&bytes, day, Some([0xB2; 16]));
        let response = Frame::Response { id: 1, status: 200, body: document.into_bytes() };
        connection.take_frame(&mut device, &crypto, response, (now, 0));
        let out = connection.tick(&mut device, &crypto, (now, 0), false).expect("the groups fetched");
        let Some(Action::Groups(mut fetch)) = out.into_iter().find(|out| matches!(out, Action::Groups(_))) else {
            panic!("the group's state asked")
        };
        assert_eq!(
            connection.status(&device, (now, 0)).due_in,
            0,
            "the message goes on as soon as its group is fetched"
        );
        assert!(fetch.next_request(&crypto).is_some() && fetch.next_request(&crypto).is_none(), "one group");
        let state = State { revision: 4, title: Some("Climbing".into()), members: vec![own, anna], ..State::default() };
        let endorsements = endorsements_of(&crypto, &server, &[9; 32], &[own, anna], now);
        fetch.take_answer(&crypto, &head(200, true), &endorsed_answer(&[9; 32], &state, Some(&endorsements)), now);
        device.take_group_fetch(&crypto, *fetch);
        let out = connection.tick(&mut device, &crypto, (now, 0), false).expect("the message on its way");
        let Some(Action::Anonymous(frame)) = out.last() else { panic!("a request of the unauthenticated socket") };
        assert!(
            matches!(Frame::read(frame), Some(Frame::Request { ref path, .. }) if *path == service::bundles_path(&anna)),
            "Anna's pre-keys asked under her access key: the message goes to the member the server told"
        );
        let again = connection.tick(&mut device, &crypto, (now, 0), false).expect("nothing more");
        assert!(!again.iter().any(|out| matches!(out, Action::Groups(_))), "the group is not fetched twice a revision");
    }
}
