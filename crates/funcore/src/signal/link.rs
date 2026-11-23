//! Linking the R1 to an account as Signal-Desktop does it.

use super::account::{Account, Identity, Listing, derive_master_key};
use super::address::{parse_uuid, uuid_to_text};
use super::curve::Pair;
use super::device::Device;
use super::keys::{self, Ring};
use super::message::{decode_key, encode_key};
use super::service::{self, Frame};
use super::{Crypto, hkdf};
use crate::base::bytes::same;
use crate::base::code::{base64, base64_unpadded, percent_encoded};
use crate::base::json;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::codec::http::{CONFLICT, LENGTH_REQUIRED, OK, PUT};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const PROVISIONING: &[u8] = b"TextSecure Provisioning Message";
const CODE_LIFETIME: u64 = 45_000;
const CODES: usize = 6;
const DEVICE_NAME: &str = "r1metal R1";
/// Why no code is shown.
pub const NO_KEY: &str = "no key for a link code";

/// The log's line on the answer to the link request.
pub fn to_answered_line(status: u16) -> String {
    format!("signal: the link request answered {status}")
}

/// The log's line on the device the link made.
pub fn to_linked_line(id: u32) -> String {
    format!("signal: linked as device {id}")
}

/// The log's line on the answer to an upload of one-time keys.
pub fn to_uploaded_line(status: u16) -> String {
    format!("signal: one-time keys uploaded: {status}")
}

/// The codes of a link, one after another while the phone takes none, timed by the paced clock in ms.
#[derive(Default)]
pub struct Codes {
    count: usize,
    end: u64,
}

impl Codes {
    pub(crate) const fn has_more(&self) -> bool {
        self.count < CODES
    }

    /// The next code begins at `now`, once its socket is open.
    pub const fn begin(&mut self, now: u64) {
        (self.count, self.end) = (self.count + 1, now + CODE_LIFETIME);
    }

    pub(crate) fn time_left(&self, now: u64) -> Option<u64> {
        (now < self.end).then(|| self.end - now)
    }
}

// The bytes `application/x-www-form-urlencoded` leaves as they are beside letters and digits.
const FORM_KEPT: &[u8] = b"*-._";

/// The QR code's text for a provisioning address and the R1's key.
pub fn address(provisioning: &str, public: &[u8; 32]) -> String {
    format!(
        "sgnl://linkdevice?uuid={}&pub_key={}&capabilities={}",
        percent_encoded(provisioning, FORM_KEPT),
        percent_encoded(&base64(&encode_key(public)), FORM_KEPT),
        percent_encoded("nopni,nopni2", FORM_KEPT)
    )
}

/// The account as the phone hands it over, and the code that verifies the link request.
#[derive(Clone, PartialEq, Eq)]
pub struct Provision {
    pub(super) account: Account,
    pub(super) code: String,
}

impl core::fmt::Debug for Provision {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Provisioned")
    }
}

fn read_uuid(message: &Message, binary: u32, text: u32) -> Option<[u8; 16]> {
    match message.bytes(binary).filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => bytes.try_into().ok(),
        None => parse_uuid(message.text(text)?),
    }
}

fn read_identity(crypto: &impl Crypto, message: &Message, number: u32) -> Option<Pair> {
    Pair::from_private(crypto, message.bytes(number)?.try_into().ok()?)
}

fn open_envelope(crypto: &impl Crypto, private: &[u8; 32], envelope: &[u8]) -> Option<Provision> {
    let envelope = Message::parse(envelope)?;
    let public = decode_key(envelope.bytes(1)?)?;
    let body = envelope.bytes(2)?;
    if body.len() < 1 + 16 + 16 + 32 || body[0] != 1 {
        return None;
    }
    let (signed_bytes, mac) = body.split_at(body.len() - 32);
    let shared = crypto.agree(private, &public)?;
    let keys: [u8; 64] = hkdf(crypto, Some(&[0; 32]), &shared, &[PROVISIONING])?;
    if !same(&crypto.hmac_sha256(&keys[32..], &[signed_bytes]), mac) {
        return None;
    }
    let iv: [u8; 16] = signed_bytes[1..17].try_into().ok()?;
    let plain = crypto.cbc_decrypt(keys[..32].try_into().ok()?, &iv, &signed_bytes[17..])?;
    let message = Message::parse(&plain)?;
    let pni = read_uuid(&message, 18, 10);
    let master_key = match message.bytes(13) {
        Some(key) => key.try_into().ok()?,
        None => derive_master_key(crypto, message.text(15)?)?,
    };
    let account = Account {
        aci: read_uuid(&message, 17, 8)?,
        pni,
        number: message.text(3).map(String::from),
        device: 0,
        password: String::new(),
        aci_identity: read_identity(crypto, &message, 2)?,
        pni_identity: match pni {
            Some(_) => Some(read_identity(crypto, &message, 12)?),
            None => None,
        },
        registration: 0,
        pni_registration: 0,
        profile_key: message.bytes(6)?.try_into().ok()?,
        master_key,
        read_receipts: message.bool(7).unwrap_or(false),
        credential_salt: message.bytes(19).map(<[u8]>::to_vec),
        is_number_shared: false,
        listing: Listing::default(),
    };
    Some(Provision { account, code: String::from(message.text(4)?) })
}

