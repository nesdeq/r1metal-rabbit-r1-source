// An attachment's transfer, step by step as Signal Desktop takes them (`uploadAttachment`, `tusUpload`, `putEncryptedAttachment`, `getAttachment`).

use super::seal::{DIGEST, IV, KEYS, Sealed, decrypt, encrypt, sealed_size};
use crate::base::code::base64;
use crate::base::json;
use crate::codec::http::{
    CLIENT_ERRORS, FORBIDDEN, GET, HEAD, Head, NOT_FOUND, PATCH, POST, PUT, REQUEST_TIMEOUT, SUCCESSFUL,
    TOO_MANY_REQUESTS, UNAUTHORIZED,
};
use crate::signal::Crypto;
use crate::signal::address::{Peer, ServiceId};
use crate::signal::service::{self, Request};
use crate::storage::paths::split_file;
use alloc::borrow::Cow;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// Signal's service gives the form an upload goes under; libsignal asks it over the socket (`get_upload_form`).
const FORM: &str = "/v4/attachments/form/upload";
// The CDN that takes uploads by TUS (`CDNS_SUPPORTING_TUS`), the version of TUS spoken and the kind of a request that carries bytes.
const TUS_CDN: u32 = 3;
const TUS_VERSION: &str = "1.0.0";
const OFFSET_OCTETS: &str = "application/offset+octet-stream";
// How often an upload by TUS that broke on the way is resumed, and how often a resumable upload is (Desktop's `DEFAULT_MAX_RETRIES`, `MAX_RETRIES` of `putEncryptedAttachment`).
const TUS_TRIES: u32 = 3;
const PUT_TRIES: u32 = 10;
// The status of a resumable upload not yet whole.
const INCOMPLETE: u16 = 308;
// The refusals of a request that may go another time (RFC 9110, 15.5): the authorization, which a stay finds out about, the client's own slowness and too many requests.
const AGAIN: [u16; 4] = [UNAUTHORIZED, FORBIDDEN, REQUEST_TIMEOUT, TOO_MANY_REQUESTS];

/// An attachment of an owner's message to go up: the message by its conversation and timestamp, the attachment's place among its own, the file that holds it, the file its plaintext is kept in once it went up, the keys it goes under, and the authorization its form is asked under.
#[derive(Clone, PartialEq, Eq)]
pub struct Upload {
    pub(in crate::signal) peer: Peer,
    pub(in crate::signal) timestamp: u64,
    pub(in crate::signal) index: usize,
    pub(in crate::signal) file: String,
    pub(in crate::signal) kept: String,
    pub(in crate::signal) keys: [u8; KEYS],
    pub(in crate::signal) authorization: String,
}

impl core::fmt::Debug for Upload {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Upload").field("timestamp", &self.timestamp).field("index", &self.index).finish_non_exhaustive()
    }
}

/// An attachment of a message to be fetched: the message by its conversation, timestamp and author, the attachment's place among its own, its CDN's number and where it lies there, the keys and digest it is checked and decrypted under, its size, and the file it is to be kept in.
#[derive(Clone, PartialEq, Eq)]
pub struct Download {
    pub(in crate::signal) peer: Peer,
    pub(in crate::signal) message: (u64, bool, Option<ServiceId>),
    pub(in crate::signal) index: usize,
    pub(in crate::signal) cdn: u32,
    pub(in crate::signal) address: String,
    pub(in crate::signal) keys: [u8; KEYS],
    pub(in crate::signal) digest: [u8; DIGEST],
    pub(in crate::signal) size: u32,
    pub(in crate::signal) file: String,
}

impl core::fmt::Debug for Download {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Download")
            .field("timestamp", &self.message.0)
            .field("index", &self.index)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// A transfer for the thread that carries attachments: one of an owner's message to go up, or one of a message to be fetched.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Job {
    Up(Upload),
    Down(Download),
}

impl Job {
    // The file it writes, from the data partition's root.
    pub(crate) fn writes(&self) -> &str {
        match self {
            Job::Up(upload) => &upload.kept,
            Job::Down(download) => &download.file,
        }
    }

