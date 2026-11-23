//! An age file of one scrypt recipient (age-encryption.org/v1), which a backup is: its header, which wraps the file's key under a passphrase, and its payload in sealed chunks, written and read one chunk at a time.

use crate::Result;
use aws_lc_rs::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::{hkdf, hmac, rand};
use aws_lc_sys as lc;
use r1_funcore::base::code;
use r1_funcore::storage::safe::Work;
use std::io::{Read, Write};

const VERSION_LINE: &str = "age-encryption.org/v1";
const STANZA: &str = "-> scrypt ";
const END: &str = "---";
// What stands before a file's salt in the salt scrypt takes.
const SALT_LABEL: &[u8] = b"age-encryption.org/v1/scrypt";
const HEADER_INFO: &[u8] = b"header";
const PAYLOAD_INFO: &[u8] = b"payload";
const FILE_KEY_BYTES: usize = 16;
const SALT_BYTES: usize = 16;
const PAYLOAD_NONCE_BYTES: usize = 16;
const KEY_BYTES: usize = 32;
const MAC_BYTES: usize = 32;
const TAG_BYTES: usize = 16;
// The plain bytes of a chunk of the payload, all but the last one's.
const CHUNK: usize = 64 << 10;
// The longest line of a header: a stanza's body is wrapped at 64 columns, and a header of one scrypt stanza has no longer line.
const COLUMNS: usize = 64;
// scrypt's block size and parallelism, which the format fixes.
const BLOCK: u64 = 8;
const LANES: u64 = 1;
// The work a new file's passphrase is stretched with, the logarithm of scrypt's N to base two, as the format's own example has it.
const WORK: u8 = 18;
// The most work a file may ask of who opens it: age's test vectors hold a file that asks for 23 to be refused.
const MOST_WORK: u8 = 22;
const NO_BACKUP: &str = "no backup of r1's: an age file of one scrypt recipient is one";
const DAMAGED: &str = "the backup is damaged or cut short";

// What a file tells whose passphrase is another than the one given.
#[derive(Debug)]
pub(super) struct WrongPassphrase;

impl std::fmt::Display for WrongPassphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the passphrase is not the backup's")
    }
}

impl std::error::Error for WrongPassphrase {}

// The bytes of base64 without padding, as the format writes it; none unless it is canonical, as the format asks.
fn from_base64(text: &str) -> Option<Vec<u8>> {
    code::unbase64_unpadded(text).filter(|bytes| !text.contains('=') && code::base64_unpadded(bytes) == text)
}

// The key a passphrase and a file's salt give, by scrypt.
fn stretch(passphrase: &str, salt: &[u8; SALT_BYTES], work: u8) -> Result<[u8; KEY_BYTES]> {
    let work = Work { log_blocks: work.into(), block: BLOCK, lanes: LANES };
    let salt = [SALT_LABEL, salt].concat();
    let mut key = [0u8; KEY_BYTES];
    let done = unsafe {
        lc::EVP_PBE_scrypt(
            passphrase.as_ptr().cast(),
            passphrase.len(),
            salt.as_ptr(),
            salt.len(),
            work.blocks(),
            work.block,
            work.lanes,
            work.limit_bytes(),
            key.as_mut_ptr(),
            key.len(),
        )
    };
    if done != 1 {
        return Err("scrypt could not stretch the passphrase".into());
    }
    Ok(key)
}

// A key of the file's, by HKDF-SHA-256 from its file key.
fn derive(file_key: &[u8], salt: &[u8], info: &[u8]) -> Result<[u8; KEY_BYTES]> {
    let mut key = [0u8; KEY_BYTES];
    hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(file_key).expand(&[info], hkdf::HKDF_SHA256)?.fill(&mut key)?;
    Ok(key)
}

fn sealing(key: &[u8; KEY_BYTES]) -> Result<LessSafeKey> {
    Ok(LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key)?))
}

fn seal(key: &LessSafeKey, nonce: [u8; NONCE_LEN], bytes: &mut Vec<u8>) -> Result<()> {
    Ok(key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), bytes)?)
}