fn seal_name(crypto: &impl Crypto, name: &str, identity: &[u8; 32]) -> Option<String> {
    let ephemeral = Pair::new(crypto)?;
    let master = crypto.agree(&ephemeral.private, identity)?;
    let plain = name.as_bytes();
    let synthetic: [u8; 16] =
        crypto.hmac_sha256(&crypto.hmac_sha256(&master, &[b"auth"]), &[plain])[..16].try_into().ok()?;
    let cipher_key = crypto.hmac_sha256(&crypto.hmac_sha256(&master, &[b"cipher"]), &[&synthetic]);
    let ciphertext = crypto.ctr(&cipher_key, &[0; 16], plain)?;
    Some(base64(
        &Writer::new().bytes(1, &encode_key(&ephemeral.public)).bytes(2, &synthetic).bytes(3, &ciphertext).finish(),
    ))
}

fn write_request_body(code: &str, name: &str, (aci, aci_ring): (u32, &Ring), pni: Option<(u32, &Ring)>) -> String {
    let pni_registration = pni.map(|(pni, _)| format!(r#","pniRegistrationId":{pni}"#)).unwrap_or_default();
    let capabilities = service::write_capabilities(pni.is_some());
    let mut body = format!(
        r#"{{"verificationCode":"{}","accountAttributes":{{"fetchesMessages":true,"name":"{}","registrationId":{aci}{pni_registration},"capabilities":{capabilities}}},"aciSignedPreKey":{},"aciPqLastResortPreKey":{}"#,
        json::escape(code),
        name,
        aci_ring.signed_keys[0].to_json(),
        aci_ring.last_resort[0].to_json()
    );
    if let Some((_, ring)) = pni {
        body.push_str(&format!(
            r#","pniSignedPreKey":{},"pniPqLastResortPreKey":{}"#,
            ring.signed_keys[0].to_json(),
            ring.last_resort[0].to_json()
        ));
    }
    body.push('}');
    body
}

fn parse_device_id(document: &str, aci: &[u8; 16]) -> Option<u32> {
    let uuid = parse_uuid(json::text_at(document, &["uuid"])?)?;
    (uuid == *aci).then(|| json::number_at(document, &["deviceId"]).unwrap_or(1))
}

fn generate_password(crypto: &impl Crypto) -> Option<String> {
    let mut bytes = [0u8; 16];
    crypto.random(&mut bytes).then_some(())?;
    Some(base64_unpadded(&bytes))
}

fn generate_registration(crypto: &impl Crypto) -> Option<u32> {
    loop {
        let mut bytes = [0u8; 2];
        crypto.random(&mut bytes).then_some(())?;
        let id = u32::from(u16::from_le_bytes(bytes) & 0x3FFF);
        if id != 0 {
            return Some(id);
        }
    }
}

/// What a frame of the provisioning socket brought.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ProvisioningMessage {
    Address(String),
    Account(Box<Provision>),
    // Anything else, or an envelope that does not open with the code's key.
    Nothing,
}

/// What a request on the provisioning socket brings, and the answer it gets.
pub fn read_frame(crypto: &impl Crypto, private: &[u8; 32], frame: &Frame) -> (ProvisioningMessage, Option<Vec<u8>>) {
    let Frame::Request { id, verb, path, body, .. } = frame else { return (ProvisioningMessage::Nothing, None) };
    if verb != PUT {
        return (ProvisioningMessage::Nothing, None);
    }
    let message = match path.as_str() {
        service::ADDRESS => Message::parse(body)
            .and_then(|address| address.text(1).map(String::from))
            .map_or(ProvisioningMessage::Nothing, ProvisioningMessage::Address),
        service::PROVISIONING_MESSAGE => open_envelope(crypto, private, body)
            .map_or(ProvisioningMessage::Nothing, |provision| ProvisioningMessage::Account(Box::new(provision))),
        _ => ProvisioningMessage::Nothing,
    };
    let answer = (!matches!(message, ProvisioningMessage::Nothing)).then(|| service::write_ok(*id));
    (message, answer)
}

/// The link request as a frame of the unauthenticated socket.
pub fn request_frame(provision: &Provision, request: &LinkRequest) -> Vec<u8> {
    let authorization = service::basic_auth(&uuid_to_text(&provision.account.aci), &request.password);
    let headers = [("Authorization", authorization.as_str()), ("Content-Type", "application/json; charset=utf-8")];
    service::write_request(0, PUT, service::LINK, &headers, request.body.as_bytes())
}

/// Why the service would not link the R1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    TooOld,
    TooManyDevices,
    Other,
}