    // The file an upload that ended so went up from and the one its message holds it in from then on, where they differ.
    pub(crate) fn moved<'a>(&'a self, finish: &'a Finish) -> Option<(&'a str, &'a str)> {
        match (self, finish) {
            (Job::Up(upload), Finish::Uploaded(uploaded)) if upload.file != uploaded.file => {
                Some((&upload.file, &uploaded.file))
            }
            _ => None,
        }
    }
}

/// An attachment that went up: to the CDN of that number under that key, encrypted to that digest from a plaintext of that size, which the R1 keeps in that file.
#[derive(Clone, PartialEq, Eq)]
pub struct Uploaded {
    pub(in crate::signal) cdn: u32,
    pub(in crate::signal) key: String,
    pub(in crate::signal) digest: [u8; DIGEST],
    pub(in crate::signal) size: u32,
    pub(in crate::signal) file: String,
}

impl core::fmt::Debug for Uploaded {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Uploaded").field("cdn", &self.cdn).field("size", &self.size).finish_non_exhaustive()
    }
}

/// How a transfer ended: it went up, or was fetched, checked and kept; it was refused, which ends its message's sending or has its fetching wait for the next stay; its CDN holds it no more, which ends its fetching for good; or the network or the storage broke on the way, and it is tried again later.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Finish {
    Uploaded(Uploaded),
    Fetched,
    Refused,
    Gone,
    Broke,
}

/// The log line of a transfer's end, which tells no name, key, place or size.
pub fn to_finished_line(job: &Job, finish: &Finish) -> String {
    let what = match job {
        Job::Up(_) => "an upload",
        Job::Down(_) => "a download",
    };
    let how = match finish {
        Finish::Uploaded(_) => "went up",
        Finish::Fetched => "was fetched and kept",
        Finish::Refused => "was refused",
        Finish::Gone => "was at its CDN no more",
        Finish::Broke => "broke",
    };
    format!("signal: {what} {how}")
}