// What a sealed text holds; none where its tag does not hold under the key and the nonce.
fn open(key: &LessSafeKey, nonce: [u8; NONCE_LEN], sealed: &mut Vec<u8>) -> Option<()> {
    let plain = key.open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), sealed).ok()?.len();
    sealed.truncate(plain);
    Some(())
}

// The nonce of a chunk of the payload: its number in eleven bytes, the highest first, and whether it is the last.
fn chunk_nonce(number: u64, is_last: bool) -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[NONCE_LEN - 1 - size_of::<u64>()..NONCE_LEN - 1].copy_from_slice(&number.to_be_bytes());
    nonce[NONCE_LEN - 1] = u8::from(is_last);
    nonce
}

// The header up to and with its end mark, which its MAC is taken over.
fn head(salt: &[u8; SALT_BYTES], work: u8, wrapped: &[u8]) -> String {
    format!("{VERSION_LINE}\n{STANZA}{} {work}\n{}\n{END}", code::base64_unpadded(salt), code::base64_unpadded(wrapped))
}

fn header_key(file_key: &[u8]) -> Result<hmac::Key> {
    Ok(hmac::Key::new(hmac::HMAC_SHA256, &derive(file_key, &[], HEADER_INFO)?))
}

// What a new file is made with beside its passphrase: its key, its salt and its payload's nonce, each drawn anew for every file.
struct Draws {
    file_key: [u8; FILE_KEY_BYTES],
    salt: [u8; SALT_BYTES],
    nonce: [u8; PAYLOAD_NONCE_BYTES],
}

impl Draws {
    fn random() -> Result<Draws> {
        let mut draws = Draws { file_key: [0; FILE_KEY_BYTES], salt: [0; SALT_BYTES], nonce: [0; PAYLOAD_NONCE_BYTES] };
        rand::fill(&mut draws.file_key)?;
        rand::fill(&mut draws.salt)?;
        rand::fill(&mut draws.nonce)?;
        Ok(draws)
    }
}

/// An age file as it is written: what is handed to it goes out in sealed chunks, the last at its finish.
pub struct Writer<W: Write> {
    out: W,
    key: LessSafeKey,
    number: u64,
    chunk: Vec<u8>,
}

impl<W: Write> Writer<W> {
    /// Begins a file under a passphrase, its header written.
    pub fn new(out: W, passphrase: &str) -> Result<Writer<W>> {
        Writer::with(out, passphrase, WORK, &Draws::random()?)
    }

    fn with(mut out: W, passphrase: &str, work: u8, draws: &Draws) -> Result<Writer<W>> {
        let mut wrapped = draws.file_key.to_vec();
        seal(&sealing(&stretch(passphrase, &draws.salt, work)?)?, [0; NONCE_LEN], &mut wrapped)?;
        let head = head(&draws.salt, work, &wrapped);
        let mac = hmac::sign(&header_key(&draws.file_key)?, head.as_bytes());
        out.write_all(format!("{head} {}\n", code::base64_unpadded(mac.as_ref())).as_bytes())?;
        out.write_all(&draws.nonce)?;
        let key = sealing(&derive(&draws.file_key, &draws.nonce, PAYLOAD_INFO)?)?;
        Ok(Writer { out, key, number: 0, chunk: Vec::with_capacity(CHUNK + TAG_BYTES) })
    }

    fn put_chunk(&mut self, is_last: bool) -> Result<()> {
        seal(&self.key, chunk_nonce(self.number, is_last), &mut self.chunk)?;
        self.out.write_all(&self.chunk)?;
        self.chunk.clear();
        self.number += 1;
        Ok(())
    }