/// The status and the device's id or refusal where the frame answers the link request; none for any other frame.
pub fn parse_answer(frame: &Frame, provision: &Provision) -> Option<(u16, Result<u32, Refusal>)> {
    let Frame::Response { id: 0, status, body, .. } = frame else { return None };
    let answer = match *status {
        OK => parse_device_id(core::str::from_utf8(body).unwrap_or_default(), &provision.account.aci)
            .ok_or(Refusal::Other),
        CONFLICT => Err(Refusal::TooOld),
        LENGTH_REQUIRED => Err(Refusal::TooManyDevices),
        _ => Err(Refusal::Other),
    };
    Some((*status, answer))
}

/// The status where the frame answers one of the `count` first uploads; none for any other frame.
pub fn parse_upload_status(frame: &Frame, count: usize) -> Option<u16> {
    let Frame::Response { id, status, .. } = frame else { return None };
    usize::try_from(*id).is_ok_and(|id| id < count).then_some(*status)
}

/// The R1 as the device the link made, the request's keys confirmed as Signal-Desktop confirms them once linked.
pub fn make_device(provision: &Provision, request: LinkRequest, id: u32) -> Device {
    let account = Account {
        device: id,
        password: request.password,
        registration: request.registration,
        pni_registration: request.pni_registration,
        ..provision.account.clone()
    };
    let mut rings = request.rings;
    for ring in rings.values_mut() {
        let created = ring.signed_keys.first().map_or(0, |key| key.created);
        ring.confirm((ring.signed_keys.first().map(|key| key.id), ring.last_resort.first().map(|key| key.id)), created);
    }
    Device::new(account, rings)
}

/// The first uploads of each identity's one-time keys, as frames of the account's socket.
pub fn first_uploads(device: &Device) -> Vec<Vec<u8>> {
    device
        .rings()
        .iter()
        .enumerate()
        .map(|(id, (identity, ring))| {
            let body = keys::write_upload(&ring.one_time, &ring.kyber, None, None);
            let path = service::keys_path(identity.name());
            service::write_request(id as u64, PUT, &path, &[service::JSON], body.as_bytes())
        })
        .collect()
}

/// What a link makes before it asks: the password, the registration ids, each identity's keys and the body.
pub struct LinkRequest {
    password: String,
    registration: u32,
    pni_registration: u32,
    rings: BTreeMap<Identity, Ring>,
    body: String,
}