/// What a transfer has the shell do next: read a file, by its directory and name; ask a request; keep bytes in a file; or nothing more.
pub enum Step<'a> {
    Read(&'a str, &'a str),
    Ask(Request<'a>),
    Keep(&'a str, &'a str, &'a [u8]),
    Finished(Finish),
}

/// What came of a step: what a file held, none where it could not be read; an answer's head and body; that none came; whether bytes are kept.
pub enum Took {
    Read(Option<Vec<u8>>),
    Answered(Head, Vec<u8>),
    Broke,
    Kept(bool),
}

// The upload form's answer (`GetUploadFormResponse`): the CDN by its number, the key it holds the upload under, the headers its requests carry, and where it goes.
#[derive(Clone, PartialEq, Eq)]
struct Form {
    cdn: u32,
    key: String,
    headers: Vec<(String, String)>,
    location: String,
}

fn form(document: &str) -> Option<Form> {
    let text = |name| json::text_at(document, &[name]).map(json::unescape);
    let headers = json::members(document, &["headers"]).filter_map(|(name, value)| {
        let value = value.strip_prefix('"')?.strip_suffix('"')?;
        Some((json::unescape(name), json::unescape(value)))
    });
    Some(Form {
        cdn: json::number_at(document, &["cdn"])?,
        key: text("key")?,
        headers: headers.collect(),
        location: text("signedUploadLocation")?,
    })
}

#[derive(Clone, PartialEq, Eq)]
enum Stage {
    Read,
    Form,
    Create,
    Offset,
    Resume(u64),
    Begin,
    Put(String, u64),
    Query(String),
    Fetch,
    Keep(Finish),
    End(Finish),
}

/// A transfer of an attachment, step by step.
pub struct Transfer {
    job: Job,
    stage: Stage,
    sealed: Sealed,
    plain: Vec<u8>,
    size: u32,
    form: Option<Form>,
    tries: u32,
}

impl Transfer {
    pub fn new(job: Job) -> Transfer {
        let stage = match job {
            Job::Up(_) => Stage::Read,
            Job::Down(_) => Stage::Fetch,
        };
        let sealed = Sealed { blob: Vec::new(), digest: [0; DIGEST] };
        Transfer { job, stage, sealed, plain: Vec::new(), size: 0, form: None, tries: 0 }
    }

    /// What the shell does next.
    pub fn next(&self) -> Step<'_> {
        // A header the request sets takes the place of the form's of its name, as Desktop lays its own over the form's (`createFetchForAttachmentUpload`): the form names `Upload-Metadata` too.
        let headers = |more: &[(&str, String)]| -> Vec<(String, String)> {
            let is_set = |name: &str| more.iter().any(|(own, _)| own.eq_ignore_ascii_case(name));
            let given =
                self.form.iter().flat_map(|form| form.headers.iter()).filter(|(name, _)| !is_set(name.as_str()));
            given.cloned().chain(more.iter().map(|(name, value)| (String::from(*name), value.clone()))).collect()
        };
        let blob = &self.sealed.blob[..];
        let length = blob.len().to_string();
        let ask =
            |method, url: String, headers, body| Step::Ask(Request::new(method, url, headers, Cow::Borrowed(body)));
        let at_key = |form: &Form| format!("{}/{}", form.location, form.key);
        let form = self.form.as_ref();
        let tus =
            |more: &[(&str, String)]| headers(&[&[("Tus-Resumable", String::from(TUS_VERSION))][..], more].concat());
        match (&self.job, &self.stage) {
            (Job::Up(upload), Stage::Read) => {
                let (directory, name) = split_file(&upload.file);
                Step::Read(directory, name)
            }
            (Job::Up(upload), Stage::Form) => {
                let url = service::url(&format!("{FORM}?uploadLength={length}"));
                ask(GET, url, alloc::vec![(String::from("Authorization"), upload.authorization.clone())], &[])
            }
            (Job::Up(_), Stage::Create) => {
                let metadata = format!("filename {}", base64(form.map_or("", |form| &form.key).as_bytes()));
                let created = tus(&[
                    ("Upload-Length", length),
                    ("Upload-Metadata", metadata),
                    ("Content-Type", String::from(OFFSET_OCTETS)),
                ]);
                ask(POST, form.map_or_else(String::new, |form| form.location.clone()), created, blob)
            }
            (Job::Up(_), Stage::Offset) => ask(HEAD, form.map_or_else(String::new, at_key), tus(&[]), &[]),
            (Job::Up(_), Stage::Resume(offset)) => {
                let resumed =
                    tus(&[("Upload-Offset", offset.to_string()), ("Content-Type", String::from(OFFSET_OCTETS))]);
                let from = usize::try_from(*offset).unwrap_or(blob.len()).min(blob.len());
                ask(PATCH, form.map_or_else(String::new, at_key), resumed, &blob[from..])
            }
            (Job::Up(_), Stage::Begin) => {
                ask(POST, form.map_or_else(String::new, |form| form.location.clone()), headers(&[]), &[])
            }
            (Job::Up(_), Stage::Put(location, start)) => {
                let range = alloc::vec![(String::from("Content-Range"), format!("bytes {start}-*/{length}"))];
                let from = usize::try_from(*start).unwrap_or(blob.len()).min(blob.len());
                ask(PUT, location.clone(), range, &blob[from..])
            }
            (Job::Up(_), Stage::Query(location)) => {
                let range = alloc::vec![(String::from("Content-Range"), format!("bytes */{length}"))];
                ask(PUT, location.clone(), range, &[])
            }
            (Job::Down(download), Stage::Fetch) => Step::Ask(Request {
                method: GET,
                url: download.address.clone(),
                headers: Vec::new(),
                body: Cow::Borrowed(&[]),
                most: sealed_size(u64::from(download.size)),
            }),
            (job, Stage::Keep(_)) => {
                let (directory, name) = split_file(job.writes());
                Step::Keep(directory, name, &self.plain)
            }
            (_, Stage::End(finish)) => Step::Finished(finish.clone()),
            (Job::Down(_), _) | (Job::Up(_), Stage::Fetch) => Step::Finished(Finish::Refused),
        }
    }

    // Refused where the request is at fault and cannot go another time, else broken.
    fn ended_by(status: u16) -> Finish {
        if CLIENT_ERRORS.contains(&status) && !AGAIN.contains(&status) { Finish::Refused } else { Finish::Broke }
    }

    // A CDN tells by a 404 that it holds an attachment no more, and the first CDN by a 403 too (Signal Desktop's `downloadAttachment`).
    fn is_gone(&self, status: u16) -> bool {
        let Job::Down(download) = &self.job else { return false };
        status == NOT_FOUND || (status == FORBIDDEN && download.cdn == service::CDNS[0].0)
    }

    // Every byte is at the CDN: the plaintext is kept in its message's own file where that is another than the one it went up from, else the upload is over.
    fn whole(&mut self) {
        let Job::Up(upload) = &self.job else { return };
        let Some(form) = &self.form else { return };
        let uploaded = Uploaded {
            cdn: form.cdn,
            key: form.key.clone(),
            digest: self.sealed.digest,
            size: self.size,
            file: upload.file.clone(),
        };
        self.stage = if upload.kept == upload.file {
            Stage::End(Finish::Uploaded(uploaded))
        } else {
            Stage::Keep(Finish::Uploaded(uploaded))
        };
    }

    /// Takes what came of the step the shell took.
    pub fn take(&mut self, crypto: &impl Crypto, took: Took) {
        match took {
            Took::Read(held) => self.take_file(crypto, held),
            Took::Answered(head, body) => self.take_answer(crypto, &head, &body),
            Took::Broke => self.take_break(),
            Took::Kept(is_kept) => self.take_kept(is_kept),
        }
    }

    // The file of an upload is encrypted under the job's keys and a random IV; one larger than Signal's apps send is refused.
    fn take_file(&mut self, crypto: &impl Crypto, held: Option<Vec<u8>>) {
        if self.stage != Stage::Read {
            return;
        }
        let Job::Up(upload) = &self.job else { return };
        let mut iv = [0u8; IV];
        let sealed = held.and_then(|plain| {
            let size = u32::try_from(plain.len()).ok().filter(|size| super::fits(u64::from(*size)))?;
            crypto.random(&mut iv).then_some(())?;
            Some((encrypt(crypto, &plain, &upload.keys, &iv)?, plain, size))
        });
        match sealed {
            Some((sealed, plain, size)) => {
                (self.sealed, self.size, self.stage) = (sealed, size, Stage::Form);
                if upload.kept != upload.file {
                    self.plain = plain;
                }
            }
            None => self.stage = Stage::End(Finish::Refused),
        }
    }

    fn is_asking(&self) -> bool {
        !matches!(self.stage, Stage::Read | Stage::Keep(_) | Stage::End(_))
    }

    // Success takes the transfer on and any other answer ends it, but a resumable upload's, which asks how far it came.
    fn take_answer(&mut self, crypto: &impl Crypto, head: &Head, body: &[u8]) {
        if !self.is_asking() {
            return;
        }
        let status = head.status;
        let is_success = SUCCESSFUL.contains(&status);
        let blob_length = self.sealed.blob.len() as u64;
        self.stage = match (&self.stage, is_success) {
            (Stage::Form, true) => match core::str::from_utf8(body).ok().and_then(form) {
                Some(form) => {
                    let stage = if form.cdn == TUS_CDN { Stage::Create } else { Stage::Begin };
                    self.form = Some(form);
                    stage
                }
                None => Stage::End(Finish::Refused),
            },
            (Stage::Create | Stage::Resume(_), true) => return self.whole(),
            (Stage::Offset, true) => match head.header("upload-offset").and_then(|offset| offset.parse::<u64>().ok()) {
                Some(offset) if offset == blob_length => return self.whole(),
                Some(offset) if offset < blob_length => Stage::Resume(offset),
                _ => Stage::End(Finish::Broke),
            },
            (Stage::Begin, true) => match head.header("location") {
                Some(location) => Stage::Put(String::from(location), 0),
                None => Stage::End(Finish::Broke),
            },
            (Stage::Put(..), true) => return self.whole(),
            (Stage::Put(location, _), false) if self.tries < PUT_TRIES => Stage::Query(location.clone()),
            (Stage::Query(location), _) if status == INCOMPLETE => {
                self.tries += 1;
                let received =
                    head.header("range").and_then(|range| range.strip_prefix("bytes=0-")?.parse::<u64>().ok());
                Stage::Put(location.clone(), received.map_or(0, |last| last + 1))
            }
            (Stage::Query(_), true) => return self.whole(),
            (Stage::Fetch, true) => {
                let Job::Down(download) = &self.job else { return };
                match decrypt(crypto, body, &download.keys, (&download.digest, download.size)) {
                    Some(plain) => {
                        self.plain = plain;
                        Stage::Keep(Finish::Fetched)
                    }
                    None => Stage::End(Finish::Refused),
                }
            }
            (Stage::Fetch, false) if self.is_gone(status) => Stage::End(Finish::Gone),
            (_, _) => Stage::End(Transfer::ended_by(status)),
        };
    }

    // No answer came: an upload by TUS or a resumable upload asks how far it came, as often as Desktop resumes one.
    fn take_break(&mut self) {
        if !self.is_asking() {
            return;
        }
        self.stage = match &self.stage {
            Stage::Create | Stage::Resume(_) if self.tries < TUS_TRIES => {
                self.tries += 1;
                Stage::Offset
            }
            Stage::Put(location, _) if self.tries < PUT_TRIES => Stage::Query(location.clone()),
            _ => Stage::End(Finish::Broke),
        };
    }

    // A plaintext not kept leaves an upload's in the file it went up from and has a download fetched again later.
    fn take_kept(&mut self, is_kept: bool) {
        let Stage::Keep(finish) = &self.stage else { return };
        self.stage = match (finish.clone(), &self.job) {
            (Finish::Uploaded(uploaded), Job::Up(upload)) if is_kept => {
                Stage::End(Finish::Uploaded(Uploaded { file: upload.kept.clone(), ..uploaded }))
            }
            (finish, _) if is_kept || matches!(finish, Finish::Uploaded(_)) => Stage::End(finish),
            _ => Stage::End(Finish::Broke),
        };
        self.plain = Vec::new();
    }
}

