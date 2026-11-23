// Sending, step by step, as Signal-Desktop does it (`sendToGroupViaSenderKey`, `doSendMessage`).

use super::Crypto;
use super::address::{Address, Peer, ServiceId};
use super::content::{self, PLAINTEXT, UNIDENTIFIED_SENDER};
use super::conversation::Status;
use super::device::{Access, Device, Order, Trust};
use super::sealed::{self, Recipient};
use super::service;
use super::session::{Ciphertext, State};
use crate::codec::http::{
    CONFLICT, FORBIDDEN, GONE, NOT_FOUND, OK, PRECONDITION_REQUIRED, SERVER_ERRORS, TOO_MANY_REQUESTS, UNAUTHORIZED,
};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const MAX_TRIES: u32 = 3;
const MAX_ROUNDS: u32 = 10;
const MIN_SENDER_KEY_MEMBERS: usize = 2;

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Step {
    Keys { path: String, access: Option<Access> },
    Submit { path: String, body: String, access: Option<Access> },
    Multi { path: String, body: Vec<u8>, token: Vec<u8> },
    Wait,
    Done(Outcome),
}

impl core::fmt::Debug for Step {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Step::Keys { access, .. } => {
                f.debug_struct("Keys").field("sealed", &access.is_some()).finish_non_exhaustive()
            }
            Step::Submit { access, .. } => {
                f.debug_struct("Submit").field("sealed", &access.is_some()).finish_non_exhaustive()
            }
            Step::Multi { .. } => f.debug_struct("Multi").finish_non_exhaustive(),
            Step::Wait => f.write_str("Wait"),
            Step::Done(outcome) => f.debug_tuple("Done").field(outcome).finish(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Outcome {
    Sent,
    Unregistered,
    Challenged,
    Later,
    Failed,
}

impl Outcome {
    // How far a message whose sending ended so got; none for one that is tried again later.
    pub(super) const fn to_status(self) -> Option<Status> {
        match self {
            Outcome::Sent => Some(Status::Sent),
            Outcome::Later => None,
            Outcome::Unregistered | Outcome::Challenged | Outcome::Failed => Some(Status::Failed),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    Nothing,
    Keys,
    Submission,
    GroupKeys,
    Distribution,
    Multi,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Authority {
    #[default]
    Key,
    Token,
    Own,
}

// The service's answers that end an errand whatever it asked: a challenge only the phone can answer, and a wait.
fn halt_of(status: u16) -> Option<Outcome> {
    if status == PRECONDITION_REQUIRED {
        Some(Outcome::Challenged)
    } else if status == TOO_MANY_REQUESTS || SERVER_ERRORS.contains(&status) {
        Some(Outcome::Later)
    } else {
        None
    }
}

// Sets aside the sessions of the devices a mismatch names extra or stale; true where the account's pre-keys are to be asked.
fn set_aside(device: &mut Device, account: ServiceId, mismatch: &service::Mismatch) -> bool {
    for id in mismatch.extra.iter().chain(&mismatch.stale) {
        device.archive_session(Address { service: account, device: *id });
    }
    !mismatch.missing.is_empty() || !mismatch.stale.is_empty()
}

type KeyDistribution = (Box<Errand>, Vec<(ServiceId, u32, u32)>);

#[derive(Clone, PartialEq, Eq, Default)]
struct GroupSend {
    rounds: u32,
    fetched_members: Vec<ServiceId>,
    one_to_one: Vec<ServiceId>,
    fetch_queue: Vec<ServiceId>,
    authority: Authority,
    is_key_refused: bool,
    distribution: Option<KeyDistribution>,
    recipients: Vec<ServiceId>,
}

// The sending of an order, step by step; its recipients are the order's, those a group's message was sealed to at once taken out as they take it.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Errand {
    pub(super) order: Order,
    at: usize,
    is_addressed: bool,
    is_refreshed: bool,
    pending: Pending,
    needs_keys: bool,
    has_fetched_keys: bool,
    trust: Trust,
    tries: u32,
    is_failed_over: bool,
    is_asked_sealed: bool,
    pub(super) sealed_recipients: Vec<ServiceId>,
    outcome: Option<Outcome>,
    has_refused: bool,
    has_unregistered: bool,
    accepted_recipients: Vec<ServiceId>,
    is_stopped: bool,
    untrusted_recipients: Vec<ServiceId>,
    keys_status: Option<u16>,
    submit_status: Option<u16>,
    device_count: usize,
    ready_count: usize,
    group_send: Option<Box<GroupSend>>,
}

impl core::fmt::Debug for Errand {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Errand")
            .field("timestamp", &self.order.timestamp)
            .field("urgent", &self.order.is_urgent)
            .field("message", &self.order.is_message)
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

// The sending under the R1's sender key a group's content begins with, to the recipients it has.
fn group_send_of(order: &Order) -> Option<Box<GroupSend>> {
    (order.uses_sender_key() && !order.recipients.is_empty()).then(Box::default)
}

impl Errand {
    // An order with nothing of it done; one that is not addressed finds its recipients as it begins.
    pub(super) fn begin(order: Order) -> Errand {
        Errand {
            is_addressed: order.is_addressed,
            group_send: if order.is_addressed { group_send_of(&order) } else { None },
            order,
            at: 0,
            is_refreshed: false,
            pending: Pending::Nothing,
            needs_keys: false,
            has_fetched_keys: false,
            trust: Trust::Trusted,
            tries: 0,
            is_failed_over: false,
            is_asked_sealed: false,
            sealed_recipients: Vec::new(),
            outcome: None,
            has_refused: false,
            has_unregistered: false,
            accepted_recipients: Vec::new(),
            is_stopped: false,
            untrusted_recipients: Vec::new(),
            keys_status: None,
            submit_status: None,
            device_count: 0,
            ready_count: 0,
        }
    }

    pub(super) fn failure_line(&self) -> Option<String> {
        let outcome = self.outcome.filter(|outcome| *outcome != Outcome::Sent)?;
        let kind = match (self.order.is_message, self.order.call.is_some()) {
            (true, _) => "a message",
            (false, true) => "a call's signal",
            (false, false) => "a receipt or a sync",
        };
        let answer =
            |status: Option<u16>| status.map_or_else(|| String::from("not asked"), |status| format!("{status}"));
        Some(format!(
            "{kind} not sent, {outcome:?}: pre-keys {}, {} of {} devices with a session, submission {}, put right {} times",
            answer(self.keys_status),
            self.ready_count,
            self.device_count,
            answer(self.submit_status),
            self.tries
        ))
    }

    fn may_seal(&self, own: ServiceId) -> bool {
        self.order.recipients.iter().any(|recipient| *recipient != own)
            || (!self.is_addressed && self.order.peer != Peer::Contact(own))
    }

    pub(super) fn certificates(&self, device: &Device) -> Vec<bool> {
        let mut certificates = Vec::new();
        if self.may_seal(device.account.aci()) {
            certificates.push(device.account.is_number_shared);
        }
        if self.order.uses_sender_key() && !certificates.contains(&false) {
            certificates.push(false);
        }
        certificates
    }

    pub(super) fn awaited_group(&self) -> Option<Peer> {
        match self.order.peer {
            peer @ Peer::Group(_) if self.order.is_message => Some(peer),
            _ => self.order.endorsing_group.filter(|_| self.is_refreshed),
        }
    }

    fn finish_account(&mut self, device: &mut Device, outcome: Outcome) {
        match outcome {
            Outcome::Later | Outcome::Challenged => (self.outcome, self.is_stopped) = (Some(outcome), true),
            Outcome::Sent => self.accepted_recipients.push(self.order.recipients[self.at]),
            Outcome::Unregistered => self.has_unregistered = true,
            Outcome::Failed => self.has_refused = true,
        }
        self.next_account(device);
    }

    fn defer_account(&mut self, device: &mut Device) {
        let recipient = self.order.recipients[self.at];
        self.untrusted_recipients.push(recipient);
        self.next_account(device);
    }

    fn next_account(&mut self, device: &mut Device) {
        if self.is_failed_over {
            device.note_sealed_refused(self.order.recipients[self.at]);
        }
        (self.at, self.needs_keys, self.has_fetched_keys, self.trust, self.tries) =
            (self.at + 1, false, false, Trust::Trusted, 0);
        (self.is_failed_over, self.is_asked_sealed) = (false, false);
    }

    fn stop(&mut self, outcome: Outcome) {
        (self.outcome, self.is_stopped, self.group_send) = (Some(outcome), true, None);
    }

    pub(super) fn note_lost(&mut self, device: &mut Device) {
        self.pending = Pending::Nothing;
        if self.group_send.is_some() {
            self.stop(Outcome::Later);
        } else if self.at < self.order.recipients.len() {
            self.finish_account(device, Outcome::Later);
        }
    }

    const fn final_outcome(&self) -> Outcome {
        let has_accepted = !self.accepted_recipients.is_empty();
        match (has_accepted, self.untrusted_recipients.is_empty(), self.has_refused, self.has_unregistered) {
            (true, ..) => Outcome::Sent,
            (false, false, ..) => Outcome::Later,
            (false, true, false, true) => Outcome::Unregistered,
            _ => Outcome::Failed,
        }
    }

    // What goes on its own later where some took it: the same to those whose key changed a moment ago, one to one.
    pub(super) fn rest(&self) -> Option<Order> {
        if self.untrusted_recipients.is_empty() || self.accepted_recipients.is_empty() {
            return None;
        }
        let recipients = self.untrusted_recipients.clone();
        Some(Order { is_message: false, is_group_send: false, is_addressed: true, recipients, ..self.order.clone() })
    }

    pub(super) fn next(&mut self, device: &mut Device, crypto: &impl Crypto, now: u64) -> Step {
        if !self.is_addressed {
            (self.order.recipients, self.is_addressed) = (device.recipients(&self.order.peer, now), true);
            self.group_send = group_send_of(&self.order);
        }
        if let Some(step) = self.group_step(device, crypto, now) {
            return step;
        }
        loop {
            if self.outcome.is_none() && self.at >= self.order.recipients.len() {
                self.outcome = Some(self.final_outcome());
            }
            if let Some(outcome) = self.outcome {
                return Step::Done(outcome);
            }
            let recipient = self.order.recipients[self.at];
            let targets = device.sendable_devices(recipient, now);
            // libsignal seals only to a session whose identity key is trusted (`message_encrypt`); Signal-Desktop then sets every session with the account aside (`doSendMessage`), and the R1 asks its pre-keys anew once.
            let trust = targets.iter().map(|target| device.session_trust(target, now)).max().unwrap_or(Trust::Trusted);
            if trust != Trust::Trusted && !self.order.is_plain {
                device.archive_sessions(recipient);
                if self.has_fetched_keys {
                    self.trust = self.trust.max(trust);
                } else {
                    self.needs_keys = true;
                }
            }
            if self.trust != Trust::Trusted {
                match self.trust {
                    Trust::Soon => self.defer_account(device),
                    _ => self.finish_account(device, Outcome::Failed),
                }
                continue;
            }
            // An account whose sealing was refused goes sealed under a group's endorsements, which wait for the group's state where they may not be used, asked anew once.
            if let Some(group) = self.order.endorsing_group.filter(|_| !self.is_refreshed && !self.is_failed_over)
                && device.wants_endorsements(&recipient, &group, now)
            {
                self.is_refreshed = true;
                device.refresh_group(&group);
                return Step::Wait;
            }
            let targets = device.sendable_devices(recipient, now);
            // It goes sealed where Signal-Desktop seals it (`getSendOptions`), until the service refuses the account's access key.
            let seal = if self.is_failed_over {
                None
            } else {
                device.seal_of(crypto, &recipient, self.order.endorsing_group.as_ref(), now)
            };
            if self.needs_keys || (targets.is_empty() && !self.has_fetched_keys) {
                (self.needs_keys, self.has_fetched_keys, self.pending, self.is_asked_sealed) =
                    (false, true, Pending::Keys, seal.is_some());
                return Step::Keys { path: service::bundles_path(&recipient), access: seal.map(|seal| seal.access) };
            }
            if targets.is_empty() {
                // The owner's own account with no other device has nothing to go to, as Signal-Desktop sends no sync then.
                let is_own = recipient == device.account.aci();
                self.finish_account(device, if is_own { Outcome::Sent } else { Outcome::Failed });
                continue;
            }
            let source = Address { service: device.account.aci(), device: device.account.device };
            let mut ciphertexts = Vec::new();
            if self.order.is_plain {
                for target in &targets {
                    if let Some(registration) = device.registration(target) {
                        let plain = Ciphertext { kind: PLAINTEXT, bytes: self.order.content.clone() };
                        ciphertexts.push((target.device, registration, plain));
                    }
                }
            } else {
                let plain = content::pad(&self.order.content);
                for target in &targets {
                    if let Some((registration, ciphertext)) = device.encrypt_to(crypto, &plain, &source, target, now) {
                        ciphertexts.push((target.device, registration, ciphertext));
                    }
                }
            }
            // Each ciphertext sealed to the identity key held for the account, with the R1's certificate, the hint and the group (`sealedSenderEncrypt`).
            let seal = seal.zip(device.identity_key(&recipient));
            if let Some((seal, identity)) = &seal {
                let envelopes = ciphertexts.into_iter().filter_map(|(id, registration, inner)| {
                    let (kind, hint, group) = (sealed::kind_of(inner.kind), self.order.hint, self.order.group.as_ref());
                    let content = sealed::write_content(kind, &seal.certificate, &inner.bytes, hint, group);
                    let bytes = sealed::seal(crypto, &device.account.aci_identity, identity, &content)?;
                    Some((id, registration, Ciphertext { kind: UNIDENTIFIED_SENDER, bytes }))
                });
                ciphertexts = envelopes.collect();
            }
            (self.pending, self.is_asked_sealed) = (Pending::Submission, seal.is_some());
            return Step::Submit {
                path: service::messages_path(&recipient),
                body: service::write_submission(&ciphertexts, self.order.timestamp, self.order.is_urgent),
                access: seal.map(|(seal, _)| seal.access),
            };
        }
    }

    fn group_step(&mut self, device: &mut Device, crypto: &impl Crypto, now: u64) -> Option<Step> {
        let peer = self.order.peer;
        let Peer::Group(id) = peer else { return None };
        loop {
            let group_send = self.group_send.as_mut()?;
            if let Some((mut distribution, devices)) = group_send.distribution.take() {
                match distribution.next(device, crypto, now) {
                    Step::Done(_) if distribution.is_stopped => {
                        self.stop(distribution.outcome.unwrap_or(Outcome::Later));
                        return None;
                    }
                    Step::Done(_) => {
                        group_send.one_to_one.extend(
                            distribution
                                .order
                                .recipients
                                .iter()
                                .filter(|recipient| !distribution.accepted_recipients.contains(recipient)),
                        );
                        let mut key_devices =
                            device.sender_key(crypto, &peer, now).map(|key| key.devices).unwrap_or_default();
                        key_devices.extend(
                            devices
                                .into_iter()
                                .filter(|(account, ..)| distribution.accepted_recipients.contains(account)),
                        );
                        device.set_sender_devices(&peer, key_devices);
                        group_send.rounds += 1;
                        continue;
                    }
                    step => {
                        group_send.distribution = Some((distribution, devices));
                        self.pending = Pending::Distribution;
                        return Some(step);
                    }
                }
            }
            let Some(key) = device.sender_key(crypto, &peer, now).filter(|_| group_send.rounds <= MAX_ROUNDS) else {
                self.group_send = None;
                return None;
            };
            if !device.has_endorsements(&peer, now) && !self.is_refreshed && device.can_refresh(&peer) {
                (self.is_refreshed, group_send.rounds) = (true, group_send.rounds + 1);
                device.refresh_group(&peer);
                return Some(Step::Wait);
            }
            if group_send.fetch_queue.is_empty() {
                let sessionless: Vec<ServiceId> = self
                    .order
                    .recipients
                    .iter()
                    .copied()
                    .filter(|recipient| {
                        device.sendable_devices(*recipient, now).is_empty()
                            && !group_send.fetched_members.contains(recipient)
                            && !group_send.one_to_one.contains(recipient)
                    })
                    .collect();
                if sessionless.iter().any(|recipient| !device.is_off_signal(recipient, now)) {
                    group_send.fetch_queue = sessionless;
                }
            }
            if let Some(&member) = group_send.fetch_queue.first() {
                let access = loop {
                    match group_send.authority {
                        Authority::Key => match device.member_access(crypto, &member) {
                            Some(key) => break Some(Access::Key(key)),
                            None => group_send.authority = Authority::Token,
                        },
                        Authority::Token => {
                            let token = matches!(member, ServiceId::Aci(_))
                                .then(|| device.group_token(crypto, &peer, &[member], now));
                            match token.flatten() {
                                Some(token) => break Some(Access::Token(token)),
                                None => group_send.authority = Authority::Own,
                            }
                        }
                        Authority::Own => break None,
                    }
                };
                self.pending = Pending::GroupKeys;
                return Some(Step::Keys { path: service::bundles_path(&member), access });
            }
            let members: Vec<ServiceId> = self
                .order
                .recipients
                .iter()
                .copied()
                .filter(|recipient| {
                    !group_send.one_to_one.contains(recipient)
                        && device.endorses(&peer, recipient, now)
                        && !device.is_off_signal(recipient, now)
                        && !device.sendable_devices(*recipient, now).is_empty()
                        && device.identity_key(recipient).is_some()
                })
                .collect();
            if members.len() < MIN_SENDER_KEY_MEMBERS {
                self.group_send = None;
                return None;
            }
            let devices: Vec<(ServiceId, u32, u32)> = members
                .iter()
                .flat_map(|member| {
                    device.sendable_devices(*member, now).into_iter().map(move |address| (*member, address))
                })
                .filter_map(|(member, address)| Some((member, address.device, device.registration(&address)?)))
                .collect();
            let removed: Vec<(ServiceId, u32, u32)> =
                key.devices.iter().filter(|entry| !devices.contains(entry)).copied().collect();
            if removed.iter().any(|(account, ..)| !device.members(&peer).contains(account)) {
                device.reset_sender_key(&peer, now);
                group_send.rounds += 1;
                continue;
            }
            let new_devices: Vec<(ServiceId, u32, u32)> =
                devices.iter().filter(|entry| !key.devices.contains(entry)).copied().collect();
            if !new_devices.is_empty() {
                let Some(distribution) = device.own_distribution(crypto, &key.distribution) else {
                    self.group_send = None;
                    return None;
                };
                let mut accounts: Vec<ServiceId> = Vec::new();
                for (account, ..) in &new_devices {
                    if !accounts.contains(account) {
                        accounts.push(*account);
                    }
                }
                // Under a timestamp of its own, after the message's, so that the log of what was sent tells them apart.
                let timestamp = device.stamp(now.max(self.order.timestamp + 1));
                let content = content::write_distribution(&distribution);
                let sent_as = (self.order.is_urgent, self.order.hint);
                let errand = Errand::begin(Order::distribution(peer, accounts, content, timestamp, sent_as));
                group_send.distribution = Some((Box::new(errand), new_devices));
                continue;
            }
            if !removed.is_empty() {
                device.set_sender_devices(
                    &peer,
                    key.devices.iter().filter(|entry| !removed.contains(entry)).copied().collect(),
                );
            }
            if !device.owns_distribution(&key.distribution) {
                device.reset_sender_key(&peer, now);
                group_send.rounds += 1;
                continue;
            }
            let token = device.group_token(crypto, &peer, &members, now);
            let certificate = device.certificate(false, now).map(<[u8]>::to_vec);
            let (Some(token), Some(certificate)) = (token, certificate) else {
                self.group_send = None;
                return None;
            };
            let plain = content::pad(&self.order.content);
            let Ok(ciphertext) = device.group_encrypt(crypto, &key.distribution, &plain) else {
                device.reset_sender_key(&peer, now);
                group_send.rounds += 1;
                continue;
            };
            let content =
                sealed::write_content(sealed::SENDER_KEY, &certificate, &ciphertext, self.order.hint, Some(&id));
            let mut recipients: Vec<Recipient> = members
                .iter()
                .filter_map(|member| {
                    let devices = devices.iter().filter(|(account, ..)| account == member);
                    let devices = devices.map(|(_, device, registration)| (*device, *registration)).collect();
                    Some(Recipient { id: *member, identity: device.identity_key(member)?, devices })
                })
                .collect();
            // Signal-Desktop hands libsignal the devices ordered by their accounts' ids as written.
            recipients.sort_by_key(|recipient| recipient.id.to_text());
            match sealed::seal_many(crypto, &device.account.aci_identity, &recipients, &content) {
                Ok(body) => {
                    group_send.recipients = members;
                    self.pending = Pending::Multi;
                    return Some(Step::Multi {
                        path: service::multi_recipient_path(self.order.timestamp, self.order.is_urgent),
                        body,
                        token,
                    });
                }
                // A registration libsignal cannot write (`InvalidRegistrationId`): the account's sealing refused, and the account left to go one to one.
                Err(Some(account)) => {
                    device.note_sealed_refused(account);
                    group_send.one_to_one.push(account);
                    group_send.rounds += 1;
                }
                Err(None) => {
                    self.group_send = None;
                    return None;
                }
            }
        }
    }

    pub(super) fn take_answer(
        &mut self,
        device: &mut Device,
        crypto: &impl Crypto,
        status: u16,
        body: &[u8],
        now: u64,
    ) {
        let document = core::str::from_utf8(body).unwrap_or_default();
        let pending = core::mem::replace(&mut self.pending, Pending::Nothing);
        match pending {
            Pending::Distribution => {
                let group_send = self.group_send.as_mut().and_then(|group_send| group_send.distribution.as_mut());
                if let Some((distribution, _)) = group_send {
                    distribution.take_answer(device, crypto, status, body, now);
                }
                return;
            }
            Pending::GroupKeys => return self.take_group_keys(device, crypto, status, document, now),
            Pending::Multi => return self.take_multi_answer(device, status, document, now),
            Pending::Keys => self.keys_status = Some(status),
            Pending::Submission => self.submit_status = Some(status),
            Pending::Nothing => return,
        }
        let Some(&recipient) = self.order.recipients.get(self.at) else { return };
        let result = match (pending, status) {
            (Pending::Keys, OK) => {
                self.begin_sessions(device, crypto, recipient, document, now);
                None
            }
            (Pending::Keys, UNAUTHORIZED | FORBIDDEN) if self.is_asked_sealed => {
                (self.is_failed_over, self.needs_keys) = (true, true);
                None
            }
            (Pending::Submission, UNAUTHORIZED | FORBIDDEN) if self.is_asked_sealed => {
                self.is_failed_over = true;
                None
            }
            (Pending::Keys, NOT_FOUND) => {
                device.archive_sessions(recipient);
                device.note_unregistered(recipient, now);
                Some(Outcome::Unregistered)
            }
            (_, NOT_FOUND) => {
                device.note_unregistered(recipient, now);
                Some(Outcome::Unregistered)
            }
            (Pending::Submission, OK) => {
                self.log_sent(device, recipient, now);
                if self.is_asked_sealed {
                    self.sealed_recipients.push(recipient);
                    device.note_sealed_delivered(recipient);
                }
                Some(Outcome::Sent)
            }
            (Pending::Submission, CONFLICT | GONE) if self.tries < MAX_TRIES => {
                self.tries += 1;
                self.needs_keys = set_aside(device, recipient, &service::parse_mismatch(document));
                None
            }
            _ => Some(halt_of(status).unwrap_or(Outcome::Failed)),
        };
        if let Some(outcome) = result {
            self.finish_account(device, outcome);
        }
    }

    fn take_group_keys(&mut self, device: &mut Device, crypto: &impl Crypto, status: u16, document: &str, now: u64) {
        self.keys_status = Some(status);
        let Some(member) = self.group_send.as_ref().and_then(|group_send| group_send.fetch_queue.first().copied())
        else {
            return;
        };
        let (is_done, is_one_to_one) = match status {
            OK => {
                self.begin_sessions(device, crypto, member, document, now);
                let is_untrusted = core::mem::replace(&mut self.trust, Trust::Trusted) != Trust::Trusted;
                if self.group_send.as_ref().is_some_and(|group_send| group_send.is_key_refused) {
                    device.note_sealed_refused(member);
                }
                (true, is_untrusted)
            }
            UNAUTHORIZED | FORBIDDEN => {
                let Some(group_send) = self.group_send.as_mut() else { return };
                match group_send.authority {
                    Authority::Key => {
                        (group_send.authority, group_send.is_key_refused) = (Authority::Token, true);
                        (false, false)
                    }
                    Authority::Token => {
                        group_send.authority = Authority::Own;
                        (false, false)
                    }
                    Authority::Own => (true, true),
                }
            }
            NOT_FOUND => {
                device.archive_sessions(member);
                device.note_unregistered(member, now);
                (true, false)
            }
            _ => match halt_of(status) {
                Some(outcome) => return self.stop(outcome),
                None => (true, true),
            },
        };
        let Some(group_send) = self.group_send.as_mut() else { return };
        if is_one_to_one {
            group_send.one_to_one.push(member);
        }
        if is_done {
            group_send.fetch_queue.remove(0);
            group_send.fetched_members.push(member);
            (group_send.authority, group_send.is_key_refused) = (Authority::Key, false);
            if group_send.fetch_queue.is_empty() {
                group_send.rounds += 1;
            }
        }
    }

    fn take_multi_answer(&mut self, device: &mut Device, status: u16, document: &str, now: u64) {
        self.submit_status = Some(status);
        let peer = self.order.peer;
        let Some(group_send) = self.group_send.as_mut() else { return };
        let recipients = core::mem::take(&mut group_send.recipients);
        match status {
            OK => {
                let unregistered = service::parse_not_on_signal(document);
                for account in &unregistered {
                    device.note_unregistered(*account, now);
                    device.archive_sessions(*account);
                }
                self.has_unregistered |= recipients.iter().any(|recipient| unregistered.contains(recipient));
                for recipient in recipients.iter().filter(|recipient| !unregistered.contains(recipient)) {
                    self.log_sent(device, *recipient, now);
                    device.note_sealed_delivered(*recipient);
                    self.sealed_recipients.push(*recipient);
                    self.accepted_recipients.push(*recipient);
                }
                self.order.recipients.retain(|recipient| !recipients.contains(recipient));
                self.group_send = None;
            }
            CONFLICT | GONE => {
                let mut stale = Vec::new();
                for (account, mismatch) in service::parse_mismatches(document) {
                    stale.extend(mismatch.stale.iter().map(|id| (account, *id)));
                    if set_aside(device, account, &mismatch) && !group_send.fetch_queue.contains(&account) {
                        group_send.fetch_queue.push(account);
                    }
                }
                device.drop_sender_devices(&peer, &stale);
                group_send.rounds += 1;
            }
            UNAUTHORIZED | NOT_FOUND => self.group_send = None,
            _ => match halt_of(status) {
                Some(outcome) => self.stop(outcome),
                None => {
                    self.has_refused = true;
                    self.order.recipients.retain(|recipient| !recipients.contains(recipient));
                    self.group_send = None;
                }
            },
        }
    }

    fn log_sent(&self, device: &mut Device, recipient: ServiceId, now: u64) {
        let order = &self.order;
        if order.is_logged {
            device.log_sent(recipient, (order.timestamp, order.is_urgent), &order.content, order.logged_peer, now);
        }
    }

    fn begin_sessions(
        &mut self,
        device: &mut Device,
        crypto: &impl Crypto,
        recipient: ServiceId,
        document: &str,
        now: u64,
    ) {
        (self.device_count, self.ready_count) = (0, 0);
        let is_own = recipient == device.account.aci();
        for (id, bundle) in service::parse_devices(document) {
            let address = Address { service: recipient, device: id };
            if is_own && id == device.account.device {
                continue;
            }
            self.device_count += 1;
            if device.sends_to(&address, now) {
                self.ready_count += 1;
                continue;
            }
            if device.trust(&recipient, &bundle.identity, now) != Trust::Trusted {
                device.save_identity(recipient, bundle.identity, Some(id), now);
                self.trust = self.trust.max(device.trust(&recipient, &bundle.identity, now));
                continue;
            }
            let Ok(state) =
                State::begin(crypto, &device.account.aci_identity, device.account.registration, &bundle, is_own, now)
            else {
                continue;
            };
            device.begin_session(address, state);
            device.save_identity(recipient, bundle.identity, Some(id), now);
            self.ready_count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::content::DecryptionError;
    use super::super::curve::Pair;
    use super::super::device::tests::{certificate_answer, keys_document, member, session_with};
    use super::super::group::tests::endorsement_server;
    use super::super::sealed::tests::{content_for, to_shares};
    use super::super::sender_keys::{Distribution, Record as SenderKeys};
    use super::super::tests::Standin;
    use super::*;
    use crate::base::proto::Message;
    use alloc::vec;

    const ANNA: ServiceId = ServiceId::Aci([1; 16]);
    const BORIS: ServiceId = ServiceId::Aci([2; 16]);
    const CAROL: ServiceId = ServiceId::Aci([6; 16]);
    const HOUR: u64 = 60 * 60 * 1000;

    fn endorsed_group(device: &mut Device, crypto: &Standin) -> (Peer, [Pair; 3], u64) {
        let (server, now) = endorsement_server(crypto);
        let anna = member(device, crypto, ANNA, &[(1, 7)], now);
        let boris = member(device, crypto, BORIS, &[(1, 8), (2, 9)], now);
        let members = [device.account.aci(), ANNA, BORIS, CAROL];
        let group = device.add_endorsed_group(crypto, &[9; 32], &members, (&server, now));
        device.take_certificate(false, &certificate_answer(now + 10 * HOUR, false), now);
        (group, [anna, boris, Pair::new(crypto).unwrap()], now)
    }

    fn to_many(
        errand: &mut Errand,
        device: &mut Device,
        crypto: &Standin,
        now: u64,
        carol: &Pair,
    ) -> (Vec<String>, Step) {
        let mut paths = Vec::new();
        loop {
            match errand.next(device, crypto, now) {
                Step::Keys { path, .. } if path == service::bundles_path(&CAROL) => {
                    errand.take_answer(device, crypto, 200, keys_document(crypto, carol).as_bytes(), now);
                }
                Step::Submit { path, .. } => {
                    paths.push(path);
                    errand.take_answer(device, crypto, 200, b"", now);
                }
                many @ Step::Multi { .. } => return (paths, many),
                other => panic!("{other:?}"),
            }
        }
    }

    fn one_to_one(errand: &mut Errand, device: &mut Device, crypto: &Standin, now: u64) -> (Vec<String>, Outcome) {
        let mut paths = Vec::new();
        loop {
            match errand.next(device, crypto, now) {
                Step::Submit { path, .. } => {
                    paths.push(path);
                    errand.take_answer(device, crypto, 200, b"", now);
                }
                Step::Done(outcome) => return (paths, outcome),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn sender_key_send() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let (group, [anna, _, carol], now) = endorsed_group(&mut device, &crypto);
        let key = device.sender_key(&crypto, &group, now).expect("a sender key");
        let distribution = device.own_distribution(&crypto, &key.distribution).expect("its distribution message");
        let mut errand = Errand::begin(Order::message(group, b"hi all".to_vec(), now, true));
        assert_eq!(
            errand.next(&mut device, &crypto, now),
            Step::Keys { path: service::bundles_path(&CAROL), access: Some(Access::Key([0; 16])) },
            "Carol has no session: her pre-keys first, under her access key, noughts while her profile key is not held"
        );
        errand.take_answer(&mut device, &crypto, 401, b"", now);
        let token = device.group_token(&crypto, &group, &[CAROL], now).map(Access::Token);
        assert_eq!(
            errand.next(&mut device, &crypto, now),
            Step::Keys { path: service::bundles_path(&CAROL), access: token },
            "her access key refused: under the group's token for her"
        );
        errand.take_answer(&mut device, &crypto, 200, keys_document(&crypto, &carol).as_bytes(), now);
        assert_eq!(device.member_access(&crypto, &CAROL), None, "her access key refused, her sealing is");
        let (paths, many) = to_many(&mut errand, &mut device, &crypto, now, &carol);
        assert_eq!(
            paths,
            [service::messages_path(&ANNA), service::messages_path(&BORIS), service::messages_path(&CAROL)],
            "the sender key distributed one to one to each member but the owner"
        );
        assert_eq!(
            device.sender_key(&crypto, &group, now).map(|key| key.devices),
            Some(vec![(ANNA, 1, 7), (BORIS, 1, 8), (BORIS, 2, 9), (CAROL, 1, 7)]),
            "each device that has it, by its registration"
        );
        let Step::Multi { path, body, token } = many else { panic!("sealed to many") };
        assert_eq!(
            (path, Some(token)),
            (service::multi_recipient_path(now, true), device.group_token(&crypto, &group, &[ANNA, BORIS, CAROL], now)),
            "under the group's token for the three"
        );
        let parts = to_shares(&body);
        assert_eq!(
            parts.iter().map(|(id, devices, _)| (*id, devices.clone())).collect::<Vec<_>>(),
            [(ANNA, vec![(1, 7)]), (BORIS, vec![(1, 8), (2, 9)]), (CAROL, vec![(1, 7)])],
            "each member's devices, in the order of their ids as written"
        );
        let content = content_for(&crypto, &anna, &parts[0].2);
        let sealed = Message::parse(&content).unwrap();
        let Peer::Group(id) = group else { panic!("a group") };
        assert_eq!(
            (sealed.varint(1), sealed.varint(4), sealed.bytes(5)),
            (Some(7), Some(1), Some(&id[..])),
            "a sender key message, resendable, to the group, sealed to Anna"
        );
        let mut record = SenderKeys::default();
        record.add_distribution(&Distribution::read(&distribution).unwrap());
        let plain = record.open(&crypto, sealed.bytes(3).unwrap()).expect("Anna opens it by the distribution she took");
        assert_eq!(content::unpad(&plain), Some(&b"hi all"[..]));
        errand.take_answer(
            &mut device,
            &crypto,
            200,
            format!(r#"{{"uuids404":["{}"]}}"#, CAROL.to_text()).as_bytes(),
            now,
        );
        assert_eq!(errand.next(&mut device, &crypto, now), Step::Done(Outcome::Sent));
        assert_eq!(
            (errand.accepted_recipients.clone(), errand.sealed_recipients.clone()),
            (vec![ANNA, BORIS], vec![ANNA, BORIS]),
            "Anna and Boris took it sealed; Carol is not on Signal"
        );
        assert!(
            device.sent_content(ANNA, now).is_some()
                && device.sent_content(CAROL, now).is_none()
                && device.is_off_signal(&CAROL, now)
        );
    }

    #[test]
    fn call_signal_to_members() {
        use super::super::call::{GroupSignal, Signal};
        use super::super::command::{Outbound, Target};
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let (group, [anna, _, carol], now) = endorsed_group(&mut device, &crypto);
        let leaving = Signal::Group { signal: GroupSignal::Leaving { group, demux_id: 16 }, is_urgent: false };
        let outbound =
            Outbound { target: Target::Members(group, vec![ANNA, BORIS, CAROL]), device: None, signal: leaving };
        let order = device.signal_orders(outbound, now).remove(0);
        let mut errand = Errand::begin(order.clone());
        let (_, many) = to_many(&mut errand, &mut device, &crypto, now, &carol);
        let Step::Multi { path, body, .. } = many else { panic!("sealed to many") };
        assert_eq!(
            path,
            service::multi_recipient_path(order.timestamp, false),
            "the members named, at once under the R1's sender key, not urgent"
        );
        let content = content_for(&crypto, &anna, &to_shares(&body)[0].2);
        assert_eq!(Message::parse(&content).unwrap().varint(4), None, "of no content hint");
        errand.take_answer(&mut device, &crypto, 200, b"{}", now);
        assert_eq!(errand.next(&mut device, &crypto, now), Step::Done(Outcome::Sent), "sent");
        assert_eq!(device.sent_content(ANNA, order.timestamp), None, "kept in no send log");
    }

    #[test]
    fn sender_key_failover() {
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let (server, now) = endorsement_server(&crypto);
        member(&mut device, &crypto, ANNA, &[(1, 7)], now);
        let pair = device.add_endorsed_group(&crypto, &[8; 32], &[device.account.aci(), ANNA], (&server, now));
        let mut errand = Errand::begin(Order::message(pair, vec![1], now, true));
        assert_eq!(
            one_to_one(&mut errand, &mut device, &crypto, now),
            (vec![service::messages_path(&ANNA)], Outcome::Sent),
            "one member is too few"
        );
        let mut device = Device::unkeyed();
        let (group, [.., carol], now) = endorsed_group(&mut device, &crypto);
        let mut errand = Errand::begin(Order::message(group, vec![1], now, true));
        to_many(&mut errand, &mut device, &crypto, now, &carol);
        errand.take_answer(&mut device, &crypto, 401, b"", now);
        assert_eq!(
            one_to_one(&mut errand, &mut device, &crypto, now),
            (
                vec![service::messages_path(&ANNA), service::messages_path(&BORIS), service::messages_path(&CAROL)],
                Outcome::Sent
            ),
            "the token refused: one to one to each"
        );
        let mut device = Device::unkeyed();
        let (group, [.., carol], now) = endorsed_group(&mut device, &crypto);
        let mut errand = Errand::begin(Order::message(group, vec![1], now, true));
        to_many(&mut errand, &mut device, &crypto, now, &carol);
        errand.take_answer(&mut device, &crypto, 428, b"", now);
        assert_eq!(
            errand.next(&mut device, &crypto, now),
            Step::Done(Outcome::Challenged),
            "a challenge ends it whole"
        );
        let mut device = Device::unkeyed();
        device.add_endorsed_group(&crypto, &[9; 32], &[device.account.aci(), ANNA], (&server, now));
        let unendorsed = device.add_group(&crypto, &[9; 32], &[device.account.aci(), ANNA, BORIS]);
        member(&mut device, &crypto, ANNA, &[(1, 7)], now);
        member(&mut device, &crypto, BORIS, &[(1, 8)], now);
        let mut errand = Errand::begin(Order::message(unendorsed, vec![1], now, true));
        assert_eq!(
            errand.next(&mut device, &crypto, now),
            Step::Wait,
            "a member without an endorsement: the group asked anew"
        );
        assert_eq!((device.group_due(&unendorsed), errand.awaited_group()), ((None, true), Some(unendorsed)));
        assert_eq!(
            one_to_one(&mut errand, &mut device, &crypto, now),
            (vec![service::messages_path(&ANNA), service::messages_path(&BORIS)], Outcome::Sent),
            "asked anew once, still without: one to one"
        );
    }

    #[test]
    fn multi_recipient_mismatch() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let (group, [anna, boris, carol], now) = endorsed_group(&mut device, &crypto);
        let mut errand = Errand::begin(Order::message(group, vec![1], now, true));
        to_many(&mut errand, &mut device, &crypto, now, &carol);
        let mismatch =
            format!(r#"[{{"uuid":"{}","devices":{{"missingDevices":[3],"extraDevices":[2]}}}}]"#, BORIS.to_text());
        errand.take_answer(&mut device, &crypto, 409, mismatch.as_bytes(), now);
        assert!(!device.sends_to(&Address { service: BORIS, device: 2 }, now), "the extra device's session set aside");
        assert!(
            matches!(errand.next(&mut device, &crypto, now), Step::Keys { ref path, .. } if *path == service::bundles_path(&BORIS)),
            "the missing device's pre-keys asked"
        );
        errand.take_answer(&mut device, &crypto, 200, keys_document(&crypto, &boris).as_bytes(), now);
        let Step::Multi { body, .. } = errand.next(&mut device, &crypto, now) else { panic!("sealed to many again") };
        assert_eq!(to_shares(&body)[1].1, [(1, 8)], "Boris's one device left");
        assert!(
            !device.sender_key(&crypto, &group, now).unwrap().devices.contains(&(BORIS, 2, 9)),
            "the device gone no longer counts as having the key"
        );
        let stale = format!(r#"[{{"uuid":"{}","devices":{{"staleDevices":[1]}}}}]"#, ANNA.to_text());
        errand.take_answer(&mut device, &crypto, 410, stale.as_bytes(), now);
        assert!(
            !device.sender_key(&crypto, &group, now).unwrap().devices.iter().any(|(account, ..)| *account == ANNA),
            "a stale device must have the key anew"
        );
        assert!(
            matches!(errand.next(&mut device, &crypto, now), Step::Keys { ref path, .. } if *path == service::bundles_path(&ANNA)),
            "its pre-keys asked anew"
        );
        errand.take_answer(&mut device, &crypto, 200, keys_document(&crypto, &anna).as_bytes(), now);
        let (paths, _) = to_many(&mut errand, &mut device, &crypto, now, &carol);
        assert_eq!(paths, [service::messages_path(&ANNA)], "the key distributed to Anna's device anew");
        errand.take_answer(&mut device, &crypto, 200, b"{}", now);
        assert_eq!(errand.next(&mut device, &crypto, now), Step::Done(Outcome::Sent));
        let (server, now) = endorsement_server(&crypto);
        let distribution = device.sender_key(&crypto, &group, now).unwrap().distribution;
        let first = device.own_distribution(&crypto, &distribution).unwrap();
        device.add_endorsed_group(&crypto, &[9; 32], &[device.account.aci(), ANNA, CAROL], (&server, now));
        let mut next = Errand::begin(Order::message(group, vec![2], now + 1, true));
        let (paths, _) = to_many(&mut next, &mut device, &crypto, now, &carol);
        let second = device.own_distribution(&crypto, &distribution).unwrap();
        assert_eq!(
            paths,
            [service::messages_path(&ANNA), service::messages_path(&CAROL)],
            "Boris left the group: the key made anew and distributed again"
        );
        assert_ne!(
            Distribution::read(&first).unwrap(),
            Distribution::read(&second).unwrap(),
            "a new chain of the same distribution"
        );
    }

    #[test]
    fn receipt_under_group_token() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let (server, now) = endorsement_server(&crypto);
        member(&mut device, &crypto, ANNA, &[(1, 7)], now);
        device.take_certificate(false, &certificate_answer(now + 10 * HOUR, false), now);
        let group = device.add_group(&crypto, &[9; 32], &[device.account.aci(), ANNA]);
        device.note_sealed_refused(ANNA);
        let mut receipt = Errand::begin(Order::new(ANNA, vec![1], now, false).with_endorsing_group(group));
        assert_eq!(receipt.next(&mut device, &crypto, now), Step::Wait, "no endorsements held: the group asked anew");
        assert_eq!((device.group_due(&group), receipt.awaited_group()), ((None, true), Some(group)));
        device.add_endorsed_group(&crypto, &[9; 32], &[device.account.aci(), ANNA], (&server, now));
        let token = device.group_token(&crypto, &group, &[ANNA], now).map(Access::Token);
        let Step::Submit { access, .. } = receipt.next(&mut device, &crypto, now) else { panic!("a submission") };
        assert!(access.is_some() && access == token, "sealed under the group's token for her");
        let direct = Order::new(ANNA, vec![1], now, false).with_endorsing_group(Peer::Contact(ANNA));
        let mut direct = Errand::begin(direct);
        assert!(
            matches!(direct.next(&mut device, &crypto, now), Step::Submit { access: None, .. }),
            "of no group: unsealed"
        );
    }

    #[test]
    fn failure_lines() {
        let (mut device, recipient, crypto) = (Device::unkeyed(), ServiceId::Aci([4; 16]), Standin::default());
        let mut challenged = Errand::begin(Order::message(Peer::Contact(recipient), vec![1], 5, true));
        assert!(
            matches!(challenged.next(&mut device, &crypto, 5), Step::Keys { .. }),
            "no session: the pre-keys first"
        );
        assert_eq!(challenged.failure_line(), None, "nothing ended yet");
        challenged.take_answer(&mut device, &crypto, 428, b"", 5);
        assert_eq!(challenged.next(&mut device, &crypto, 5), Step::Done(Outcome::Challenged));
        assert_eq!(
            challenged.failure_line().expect("ended"),
            "a message not sent, Challenged: pre-keys 428, 0 of 0 devices with a session, submission not asked, put right 0 times",
            "a challenge to the pre-keys, no submission made"
        );
        let mut errand = Errand::begin(Order::message(Peer::Contact(recipient), vec![1], 5, true));
        errand.next(&mut device, &crypto, 5);
        errand.take_answer(&mut device, &crypto, 200, br#"{"identityKey":"","devices":[]}"#, 5);
        assert_eq!(errand.next(&mut device, &crypto, 5), Step::Done(Outcome::Failed), "pre-keys of no device");
        assert_eq!(
            errand.failure_line().expect("ended"),
            "a message not sent, Failed: pre-keys 200, 0 of 0 devices with a session, submission not asked, put right 0 times",
            "the pre-keys answered and named no device to send to"
        );
        let mut receipt = Errand::begin(Order::new(recipient, vec![1], 5, false));
        (receipt.outcome, receipt.keys_status, receipt.submit_status, receipt.tries) =
            (Some(Outcome::Failed), Some(200), Some(409), 3);
        assert_eq!(
            receipt.failure_line().expect("ended"),
            "a receipt or a sync not sent, Failed: pre-keys 200, 0 of 0 devices with a session, submission 409, put right 3 times"
        );
    }

    #[test]
    fn outcome_status() {
        assert_eq!(
            [Outcome::Sent, Outcome::Unregistered, Outcome::Challenged, Outcome::Failed].map(Outcome::to_status),
            [Some(Status::Sent), Some(Status::Failed), Some(Status::Failed), Some(Status::Failed)],
            "sent, or not sent whatever refused it"
        );
        assert_eq!(Outcome::Later.to_status(), None, "one tried again later stays as it stands");
    }

    #[test]
    fn unendorsed_group_send() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let (anna, boris) = (ServiceId::Aci([1; 16]), ServiceId::Aci([2; 16]));
        session_with(&mut device, &crypto, Address { service: anna, device: 1 }, 7);
        session_with(&mut device, &crypto, Address { service: boris, device: 2 }, 8);
        let own = device.account.aci();
        let group = device.add_group(&crypto, &[9; 32], &[anna]);
        let mut errand = Errand::begin(Order::message(group, vec![1], 5, true));
        device.add_group(&crypto, &[9; 32], &[own, anna, boris]);
        assert_eq!(
            errand.next(&mut device, &crypto, 5),
            Step::Wait,
            "no endorsements: the group's state asked anew first"
        );
        let Step::Submit { path, body, .. } = errand.next(&mut device, &crypto, 5) else { panic!("a submission") };
        assert!(
            path == service::messages_path(&anna) && body.contains(r#""destinationRegistrationId":7"#),
            "Anna's first"
        );
        errand.take_answer(&mut device, &crypto, 200, b"", 5);
        let Step::Submit { path, .. } = errand.next(&mut device, &crypto, 5) else { panic!("a submission") };
        assert_eq!(path, service::messages_path(&boris), "then Boris's, who joined before it began; never the owner's");
        errand.take_answer(&mut device, &crypto, 404, b"", 5);
        assert_eq!(errand.next(&mut device, &crypto, 5), Step::Done(Outcome::Sent), "one took it");
        let empty = device.add_group(&crypto, &[8; 32], &[]);
        let mut nobody = Errand::begin(Order::message(empty, vec![1], 5, true));
        assert_eq!(
            nobody.next(&mut device, &crypto, 5),
            Step::Done(Outcome::Failed),
            "a group the owner is no member of"
        );
        let mut later = Errand::begin(Order::message(group, vec![1], 5, true));
        assert_eq!(later.next(&mut device, &crypto, 5), Step::Wait);
        later.next(&mut device, &crypto, 5);
        later.take_answer(&mut device, &crypto, 429, b"", 5);
        assert_eq!(later.next(&mut device, &crypto, 5), Step::Done(Outcome::Later), "a wait asked ends it whole");
        device.add_group(&crypto, &[9; 32], &[own, boris]);
        let mut again = Errand::begin(later.order);
        assert_eq!(again.next(&mut device, &crypto, 5), Step::Wait, "begun anew, it asks the group anew");
        let Step::Submit { path, .. } = again.next(&mut device, &crypto, 5) else { panic!("a submission") };
        assert_eq!(path, service::messages_path(&boris), "tried again, it goes to the members as they are then");
    }

    #[test]
    fn resend_request_plain() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let anna = ServiceId::Aci([1; 16]);
        session_with(&mut device, &crypto, Address { service: anna, device: 1 }, 7);
        session_with(&mut device, &crypto, Address { service: anna, device: 3 }, 9);
        let error = DecryptionError { ratchet: Some([4; 32]), timestamp: 1_700, device: 3 };
        let mut errand = Errand::begin(Order::resend_request(anna, &error, None));
        let Step::Submit { path, body, access: None } = errand.next(&mut device, &crypto, 2_000) else {
            panic!("a submission, on the account's socket where no certificate is held")
        };
        let plain = crate::base::code::base64(&content::write_decryption_error(&error));
        assert_eq!(path, service::messages_path(&anna));
        assert_eq!(
            body,
            format!(
                r#"{{"messages":[{{"type":8,"destinationDeviceId":1,"destinationRegistrationId":7,"content":"{plain}"}},{{"type":8,"destinationDeviceId":3,"destinationRegistrationId":9,"content":"{plain}"}}],"online":false,"urgent":false,"timestamp":1700}}"#
            ),
            "to each device, unsealed, under its registration, the timestamp the message's, not urgent"
        );
        errand.take_answer(&mut device, &crypto, 200, b"", 2_000);
        assert_eq!(device.sent_content(anna, 1_700), None, "a request to send again is not kept to be sent again");
    }

    #[test]
    fn mismatch_sessions() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let anna = ServiceId::Aci([1; 16]);
        for id in [1, 2, 3] {
            session_with(&mut device, &crypto, Address { service: anna, device: id }, id);
        }
        let mut errand = Errand::begin(Order::new(anna, vec![1], 5, false));
        errand.next(&mut device, &crypto, 5);
        errand.take_answer(&mut device, &crypto, 409, br#"{"missingDevices":[],"extraDevices":[2]}"#, 5);
        assert_eq!(
            device.sendable_devices(anna, 5),
            [Address { service: anna, device: 1 }, Address { service: anna, device: 3 }],
            "the extra device's session set aside, its record kept"
        );
        assert!(device.has_session(&Address { service: anna, device: 2 }), "set aside, not let go");
        errand.next(&mut device, &crypto, 5);
        errand.take_answer(&mut device, &crypto, 410, br#"{"staleDevices":[3]}"#, 5);
        assert!(
            matches!(errand.next(&mut device, &crypto, 5), Step::Keys { .. }),
            "a stale device's pre-keys asked anew"
        );
        errand.take_answer(&mut device, &crypto, 404, b"", 5);
        assert!(device.sendable_devices(anna, 5).is_empty(), "no pre-keys: every session set aside");
        assert_eq!(errand.next(&mut device, &crypto, 5), Step::Done(Outcome::Unregistered));
    }
}