    /// Takes more of the payload; a full chunk goes out once more follows it, so that the last is never empty.
    pub fn write(&mut self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            if self.chunk.len() == CHUNK {
                self.put_chunk(false)?;
            }
            let (taken, rest) = bytes.split_at(bytes.len().min(CHUNK - self.chunk.len()));
            self.chunk.extend_from_slice(taken);
            bytes = rest;
        }
        Ok(())
    }

    // Ends the file with its last chunk and hands back what it was written to.
    pub(super) fn finish(mut self) -> Result<W> {
        self.put_chunk(true)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

// A line of a header, without its end; none where the file ends or the line runs past the longest a header has.
fn line(input: &mut impl Read) -> Option<String> {
    let mut text = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        input.read_exact(&mut byte).ok()?;
        if byte[0] == b'\n' {
            return String::from_utf8(text).ok();
        }
        text.push(byte[0]);
        if text.len() > COLUMNS {
            return None;
        }
    }
}

// A header of one scrypt stanza: its salt, its work, the wrapped file key and its MAC.
struct Header {
    salt: [u8; SALT_BYTES],
    work: u8,
    wrapped: Vec<u8>,
    mac: Vec<u8>,
}

impl Header {
    // The header a file begins with; none for one of another kind, or one that asks for more work than is given.
    fn read(input: &mut impl Read) -> Option<Header> {
        let (version, stanza, body, end) = (line(input)?, line(input)?, line(input)?, line(input)?);
        let (salt, work) = stanza.strip_prefix(STANZA)?.split_once(' ')?;
        let is_decimal = !work.starts_with('0') && work.bytes().all(|digit| digit.is_ascii_digit());
        let work = work.parse::<u8>().ok().filter(|work| is_decimal && *work <= MOST_WORK)?;
        let salt = from_base64(salt)?.try_into().ok()?;
        let wrapped = from_base64(&body).filter(|wrapped| wrapped.len() == FILE_KEY_BYTES + TAG_BYTES)?;
        let mac = from_base64(end.strip_prefix(END)?.strip_prefix(' ')?).filter(|mac| mac.len() == MAC_BYTES)?;
        (version == VERSION_LINE).then_some(Header { salt, work, wrapped, mac })
    }
}

// As much of `input` as fills `buffer`, or all that is left of it.
fn read_up_to(input: &mut impl Read, buffer: &mut Vec<u8>, length: usize) -> Result<()> {
    buffer.clear();
    input.take(length as u64).read_to_end(buffer)?;
    Ok(())
}

// An age file as it is read: its payload by chunks, each held to its place in the file and the last to be the last.
pub(super) struct Reader<R: Read> {
    input: R,
    key: LessSafeKey,
    number: u64,
    // The sealed chunk read ahead: whether another follows tells whether the one before it is the last.
    ahead: Vec<u8>,
    is_ended: bool,
}

impl<R: Read> Reader<R> {
    /// Opens a file under its passphrase; `WrongPassphrase` where it is another's.
    pub fn open(mut input: R, passphrase: &str) -> Result<Reader<R>> {
        let Header { salt, work, wrapped, mac } = Header::read(&mut input).ok_or(NO_BACKUP)?;
        let mut file_key = wrapped.clone();
        open(&sealing(&stretch(passphrase, &salt, work)?)?, [0; NONCE_LEN], &mut file_key).ok_or(WrongPassphrase)?;
        hmac::verify(&header_key(&file_key)?, head(&salt, work, &wrapped).as_bytes(), &mac).map_err(|_| DAMAGED)?;
        let mut nonce = [0u8; PAYLOAD_NONCE_BYTES];
        input.read_exact(&mut nonce).map_err(|_| DAMAGED)?;
        let key = sealing(&derive(&file_key, &nonce, PAYLOAD_INFO)?)?;
        let mut reader = Reader { input, key, number: 0, ahead: Vec::new(), is_ended: false };
        read_up_to(&mut reader.input, &mut reader.ahead, CHUNK + TAG_BYTES)?;
        Ok(reader)
    }