#[cfg(test)]
pub(crate) mod samples {
    use super::Uploaded;

    // An attachment that went up to the third CDN, its plaintext kept in `file`.
    pub(crate) fn uploaded(file: &str) -> Uploaded {
        Uploaded { cdn: 3, key: "k".into(), digest: [5; 32], size: 15, file: file.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ATTACHMENTS, kept_as, sent_as};
    use super::*;
    use crate::signal::tests::Standin;

    fn head(status: u16, lines: &str) -> Head {
        Head::parse(format!("HTTP/1.1 {status} X\r\n{lines}\r\n").as_bytes()).unwrap().unwrap().0
    }

    fn asked(transfer: &Transfer) -> (&'static str, String, Vec<(String, String)>, usize) {
        match transfer.next() {
            Step::Ask(request) => (request.method, request.url, request.headers, request.body.len()),
            _ => panic!("a request"),
        }
    }

    fn written(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(name, value)| (String::from(*name), String::from(*value))).collect()
    }

    fn upload() -> Upload {
        let file = String::from("camera/pictures/20261029_101500.jpg");
        Upload {
            peer: Peer::Contact(ServiceId::Aci([4; 16])),
            timestamp: 5,
            index: 0,
            kept: sent_as(&file, (5, 0), "image/jpeg"),
            file,
            keys: [3; KEYS],
            authorization: String::from("Basic YTpi"),
        }
    }

