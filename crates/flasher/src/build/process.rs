//! The programs the build runs: each has to succeed, and says which it was if it does not.

use crate::{Result, of, part, to_hex};
use aws_lc_rs::digest;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Command;

// Runs a command to its end; an error if it cannot start or does not succeed.
pub(super) fn run(command: &mut Command) -> Result<()> {
    let status = command.status().map_err(|e| format!("{command:?}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{command:?} failed: {status}").into()) }
}

// The SHA-256 of a file, as `sha256sum` prints it.
pub(super) fn checksum(file: &Path) -> Result<String> {
    let mut input = BufReader::new(File::open(file).map_err(|e| of(file, e))?);
    let mut sum = digest::Context::new(&digest::SHA256);
    loop {
        let read = input.fill_buf().map_err(|e| of(file, e))?;
        if read.is_empty() {
            return Ok(to_hex(sum));
        }
        sum.update(read);
        let taken = read.len();
        input.consume(taken);
    }
}

// Fetches an archive unless it is there, and makes sure it is the one whose SHA-256 is `sha256`.
pub(super) fn fetch_archive(url: &str, sha256: &str, archive: &Path) -> Result<()> {
    if !archive.exists() {
        eprintln!("fetching {url}");
        if let Some(parent) = archive.parent() {
            fs::create_dir_all(parent).map_err(|e| of(parent, e))?;
        }
        let partial = part(archive);
        run(Command::new("curl").args(["-sSfL", "-o"]).arg(&partial).arg(url))?;
        fs::rename(&partial, archive).map_err(|e| of(archive, e))?;
    }
    let found = checksum(archive)?;
    if found != sha256 {
        return Err(of(archive, format!("its SHA-256 is {found}, not {sha256}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_vectors() {
        let file = std::env::temp_dir().join(format!("r1-checksum-{}", std::process::id()));
        // The vectors of FIPS 180-2 for SHA-256: "abc", and a million times "a", which no one read brings.
        let vectors: [(&[u8], &str); 2] = [
            (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (&[b'a'; 1_000_000], "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"),
        ];
        for (bytes, sum) in vectors {
            fs::write(&file, bytes).unwrap();
            assert_eq!(checksum(&file).unwrap(), sum, "{} bytes", bytes.len());
        }
        fs::remove_file(&file).unwrap();
        let missing = checksum(&file).unwrap_err().to_string();
        assert!(missing.starts_with(&format!("{}: ", file.display())), "{missing}");
    }
}
