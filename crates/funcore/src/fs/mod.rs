//! Filesystem formats, built and read in memory: exFAT, f2fs, fscrypt, FUSE, the bootloader's dump table and tar.

pub mod exfat;
pub mod f2fs;
pub mod fscrypt;
pub mod fuse;
pub mod kedump;
pub mod tar;

/// The error text of a filesystem that cannot be built.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BuildError(&'static str);

impl BuildError {
    pub const fn message(self) -> &'static str {
        self.0
    }
}

/// A file's time as a tape and a volume keep it, in seconds since 1970: zero before.
pub const fn to_modified(seconds: i64) -> u64 {
    if seconds < 0 { 0 } else { seconds as u64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_before_1970() {
        assert_eq!([i64::MIN, -1, 0, 1_791_697_759].map(to_modified), [0, 0, 0, 1_791_697_759]);
    }
}