    // The next chunk of the payload; none once the last was handed out.
    pub(super) fn chunk(&mut self) -> Result<Option<Vec<u8>>> {
        if self.is_ended {
            return Ok(None);
        }
        let mut chunk = Vec::with_capacity(CHUNK + TAG_BYTES);
        std::mem::swap(&mut chunk, &mut self.ahead);
        read_up_to(&mut self.input, &mut self.ahead, CHUNK + TAG_BYTES)?;
        self.is_ended = self.ahead.is_empty();
        open(&self.key, chunk_nonce(self.number, self.is_ended), &mut chunk).ok_or(DAMAGED)?;
        if chunk.is_empty() && self.number > 0 {
            return Err(DAMAGED.into());
        }
        self.number += 1;
        Ok(Some(chunk))
    }
}

#[cfg(test)]
pub(super) mod samples {
    use super::*;

    pub(super) const PASSPHRASE: &str = "opening";
    // The work the tests stretch a passphrase with: that of age's own test file.
    pub(super) const WORK: u8 = 10;

    /// A file begun in memory under `PASSPHRASE`, with draws that are always the same.
    pub fn writer() -> Writer<Vec<u8>> {
        let draws = Draws { file_key: [7; FILE_KEY_BYTES], salt: [8; SALT_BYTES], nonce: [9; PAYLOAD_NONCE_BYTES] };
        Writer::with(Vec::new(), PASSPHRASE, WORK, &draws).unwrap()
    }

