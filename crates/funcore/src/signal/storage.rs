//! The storage service, read for names and groups.

use super::account::{Account, NOTIFY_IF_MUTED};
use super::address::ServiceId;
use super::message::decode_key;
use super::service::{Request, storage_headers, storage_url};
use super::{Crypto, hkdf};
use crate::base::json;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::codec::http::{GET, OK, PUT};
use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const STORAGE: &[u8] = b"Storage Service Encryption";
const ITEM: &[u8] = b"20240801_SIGNAL_STORAGE_SERVICE_ITEM_";
const CONTACT: u64 = 1;
const GROUP: u64 = 3;
const ACCOUNT: u64 = 4;
const EVERYBODY: u64 = 1;
const ENABLED: u64 = 1;
const DISABLED: u64 = 2;
const READ_BATCH: usize = 2_500;
const MANIFEST: &str = "/v1/storage/manifest";
const READ: &str = "/v1/storage/read";

fn storage_key(crypto: &impl Crypto, master: &[u8; 32]) -> [u8; 32] {
    crypto.hmac_sha256(master, &[STORAGE])
}

pub(super) fn parse_credentials(document: &str) -> Option<(String, String)> {
    Some((String::from(json::text_at(document, &["username"])?), String::from(json::text_at(document, &["password"])?)))
}

fn open_item(crypto: &impl Crypto, key: &[u8; 32], sealed: &[u8]) -> Option<Vec<u8>> {
    let (nonce, rest) = sealed.split_first_chunk::<12>()?;
    crypto.gcm_decrypt(key, nonce, &[], rest)
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Manifest {
    pub(super) version: u64,
    pub(super) records: Vec<Vec<u8>>,
    pub(super) record_key: Vec<u8>,
}

impl core::fmt::Debug for Manifest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Manifest").field("version", &self.version).finish_non_exhaustive()
    }
}

fn read_manifest(crypto: &impl Crypto, storage: &[u8; 32], bytes: &[u8]) -> Option<Manifest> {
    let outer = Message::parse(bytes)?;
    let version = outer.varint(1)?;
    let key = crypto.hmac_sha256(storage, &[format!("Manifest_{version}").as_bytes()]);
    let plain = open_item(crypto, &key, outer.bytes(2)?)?;
    let record = Message::parse(&plain)?;
    let records = record
        .repeated(2)
        .filter_map(Message::parse)
        .filter(|identifier| matches!(identifier.varint(2), Some(CONTACT | GROUP | ACCOUNT)))
        .filter_map(|identifier| identifier.bytes(1).map(<[u8]>::to_vec))
        .collect();
    Some(Manifest { version, records, record_key: record.bytes(4).filter(|key| !key.is_empty())?.to_vec() })
}