/// Makes what linking the account the phone handed over needs.
pub fn prepare(crypto: &impl Crypto, provision: &Provision, now: u64) -> Option<LinkRequest> {
    let password = generate_password(crypto)?;
    let (aci_id, pni_id) = (generate_registration(crypto)?, generate_registration(crypto)?);
    let mut rings = BTreeMap::new();
    let account = &provision.account;
    rings.insert(Identity::Aci, Ring::generate(crypto, &account.aci_identity, now)?);
    if let Some(pni) = &account.pni_identity {
        rings.insert(Identity::Pni, Ring::generate(crypto, pni, now)?);
    }
    let name = seal_name(crypto, DEVICE_NAME, &account.aci_identity.public)?;
    let pni = rings.get(&Identity::Pni).map(|ring| (pni_id, ring));
    let body = write_request_body(&provision.code, &name, (aci_id, &rings[&Identity::Aci]), pni);
    Some(LinkRequest { password, registration: aci_id, pni_registration: pni_id, rings, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_form_encoding() {
        let address = address("WEJXAlXeJ5RmpGchGXDt5w==", &[0xFB; 32]);
        assert_eq!(
            address,
            "sgnl://linkdevice?uuid=WEJXAlXeJ5RmpGchGXDt5w%3D%3D&pub_key=Bfv7%2B%2Fv7%2B%2Fv7%2B%2Fv7%2B%2Fv7%2B%2Fv7%2B%2Fv7%2B%2Fv7%2B%2Fv7%2B%2Fv7%2B%2Fv7&capabilities=nopni%2Cnopni2"
        );
    }

    #[test]
    fn code_sequence() {
        let mut codes = Codes::default();
        assert_eq!(codes.time_left(0), None, "before the first, none stands");
        for code in 0..CODES as u64 {
            let now = code * CODE_LIFETIME;
            assert!(codes.has_more(), "code {code}");
            codes.begin(now);
            assert_eq!(
                (codes.time_left(now), codes.time_left(now + CODE_LIFETIME - 1)),
                (Some(CODE_LIFETIME), Some(1))
            );
            assert_eq!(codes.time_left(now + CODE_LIFETIME), None, "expired at its end");
        }
        assert!(!codes.has_more(), "every code shown: the link has expired");
    }

    #[test]
    fn lines_of_a_link() {
        assert_eq!(to_answered_line(200), "signal: the link request answered 200");
        assert_eq!(to_linked_line(3), "signal: linked as device 3");
        assert_eq!(to_uploaded_line(204), "signal: one-time keys uploaded: 204");
    }

    #[test]
    fn device_id_answer() {
        let aci = [0x9D; 16];
        let text = uuid_to_text(&aci);
        assert_eq!(parse_device_id(&format!(r#"{{"uuid":"{text}","pni":null,"deviceId":3}}"#), &aci), Some(3));
        assert_eq!(parse_device_id(&format!(r#"{{"uuid":"{text}"}}"#), &aci), Some(1));
        assert_eq!(parse_device_id(r#"{"uuid":"00000000-0000-0000-0000-000000000000","deviceId":3}"#, &aci), None);
    }

    #[test]
    fn link_answer() {
        let aci = [0x9D; 16];
        let provision = Provision { account: Account { aci, ..Device::unkeyed().account }, code: String::new() };
        let response = |id, status, body: &str| Frame::Response { id, status, body: body.into() };
        let document = format!(r#"{{"uuid":"{}","deviceId":3}}"#, uuid_to_text(&aci));
        assert_eq!(parse_answer(&response(0, 200, &document), &provision), Some((200, Ok(3))));
        assert_eq!(
            parse_answer(&response(0, 200, "{}"), &provision),
            Some((200, Err(Refusal::Other))),
            "another account's"
        );
        assert_eq!(parse_answer(&response(0, 409, ""), &provision), Some((409, Err(Refusal::TooOld))));
        assert_eq!(parse_answer(&response(0, 411, ""), &provision), Some((411, Err(Refusal::TooManyDevices))));
        assert_eq!(parse_answer(&response(0, 403, ""), &provision), Some((403, Err(Refusal::Other))));
        assert_eq!(parse_answer(&response(1, 200, &document), &provision), None, "the answer to another request");
        let request =
            Frame::Request { id: 0, verb: "PUT".into(), path: "/v1/x".into(), body: Vec::new(), delivered: None };
        assert_eq!(parse_answer(&request, &provision), None, "a request of the service's");
        assert_eq!(
            [0, 1, 2].map(|id| parse_upload_status(&response(id, 204, ""), 2)),
            [Some(204), Some(204), None],
            "the answers to the first uploads, by their numbers"
        );
        assert_eq!(parse_upload_status(&request, 2), None, "a delivery that comes meanwhile is none");
    }

    #[test]
    fn made_device() {
        use super::super::device::File;
        let phone_account =
            Account { pni: Some([4; 16]), master_key: [6; 32], read_receipts: true, ..Device::unkeyed().account };
        let provision =
            Provision { account: Account { device: 0, password: String::new(), ..phone_account }, code: "code".into() };
        let mut rings = BTreeMap::new();
        rings.insert(Identity::Aci, Ring::default());
        let request =
            LinkRequest { password: "secret".into(), registration: 7, pni_registration: 8, rings, body: String::new() };
        let mut device = make_device(&provision, request, 3);
        let account = &device.account;
        assert_eq!(
            (account.aci, account.pni, account.device, account.password.as_str()),
            ([3; 16], Some([4; 16]), 3, "secret")
        );
        assert_eq!(
            (account.registration, account.pni_registration, account.master_key, account.read_receipts),
            (7, 8, [6; 32], true)
        );
        assert_eq!(
            device.take_dirty(),
            [File::Account, File::Ring(Identity::Aci), File::Identities],
            "to be written, the account's own identity keys held as verified"
        );
    }
}