    #[test]
    fn upload_by_tus_resumed() {
        let crypto = Standin::default();
        let mut transfer = Transfer::new(Job::Up(upload()));
        assert!(matches!(transfer.next(), Step::Read("camera/pictures", "20261029_101500.jpg")));
        transfer.take_file(&crypto, Some(b"a photo's bytes".to_vec()));
        let length = sealed_size(15);
        assert_eq!(
            asked(&transfer),
            (
                "GET",
                format!("https://chat.signal.org/v4/attachments/form/upload?uploadLength={length}"),
                written(&[("Authorization", "Basic YTpi")]),
                0
            ),
            "the form for as many bytes as the CDN holds, under the account's authorization"
        );
        let form = br#"{"cdn":3,"key":"ab/c","headers":{"Authorization":"Bearer a\"b","upload-metadata":"filename YWIvYw=="},"signedUploadLocation":"https://cdn3.signal.org/upload"}"#;
        transfer.take_answer(&crypto, &head(200, ""), form);
        let tus =
            [("Authorization", "Bearer a\"b"), ("upload-metadata", "filename YWIvYw=="), ("Tus-Resumable", "1.0.0")];
        assert_eq!(
            asked(&transfer),
            (
                "POST",
                String::from("https://cdn3.signal.org/upload"),
                written(&[
                    tus[0],
                    tus[2],
                    ("Upload-Length", &length.to_string()),
                    ("Upload-Metadata", "filename YWIvYw=="),
                    ("Content-Type", "application/offset+octet-stream")
                ]),
                length as usize
            ),
            "created with its upload by TUS under the form's headers, its key the file's name, a header it sets itself sent once however the form writes its name"
        );
        transfer.take_break();
        assert_eq!(
            asked(&transfer),
            ("HEAD", String::from("https://cdn3.signal.org/upload/ab/c"), written(&tus), 0),
            "broken on the way, it asks how far it came"
        );
        transfer.take_answer(&crypto, &head(200, "Upload-Offset: 100\r\n"), b"");
        let resumed =
            [tus[0], tus[1], tus[2], ("Upload-Offset", "100"), ("Content-Type", "application/offset+octet-stream")];
        assert_eq!(
            asked(&transfer),
            ("PATCH", String::from("https://cdn3.signal.org/upload/ab/c"), written(&resumed), length as usize - 100),
            "and goes on from there"
        );
        transfer.take_answer(&crypto, &head(204, ""), b"");
        assert!(
            matches!(transfer.next(), Step::Keep(ATTACHMENTS, "5-0.jpg", b"a photo's bytes")),
            "whole at the CDN, its plaintext is kept beside the others"
        );
        transfer.take_kept(true);
        let Step::Finished(Finish::Uploaded(uploaded)) = transfer.next() else { panic!("up") };
        assert_eq!(
            (uploaded.cdn, uploaded.key.as_str(), uploaded.size, uploaded.file.as_str()),
            (3, "ab/c", 15, ".r1metal/signal/attachments/5-0.jpg")
        );
        assert_eq!(uploaded.digest, transfer.sealed.digest, "under the digest of what went up");
        let finish = Finish::Uploaded(uploaded);
        assert_eq!(
            transfer.job.moved(&finish),
            Some(("camera/pictures/20261029_101500.jpg", ".r1metal/signal/attachments/5-0.jpg")),
            "the photo moves from the roll's file to its message's own"
        );
        assert_eq!(transfer.job.moved(&Finish::Refused), None, "one refused moves nothing");
    }