    pub fn reader(file: &[u8]) -> Result<Reader<&[u8]>> {
        Reader::open(file, PASSPHRASE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::digest;

    // The file `scrypt` of age's test vectors (C2SP's CCTV): its header, and its payload in hex; its passphrase, its file key, and the SHA-256 of what it holds.
    const VECTOR_HEADER: &str = "age-encryption.org/v1\n-> scrypt rF0/NwblUHHTpgQgRpe5CQ 10\ngUjEymFKMVXQEKdMMHL24oYexjE3TIC0O0zGSqJ2aUY\n--- IOXiQYStkoT1mvZW2tFOqZdhRVvj58egABx/sWfZQbc\n";
    const VECTOR_PAYLOAD: &str = "1b35c6e687dd00da3ac379ac9f742c21fd185a1b9e3ded739d14ac6a9a50124db866d8";
    const VECTOR_PASSPHRASE: &str = "password";
    const VECTOR_FILE_KEY: &[u8; FILE_KEY_BYTES] = b"YELLOW SUBMARINE";
    const VECTOR_SUM: &str = "013f54400c82da08037759ada907a8b864e97de81c088a182062c4b5622fd2ab";

    fn vector() -> Vec<u8> {
        [VECTOR_HEADER.as_bytes(), &code::unhex(VECTOR_PAYLOAD).unwrap()].concat()
    }

    fn payload(file: &[u8], passphrase: &str) -> Result<Vec<u8>> {
        let mut reader = Reader::open(file, passphrase)?;
        let mut payload = Vec::new();
        while let Some(chunk) = reader.chunk()? {
            payload.extend(chunk);
        }
        Ok(payload)
    }

    fn written(plain: &[u8], pieces: usize) -> Vec<u8> {
        let mut writer = samples::writer();
        plain.chunks(pieces).for_each(|piece| writer.write(piece).unwrap());
        writer.finish().unwrap()
    }

    #[test]
    fn age_vector_round_trip() {
        let plain = payload(&vector(), VECTOR_PASSPHRASE).unwrap();
        assert_eq!(code::hex(digest::digest(&digest::SHA256, &plain).as_ref()), VECTOR_SUM, "what the vector holds");
        let salt = from_base64("rF0/NwblUHHTpgQgRpe5CQ").unwrap().try_into().unwrap();
        let nonce = code::unhex(&VECTOR_PAYLOAD[..2 * PAYLOAD_NONCE_BYTES]).unwrap().try_into().unwrap();
        let draws = Draws { file_key: *VECTOR_FILE_KEY, salt, nonce };
        let mut writer = Writer::with(Vec::new(), VECTOR_PASSPHRASE, samples::WORK, &draws).unwrap();
        writer.write(&plain).unwrap();
        assert!(writer.finish().unwrap() == vector(), "with the vector's draws, the vector's bytes");
    }

    #[test]
    fn wrong_passphrase_and_foreign_files() {
        let wrong = payload(&vector(), "passwork").unwrap_err();
        assert!(wrong.is::<WrongPassphrase>(), "another passphrase: {wrong}");
        // The vector `scrypt_bad_tag`: the tag of the wrapped file key does not hold.
        let bad_tag = VECTOR_HEADER.replace("O0zGSqJ2aUY\n", "O0zGSqJ2abk\n");
        assert!(payload(bad_tag.as_bytes(), VECTOR_PASSPHRASE).unwrap_err().is::<WrongPassphrase>());
        // The vector `scrypt_work_factor_23`, refused by its header before its passphrase is stretched.
        let costly = VECTOR_HEADER.replace(" 10\n", " 23\n");
        for other in [
            costly.as_str(),
            &VECTOR_HEADER.replace(" 10\n", " 010\n"),
            &VECTOR_HEADER.replace("-> scrypt", "-> X25519"),
            &VECTOR_HEADER.replace("/v1\n", "/v2\n"),
            &VECTOR_HEADER.replace("5CQ 10", "5CQ= 10"),
            &VECTOR_HEADER.replace("5CQ 10", "5CR 10"),
            "no age file at all\n",
            "",
        ] {
            let refused = payload(other.as_bytes(), VECTOR_PASSPHRASE).unwrap_err();
            assert_eq!(refused.to_string(), NO_BACKUP, "{other:?}");
        }
        let mut other_mac = vector();
        let mac_at = VECTOR_HEADER.rfind(END).unwrap() + END.len() + 1;
        other_mac[mac_at] = b'J';
        assert_eq!(
            payload(&other_mac, VECTOR_PASSPHRASE).unwrap_err().to_string(),
            DAMAGED,
            "a header changed behind its wrapped key"
        );
    }

    #[test]
    fn payload_chunks_round_trip() {
        for length in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK, 2 * CHUNK + 5] {
            let plain: Vec<u8> = (0..length).map(|at| (at % 251) as u8).collect();
            let file = written(&plain, 4099);
            let chunks = length.div_ceil(CHUNK).max(1);
            assert_eq!(
                file.len(),
                VECTOR_HEADER.len() + PAYLOAD_NONCE_BYTES + length + chunks * TAG_BYTES,
                "{length}: a header of the vector's length, the nonce, and a tag to each chunk, the last never empty"
            );
            assert!(payload(&file, samples::PASSPHRASE).unwrap() == plain, "{length}");
            assert!(written(&plain, usize::MAX) == file, "{length}: however it is handed over");
        }
        let plain = vec![3u8; 2 * CHUNK + 5];
        let file = written(&plain, CHUNK);
        let payload_at = VECTOR_HEADER.len() + PAYLOAD_NONCE_BYTES;
        let (first, second) = (payload_at + CHUNK + TAG_BYTES, payload_at + 2 * (CHUNK + TAG_BYTES));
        for cut in [file.len() - 1, second, first, payload_at, payload_at - 1] {
            let refused = payload(&file[..cut], samples::PASSPHRASE).unwrap_err();
            assert_eq!(refused.to_string(), DAMAGED, "cut to {cut} bytes");
        }
        let mut changed = file.clone();
        changed[payload_at + 17] ^= 1;
        assert_eq!(payload(&changed, samples::PASSPHRASE).unwrap_err().to_string(), DAMAGED);
        let swapped = [&file[..payload_at], &file[first..second], &file[payload_at..first], &file[second..]].concat();
        assert_eq!(
            payload(&swapped, samples::PASSPHRASE).unwrap_err().to_string(),
            DAMAGED,
            "two chunks in each other's place"
        );
        assert!(payload(&file, "another").unwrap_err().is::<WrongPassphrase>());
    }

    #[test]
    fn base64_without_padding_is_canonical() {
        assert_eq!(from_base64("Zm8").as_deref(), Some(&b"fo"[..]));
        for refused in ["Zm8=", "Zm9", "Zh", "Z", "Zm 8"] {
            assert_eq!(from_base64(refused), None, "{refused:?}: padded, or with bits past its bytes");
        }
    }
}