fn write_read_operation(ids: &[Vec<u8>]) -> Vec<u8> {
    ids.iter().fold(Writer::new(), |writer, id| writer.bytes(1, id)).finish()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct ListState {
    pub(crate) archived: bool,
    pub(crate) marked_unread: bool,
    pub(crate) muted_until: u64,
}

impl ListState {
    fn read(record: &Message, (archived, marked_unread, muted_until): (u32, u32, u32)) -> ListState {
        ListState {
            archived: record.bool(archived).unwrap_or(false),
            marked_unread: record.bool(marked_unread).unwrap_or(false),
            muted_until: record.varint(muted_until).unwrap_or(0),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ContactRecord {
    pub(super) aci: ServiceId,
    pub(super) name: Option<String>,
    pub(super) number: Option<String>,
    pub(super) username: Option<String>,
    pub(super) pni: Option<ServiceId>,
    pub(super) profile_key: Option<[u8; 32]>,
    pub(super) is_system: bool,
    pub(super) blocked: bool,
    pub(super) whitelisted: bool,
    pub(super) hidden: bool,
    pub(super) unregistered: u64,
    pub(super) identity: Option<([u8; 32], u32)>,
    pub(super) list_state: ListState,
    // Whether its conversation tells calls while muted, where the record says.
    pub(super) notify_calls: Option<bool>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct GroupRecord {
    pub(super) master_key: [u8; 32],
    pub(super) blocked: bool,
    pub(super) whitelisted: bool,
    pub(super) list_state: ListState,
    // Whether it tells mentions, replies and calls while muted, where the record says.
    pub(super) notify: (Option<bool>, Option<bool>, Option<bool>),
}

impl core::fmt::Debug for GroupRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GroupRecord")
            .field("blocked", &self.blocked)
            .field("whitelisted", &self.whitelisted)
            .field("listed", &self.list_state)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Pin {
    Contact(ServiceId),
    Number(String),
    Group([u8; 32]),
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct AccountRecord {
    pub(super) profile_key: Option<[u8; 32]>,
    pub(super) read_receipts: bool,
    pub(super) is_number_shared: bool,
    pub(super) note: ListState,
    pub(super) pins: Vec<Pin>,
    pub(super) keep_muted_archived: bool,
    pub(super) count_muted: bool,
    pub(super) notify: (bool, bool, bool),
}

impl core::fmt::Debug for AccountRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AccountRecord")
            .field("read_receipts", &self.read_receipts)
            .field("number_shared", &self.is_number_shared)
            .field("pinned", &self.pins.len())
            .finish_non_exhaustive()
    }
}

fn join_names(given: Option<&str>, family: Option<&str>) -> Option<String> {
    let parts: Vec<&str> =
        [given, family].into_iter().flatten().map(str::trim).filter(|part| !part.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn read_contact(record: &Message) -> Option<ContactRecord> {
    let aci = match record.bytes(25).filter(|bytes| bytes.len() == 16) {
        Some(bytes) => ServiceId::Aci(bytes.try_into().ok()?),
        None => ServiceId::from_text(record.text(1)?).filter(|id| matches!(id, ServiceId::Aci(_)))?,
    };
    let nickname = record.message(22).and_then(|name| join_names(name.text(1), name.text(2)));
    let system_name = record
        .text(19)
        .filter(|nick| !nick.is_empty())
        .map(String::from)
        .or_else(|| join_names(record.text(17), record.text(18)));
    let profile_name = join_names(record.text(6), record.text(7));
    let has_name = |field| record.text(field).is_some_and(|name| !name.is_empty());
    let pni = match record.bytes(26).filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => Some(ServiceId::Pni(bytes.try_into().ok()?)),
        None => record.text(15).and_then(|text| ServiceId::from_text(&format!("PNI:{text}"))),
    };
    let identity = record.bytes(4).and_then(decode_key).map(|key| (key, record.u32(5).unwrap_or(0)));
    Some(ContactRecord {
        aci,
        name: nickname.or(system_name).or(profile_name),
        number: record.text(2).filter(|number| !number.is_empty()).map(String::from),
        username: record.text(8).filter(|username| !username.is_empty()).map(String::from),
        pni,
        profile_key: record.bytes(3).and_then(|key| key.try_into().ok()),
        is_system: has_name(17) || has_name(18),
        blocked: record.bool(9).unwrap_or(false),
        whitelisted: record.bool(10).unwrap_or(false),
        hidden: record.bool(20).unwrap_or(false),
        unregistered: record.varint(16).unwrap_or(0),
        identity,
        list_state: ListState::read(record, (11, 12, 13)),
        notify_calls: read_optional(record, 28),
    })
}

fn read_optional(record: &Message, field: u32) -> Option<bool> {
    match record.varint(field) {
        Some(ENABLED) => Some(true),
        Some(DISABLED) => Some(false),
        _ => None,
    }
}

fn read_group(record: &Message) -> Option<GroupRecord> {
    let mentions = if record.bool(7).unwrap_or(false) { Some(false) } else { read_optional(record, 15) };
    Some(GroupRecord {
        master_key: record.bytes(1)?.try_into().ok()?,
        blocked: record.bool(2).unwrap_or(false),
        whitelisted: record.bool(3).unwrap_or(false),
        list_state: ListState::read(record, (4, 5, 6)),
        notify: (mentions, read_optional(record, 16), read_optional(record, 14)),
    })
}

fn read_pin(pin: &Message) -> Option<Pin> {
    if let Some(key) = pin.bytes(4) {
        return Some(Pin::Group(key.try_into().ok()?));
    }
    let contact = pin.message(1)?;
    match contact.bytes(3).filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => ServiceId::from_binary(bytes).map(Pin::Contact),
        None => match contact.text(1).and_then(ServiceId::from_text) {
            Some(service) => Some(Pin::Contact(service)),
            None => contact.text(2).filter(|number| !number.is_empty()).map(|number| Pin::Number(number.into())),
        },
    }
}

fn read_account(record: &Message) -> AccountRecord {
    AccountRecord {
        profile_key: record.bytes(1).and_then(|key| key.try_into().ok()),
        read_receipts: record.bool(6).unwrap_or(false),
        is_number_shared: record.varint(12) == Some(EVERYBODY),
        note: ListState {
            archived: record.bool(5).unwrap_or(false),
            marked_unread: record.bool(10).unwrap_or(false),
            muted_until: 0,
        },
        pins: record.repeated(14).filter_map(Message::parse).filter_map(|item| read_pin(&item)).collect(),
        keep_muted_archived: record.bool(25).unwrap_or(false),
        count_muted: read_optional(record, 54).unwrap_or(false),
        notify: (
            read_optional(record, 57).unwrap_or(NOTIFY_IF_MUTED.0),
            read_optional(record, 58).unwrap_or(NOTIFY_IF_MUTED.1),
            read_optional(record, 56).unwrap_or(NOTIFY_IF_MUTED.2),
        ),
    }
}

// Takes the records of an answer to a read; how many did not open, an answer that is none counting as one.
fn read_records(crypto: &impl Crypto, manifest: &Manifest, answer: &[u8], records: &mut StorageRecords) -> usize {
    let Some(items) = Message::parse(answer) else { return 1 };
    let mut unopened = 0;
    for item in items.repeated(1).filter_map(Message::parse) {
        let plain = (|| {
            let id = item.bytes(1)?;
            let key = hkdf(crypto, None, &manifest.record_key, &[ITEM, id])?;
            open_item(crypto, &key, item.bytes(2)?)
        })();
        let Some(record) = plain.as_deref().and_then(Message::parse) else {
            unopened += 1;
            continue;
        };
        records.contacts.extend(record.message(1).and_then(|record| read_contact(&record)));
        records.groups.extend(record.message(3).and_then(|record| read_group(&record)));
        if let Some(record) = record.message(4) {
            records.account = Some(read_account(&record));
        }
    }
    unopened
}

/// A reading of the contacts, groups and account settings under the authorization of `GET /v1/storage/auth`: the manifest, then its records `READ_BATCH` at a time.
pub struct StorageRead {
    authorization: String,
    key: [u8; 32],
    manifest: Option<Manifest>,
    next: usize,
    records: StorageRecords,
    refusals: Vec<u16>,
    unopened: usize,
    failure: Option<String>,
}

impl StorageRead {
    /// A reading under an authorization, with the storage key of the account's master key.
    pub fn new(crypto: &impl Crypto, account: &Account, authorization: String) -> StorageRead {
        StorageRead {
            authorization,
            key: storage_key(crypto, &account.master_key),
            manifest: None,
            next: 0,
            records: StorageRecords { version: 0, contacts: Vec::new(), groups: Vec::new(), account: None },
            refusals: Vec::new(),
            unopened: 0,
            failure: None,
        }
    }

    /// The next request; none once the reading is over.
    pub fn next_request(&mut self) -> Option<Request<'static>> {
        if self.failure.is_some() {
            return None;
        }
        let (method, path, body) = match &self.manifest {
            None => (GET, MANIFEST, Vec::new()),
            Some(manifest) => {
                let ids = &manifest.records[self.next..];
                if ids.is_empty() {
                    return None;
                }
                let ids = &ids[..ids.len().min(READ_BATCH)];
                self.next += ids.len();
                (PUT, READ, write_read_operation(ids))
            }
        };
        Some(Request::new(method, storage_url(path), storage_headers(self.authorization.clone()), Cow::Owned(body)))
    }

    /// Takes the service's answer to the last request.
    pub fn take_answer(&mut self, crypto: &impl Crypto, status: u16, body: &[u8]) {
        match &self.manifest {
            None if status != OK => self.failure = Some(format!("the manifest: {status}")),
            None => match read_manifest(crypto, &self.key, body) {
                Some(manifest) => self.manifest = Some(manifest),
                None => self.failure = Some(String::from("a manifest that does not open")),
            },
            Some(_) if status != OK => self.refusals.push(status),
            Some(manifest) => self.unopened += read_records(crypto, manifest, body, &mut self.records),
        }
    }

    /// Ends the reading, a request having failed for this reason.
    pub fn abort(&mut self, reason: String) {
        self.failure = Some(reason);
    }

    pub(super) fn finish(self) -> (Option<StorageRecords>, String) {
        let manifest = match (self.failure, self.manifest) {
            (Some(reason), _) => return (None, format!("no names from the storage service: {reason}")),
            (None, None) => return (None, String::from("no names from the storage service: no manifest")),
            (None, Some(manifest)) => manifest,
        };
        let records = StorageRecords { version: manifest.version, ..self.records };
        let has_none = records.contacts.is_empty() && records.groups.is_empty() && records.account.is_none();
        if self.unopened > 0 && has_none {
            let line = format!("no names from the storage service: {} records that do not open", self.unopened);
            return (None, line);
        }
        let mut line =
            format!("{} names and {} groups from the storage service", records.contacts.len(), records.groups.len());
        if records.account.is_some() {
            line.push_str(", the account's settings");
        }
        for status in self.refusals {
            let _ = write!(line, ", a read answered {status}");
        }
        if self.unopened > 0 {
            let _ = write!(line, ", {} records that do not open", self.unopened);
        }
        (Some(records), line)
    }
}

pub(super) struct StorageRecords {
    pub(super) version: u64,
    pub(super) contacts: Vec<ContactRecord>,
    pub(super) groups: Vec<GroupRecord>,
    pub(super) account: Option<AccountRecord>,
}

#[cfg(test)]
pub(super) mod samples {
    use super::{ContactRecord, ListState};
    use crate::signal::address::ServiceId;

    // A contact's record that tells nothing but whose it is.
    pub(in crate::signal) fn contact_record(aci: ServiceId) -> ContactRecord {
        ContactRecord {
            aci,
            name: None,
            number: None,
            username: None,
            pni: None,
            profile_key: None,
            is_system: false,
            blocked: false,
            whitelisted: false,
            hidden: false,
            unregistered: 0,
            identity: None,
            list_state: ListState::default(),
            notify_calls: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::device::{Device, File};
    use super::super::tests::Standin;
    use super::*;
    use crate::crypto::Primitives;
    use alloc::vec;

    const MASTER: [u8; 32] = [6; 32];
    const AUTHORIZATION: &str = "Basic YTpi";
    const RECORD_KEY: &[u8] = b"records";
    const GROUP_ID: &[u8] = b"group";
    const ACCOUNT_ID: &[u8] = b"account";
    const CLIMBING: [u8; 32] = [8; 32];

    fn account() -> Account {
        Account { master_key: MASTER, ..Device::unkeyed().account }
    }

    fn seal(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
        [&[0u8; 12][..], &Standin::seal(key, plain)].concat()
    }

    fn id_of(number: usize) -> Vec<u8> {
        (number as u32).to_be_bytes().to_vec()
    }

    fn manifest_answer(version: u64, contacts: usize, storage: &[u8; 32], record_key: &[u8]) -> Vec<u8> {
        let identifiers = (0..contacts).fold(Writer::new(), |record, number| {
            record.message(2, Writer::new().bytes(1, &id_of(number)).varint(2, CONTACT))
        });
        let record = identifiers
            .message(2, Writer::new().bytes(1, ACCOUNT_ID).varint(2, ACCOUNT))
            .message(2, Writer::new().bytes(1, b"stories").varint(2, 5))
            .message(2, Writer::new().bytes(1, GROUP_ID).varint(2, GROUP))
            .bytes(4, record_key)
            .finish();
        let key = Standin::default().hmac_sha256(storage, &[format!("Manifest_{version}").as_bytes()]);
        Writer::new().varint(1, version).bytes(2, &seal(&key, &record)).finish()
    }

    fn item(answer: Writer, id: &[u8], record: &[u8]) -> Writer {
        let key: [u8; 32] = hkdf(&Standin::default(), None, RECORD_KEY, &[ITEM, id]).unwrap();
        answer.message(1, Writer::new().bytes(1, id).bytes(2, &seal(&key, record)))
    }

    fn records_answer(numbers: core::ops::Range<usize>, rest: bool) -> Vec<u8> {
        let answer = numbers.fold(Writer::new(), |answer, number| {
            let id = id_of(number);
            let mut aci = [0u8; 16];
            aci[..4].copy_from_slice(&id);
            item(answer, &id, &Writer::new().message(1, Writer::new().bytes(25, &aci).text(6, "Ann")).finish())
        });
        if !rest {
            return answer.finish();
        }
        let account = Writer::new().bytes(1, &[9; 32]).bool(6, true).varint(12, EVERYBODY);
        let answer = item(answer, ACCOUNT_ID, &Writer::new().message(4, account).finish());
        let climbing = Writer::new().message(3, Writer::new().bytes(1, &CLIMBING).bool(3, true)).finish();
        item(answer, GROUP_ID, &climbing).finish()
    }

    #[test]
    fn manifest_then_records() {
        use super::super::address::Peer;
        let crypto = Standin::default();
        let storage = storage_key(&crypto, &MASTER);
        let mut read = StorageRead::new(&crypto, &account(), String::from(AUTHORIZATION));
        let headers = vec![
            (String::from("Authorization"), String::from(AUTHORIZATION)),
            (String::from("Content-Type"), String::from("application/x-protobuf")),
        ];
        let manifest = String::from("https://storage.signal.org/v1/storage/manifest");
        assert_eq!(read.next_request(), Some(Request::new("GET", manifest, headers.clone(), Cow::Borrowed(&[]))));
        read.take_answer(&crypto, 200, &manifest_answer(9, READ_BATCH + 1, &storage, RECORD_KEY));
        let first = read.next_request().expect("the records");
        assert_eq!(
            (first.method, first.url.as_str(), &first.headers),
            ("PUT", "https://storage.signal.org/v1/storage/read", &headers)
        );
        assert_eq!(
            first.body,
            write_read_operation(&(0..READ_BATCH).map(id_of).collect::<Vec<_>>()),
            "so many at a time"
        );
        read.take_answer(&crypto, 200, &records_answer(0..READ_BATCH, false));
        let last = read.next_request().expect("the rest");
        assert_eq!(
            last.body,
            write_read_operation(&[id_of(READ_BATCH), ACCOUNT_ID.to_vec(), GROUP_ID.to_vec()]),
            "the contact left, the account's record and the group's, the story list's passed over"
        );
        read.take_answer(&crypto, 200, &records_answer(READ_BATCH..READ_BATCH + 1, true));
        assert_eq!(read.next_request(), None, "every record asked");
        let mut device = Device::unkeyed();
        device.take_dirty();
        assert_eq!(
            device.take_storage_read(&crypto, read, 77),
            "2501 names and 1 groups from the storage service, the account's settings"
        );
        assert_eq!(
            (device.contacts().len(), device.storage_version, device.storage_read_at, device.has_listed_groups),
            (READ_BATCH + 1, 9, 77, true),
            "the reading has listed the groups"
        );
        assert_eq!(device.name_of(&Peer::Contact(ServiceId::Aci([0; 16]))).as_deref(), Some("Ann"));
        let climbing = Peer::Group(super::super::group::id_of(&crypto, &CLIMBING));
        assert_eq!(device.groups_due(), [(climbing, CLIMBING, Some(0), false)], "the group is known, its state due");
        let account = &device.account;
        assert_eq!(
            (account.read_receipts, account.is_number_shared, account.profile_key),
            (true, true, [9; 32]),
            "the account's settings and its profile key"
        );
        assert!(device.is_accepted(&climbing, true), "the group the owner shares the profile with is accepted");
        assert_eq!(
            device.take_dirty(),
            [File::Account, File::Contacts, File::Conversation(climbing), File::Seen],
            "all kept"
        );
    }

    #[test]
    fn optional_settings() {
        let to_group =
            |fields: Writer| read_group(&Message::parse(&fields.bytes(1, &CLIMBING).finish()).unwrap()).unwrap();
        assert_eq!(to_group(Writer::new()).notify, (None, None, None), "not set: the account's");
        assert_eq!(
            to_group(Writer::new().varint(15, ENABLED).varint(16, DISABLED).varint(14, ENABLED)).notify,
            (Some(true), Some(false), Some(true)),
            "set on and set off, calls among them"
        );
        assert_eq!(
            to_group(Writer::new().bool(7, true).varint(15, ENABLED)).notify,
            (Some(false), None, None),
            "the deprecated field holds mentions off"
        );
        let to_account = |fields: Writer| read_account(&Message::parse(&fields.finish()).unwrap());
        let unset = to_account(Writer::new());
        assert_eq!(
            (unset.count_muted, unset.notify),
            (false, NOTIFY_IF_MUTED),
            "not set: counted off, mentions and replies told, calls not"
        );
        let off =
            to_account(Writer::new().varint(54, ENABLED).varint(57, DISABLED).varint(58, DISABLED).varint(56, ENABLED));
        assert_eq!((off.count_muted, off.notify), (true, (false, false, true)));
        let contact = |fields: Writer| {
            let aci = Writer::new().bytes(25, &[1; 16]);
            read_contact(&Message::parse(&[aci.finish(), fields.finish()].concat()).unwrap()).unwrap().notify_calls
        };
        assert_eq!(
            (
                contact(Writer::new()),
                contact(Writer::new().varint(28, ENABLED)),
                contact(Writer::new().varint(28, DISABLED))
            ),
            (None, Some(true), Some(false)),
            "a contact's calls while muted, where its record says"
        );
    }

    #[test]
    fn refused_read() {
        let crypto = Standin::default();
        let storage = storage_key(&crypto, &MASTER);
        let mut read = StorageRead::new(&crypto, &account(), String::from(AUTHORIZATION));
        read.next_request();
        read.take_answer(&crypto, 200, &manifest_answer(9, 1, &storage, RECORD_KEY));
        read.next_request();
        read.take_answer(&crypto, 500, b"");
        assert_eq!(read.next_request(), None);
        let mut device = Device::unkeyed();
        assert_eq!(
            device.take_storage_read(&crypto, read, 77),
            "0 names and 0 groups from the storage service, a read answered 500"
        );
        assert_eq!((device.contacts().len(), device.storage_version), (0, 9), "the manifest's version is kept");
        let mut read = StorageRead::new(&crypto, &account(), String::from(AUTHORIZATION));
        read.next_request();
        read.take_answer(&crypto, 200, &manifest_answer(9, 1, &storage, RECORD_KEY));
        read.next_request();
        let unsealed = Writer::new().message(1, Writer::new().bytes(1, b"stray")).finish();
        read.take_answer(&crypto, 200, &[records_answer(0..1, false), unsealed].concat());
        assert_eq!(
            device.take_storage_read(&crypto, read, 78),
            "1 names and 0 groups from the storage service, 1 records that do not open",
            "one that does not open beside one that does is told, and the reading counts"
        );
    }

    #[test]
    fn failed_reading() {
        let crypto = Standin::default();
        let storage = storage_key(&crypto, &MASTER);
        let read_line = |answer: &dyn Fn(&mut StorageRead)| {
            let mut read = StorageRead::new(&crypto, &account(), String::from(AUTHORIZATION));
            answer(&mut read);
            let mut device = Device::unkeyed();
            device.take_dirty();
            let line = device.take_storage_read(&crypto, read, 77);
            assert_eq!(
                (device.contacts().len(), device.storage_version, device.storage_read_at, device.has_listed_groups),
                (0, 0, 77, false),
                "nothing listed: the next stay reads again"
            );
            assert_eq!(device.take_dirty(), [File::Seen], "when it was read is kept");
            line
        };
        let refusal = read_line(&|read| {
            read.next_request();
            read.take_answer(&crypto, 401, b"");
            assert_eq!(read.next_request(), None, "the reading is over");
        });
        assert_eq!(refusal, "no names from the storage service: the manifest: 401");
        for (key, record_key) in [([0; 32], RECORD_KEY), (storage, &[][..])] {
            let line = read_line(&|read| {
                read.next_request();
                read.take_answer(&crypto, 200, &manifest_answer(9, 1, &key, record_key));
                assert_eq!(read.next_request(), None);
            });
            assert_eq!(
                line, "no names from the storage service: a manifest that does not open",
                "sealed under another key, or without the records' key"
            );
        }
        let failure = read_line(&|read| {
            read.next_request();
            read.take_answer(&crypto, 200, &manifest_answer(9, READ_BATCH + 1, &storage, RECORD_KEY));
            read.next_request();
            read.take_answer(&crypto, 200, &records_answer(0..READ_BATCH, false));
            read.next_request();
            read.abort(String::from("timed out"));
            assert_eq!(read.next_request(), None);
        });
        assert_eq!(failure, "no names from the storage service: timed out", "what was read before goes nowhere");
        let unopened = read_line(&|read| {
            read.next_request();
            read.take_answer(&crypto, 200, &manifest_answer(9, 1, &storage, b"other records"));
            read.next_request();
            read.take_answer(&crypto, 200, &records_answer(0..1, true));
        });
        assert_eq!(
            unopened, "no names from the storage service: 3 records that do not open",
            "records sealed under another key than the manifest's: none opened, so none is listed"
        );
        assert_eq!(read_line(&|_| {}), "no names from the storage service: no manifest");
    }
}