    #[test]
    fn resumable_upload() {
        let crypto = Standin::default();
        let voice = Upload { file: format!("{ATTACHMENTS}/1700.m4a"), ..upload() };
        let voice = Upload { kept: sent_as(&voice.file, (5, 0), "audio/aac"), ..voice };
        assert_eq!(voice.kept, voice.file, "a voice note keeps its own file");
        let mut transfer = Transfer::new(Job::Up(voice));
        transfer.take_file(&crypto, Some(alloc::vec![7; 1_000]));
        let length = sealed_size(1_000);
        let form = br#"{"cdn":2,"key":"k","headers":{"x-goog-resumable":"start"},"signedUploadLocation":"https://cdn2.signal.org/u"}"#;
        transfer.take_answer(&crypto, &head(200, ""), form);
        assert_eq!(
            asked(&transfer),
            ("POST", String::from("https://cdn2.signal.org/u"), written(&[("x-goog-resumable", "start")]), 0)
        );
        transfer.take_answer(&crypto, &head(201, "Location: https://cdn2.signal.org/session\r\n"), b"");
        let range = |text: &str| written(&[("Content-Range", text)]);
        assert_eq!(
            asked(&transfer),
            ("PUT", String::from("https://cdn2.signal.org/session"), range(&format!("bytes 0-*/{length}")), 1_072)
        );
        transfer.take_answer(&crypto, &head(503, ""), b"");
        assert_eq!(
            asked(&transfer),
            ("PUT", String::from("https://cdn2.signal.org/session"), range(&format!("bytes */{length}")), 0),
            "a put that fails asks how far it came"
        );
        transfer.take_answer(&crypto, &head(308, "Range: bytes=0-99\r\n"), b"");
        assert_eq!(
            asked(&transfer),
            (
                "PUT",
                String::from("https://cdn2.signal.org/session"),
                range(&format!("bytes 100-*/{length}")),
                length as usize - 100
            ),
            "and puts the rest from the byte after the last that came"
        );
        transfer.take_answer(&crypto, &head(200, ""), b"");
        let Step::Finished(Finish::Uploaded(uploaded)) = transfer.next() else { panic!("up") };
        assert_eq!(
            (uploaded.cdn, uploaded.file.as_str()),
            (2, ".r1metal/signal/attachments/1700.m4a"),
            "nothing to keep: the file it went up from"
        );
        assert_eq!(transfer.job.moved(&Finish::Uploaded(uploaded)), None, "and nothing moves");
    }

