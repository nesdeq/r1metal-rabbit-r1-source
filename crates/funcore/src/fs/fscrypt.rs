//! The kernel's directory encryption (fscrypt) as the safe has it: its policy and key.

/// The kernel's largest key, in bytes.
pub const KEY_SIZE: usize = 64;
const VERSION_2: u8 = 2;
const AES_256_XTS: u8 = 1;
const AES_256_CTS: u8 = 4;
const PAD_32: u8 = 0x03;
const POLICY_BYTES: u64 = 24;

/// A safe's key, zeroed on drop, with no `Clone` and no `Debug`.
pub struct Key([u8; KEY_SIZE]);

impl Key {
    /// A key of zeros, for the caller to fill.
    pub const fn zeroed() -> Key {
        Key([0; KEY_SIZE])
    }

    pub const fn bytes(&self) -> &[u8; KEY_SIZE] {
        &self.0
    }

    pub const fn bytes_mut(&mut self) -> &mut [u8; KEY_SIZE] {
        &mut self.0
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        self.0.fill(0);
        core::hint::black_box(&self.0);
    }
}

/// A safe's policy: version 2, contents in AES-256-XTS, names in AES-256-CTS padded to 32 bytes.
pub const POLICY: Policy = Policy { version: VERSION_2, contents: AES_256_XTS, names: AES_256_CTS, flags: PAD_32 };

/// A directory's encryption policy, by the kernel's numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Policy {
    pub version: u8,
    pub contents: u8,
    pub names: u8,
    pub flags: u8,
}

const fn is_safe_version(version: u8) -> bool {
    version == POLICY.version
}

/// True for a kernel policy of a safe's size and version; another version identifies its key differently.
pub const fn is_safe_policy(size: u64, version: u8) -> bool {
    size == POLICY_BYTES && is_safe_version(version)
}

/// True if the kernel's identifier of an added key is the expected one.
pub fn is_same_key(added: &[u8], expected: &[u8]) -> bool {
    added == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_policy() {
        assert_eq!(POLICY, Policy { version: 2, contents: 1, names: 4, flags: 3 }, "version 2, XTS, CTS, padded to 32");
        assert!(is_safe_version(2), "a policy of version 2 names its key by an identifier");
        assert!(!is_safe_version(0), "one of version 1, which the kernel numbers 0, names its key otherwise");
        assert!(is_safe_policy(24, 2) && !is_safe_policy(12, 2) && !is_safe_policy(24, 0));
        assert!(is_same_key(&[1], &[1]) && !is_same_key(&[1], &[2]));
    }
}