    #[test]
    fn upload_ends() {
        let crypto = Standin::default();
        let ended = |answer: Option<u16>, read: Option<Vec<u8>>| {
            let mut transfer = Transfer::new(Job::Up(upload()));
            transfer.take_file(&crypto, read);
            match answer {
                Some(status) => transfer.take_answer(&crypto, &head(status, ""), b""),
                None => transfer.take_break(),
            }
            match transfer.next() {
                Step::Finished(finish) => finish,
                _ => panic!("over"),
            }
        };
        let bytes = || Some(b"x".to_vec());
        assert_eq!(ended(None, None), Finish::Refused, "a file that cannot be read");
        assert_eq!(
            ended(None, Some(alloc::vec![0; 102_224_513])),
            Finish::Refused,
            "a file a byte larger than Signal's apps send"
        );
        assert_eq!(ended(Some(413), bytes()), Finish::Refused, "too large for the service");
        assert_eq!(ended(Some(200), bytes()), Finish::Refused, "a form that is none");
        assert_eq!(
            [ended(Some(429), bytes()), ended(Some(401), bytes()), ended(Some(500), bytes()), ended(None, bytes())],
            [Finish::Broke, Finish::Broke, Finish::Broke, Finish::Broke],
            "too many requests, an authorization the stay finds out about, the service's error, the network"
        );
        let mut transfer = Transfer::new(Job::Up(upload()));
        transfer.take_file(&crypto, bytes());
        let form = br#"{"cdn":3,"key":"k","headers":{},"signedUploadLocation":"https://cdn3.signal.org/upload"}"#;
        transfer.take_answer(&crypto, &head(200, ""), form);
        for _ in 0..TUS_TRIES {
            transfer.take_break();
            transfer.take_answer(&crypto, &head(200, "Upload-Offset: 0\r\n"), b"");
        }
        transfer.take_break();
        assert!(matches!(transfer.next(), Step::Finished(Finish::Broke)), "resumed as often as Desktop resumes");
        let mut unkept = Transfer::new(Job::Up(upload()));
        unkept.take_file(&crypto, bytes());
        unkept.take_answer(&crypto, &head(200, ""), form);
        unkept.take_answer(&crypto, &head(201, ""), b"");
        unkept.take_kept(false);
        let Step::Finished(Finish::Uploaded(uploaded)) = unkept.next() else { panic!("up") };
        assert_eq!(uploaded.file, upload().file, "a plaintext not kept leaves it in the file it went up from");
    }

    #[test]
    fn download() {
        let crypto = Standin::default();
        let plain = b"a photo's bytes";
        let sealed = encrypt(&crypto, plain, &[3; KEYS], &[7; 16]).unwrap();
        let download = Download {
            peer: Peer::Contact(ServiceId::Aci([4; 16])),
            message: (9, false, None),
            index: 0,
            cdn: 3,
            address: String::from("https://cdn3.signal.org/attachments/k"),
            keys: [3; KEYS],
            digest: sealed.digest,
            size: plain.len() as u32,
            file: kept_as(&sealed.digest, "image/jpeg"),
        };
        let fetched_from = |cdn: u32, status: u16, body: &[u8], keep: bool| {
            let mut transfer = Transfer::new(Job::Down(Download { cdn, ..download.clone() }));
            let Step::Ask(request) = transfer.next() else { panic!("a request") };
            assert_eq!(
                (request.method, request.url.as_str(), request.headers.len(), request.most),
                ("GET", "https://cdn3.signal.org/attachments/k", 0, sealed_size(15))
            );
            transfer.take(&crypto, Took::Answered(head(status, ""), body.to_vec()));
            if let Step::Keep(directory, name, bytes) = transfer.next() {
                assert_eq!((format!("{directory}/{name}"), bytes), (download.file.clone(), &plain[..]));
                transfer.take(&crypto, Took::Kept(keep));
            }
            match transfer.next() {
                Step::Finished(finish) => finish,
                _ => panic!("over"),
            }
        };
        let fetched = |status: u16, body: &[u8], keep: bool| fetched_from(3, status, body, keep);
        assert_eq!(fetched(200, &sealed.blob, true), Finish::Fetched);
        assert_eq!(fetched(200, &sealed.blob, false), Finish::Broke, "not kept: fetched again later");
        assert_eq!(fetched(200, &sealed.blob[..sealed.blob.len() - 1], true), Finish::Refused, "not checked");
        assert_eq!([fetched(400, b"", true), fetched(500, b"", true)], [Finish::Refused, Finish::Broke]);
        assert_eq!(
            [fetched(404, b"", true), fetched_from(0, 403, b"", true), fetched(403, b"", true)],
            [Finish::Gone, Finish::Gone, Finish::Broke],
            "a 404 says its CDN holds it no more, and so does the first CDN's 403, as Desktop takes them"
        );
    }

    #[test]
    fn end_lines_tell_no_secret() {
        let up = Job::Up(upload());
        let uploaded = Uploaded { cdn: 3, key: "ab/c".into(), digest: [5; 32], size: 15, file: "x".into() };
        assert_eq!(to_finished_line(&up, &Finish::Uploaded(uploaded)), "signal: an upload went up");
        assert_eq!(to_finished_line(&up, &Finish::Broke), "signal: an upload broke");
        let down = Job::Down(Download {
            peer: Peer::Contact(ServiceId::Aci([4; 16])),
            message: (9, false, None),
            index: 0,
            cdn: 3,
            address: String::from("https://cdn3.signal.org/attachments/k"),
            keys: [3; KEYS],
            digest: [6; 32],
            size: 15,
            file: String::from("f"),
        });
        assert_eq!(to_finished_line(&down, &Finish::Fetched), "signal: a download was fetched and kept");
        assert_eq!(to_finished_line(&down, &Finish::Refused), "signal: a download was refused");
        assert_eq!(to_finished_line(&down, &Finish::Gone), "signal: a download was at its CDN no more");
    }

    #[test]
    fn debug_tells_no_secret() {
        assert_eq!(format!("{:?}", upload()), "Upload { timestamp: 5, index: 0, .. }");
    }
}
