//! The hardware address a station shows each network, derived from the device's secret and the network's name.

use crate::crypto::Primitives;
use alloc::vec::Vec;

pub const ADDRESS_BYTES: usize = 6;
pub const SECRET_BYTES: usize = 32;
/// The file of the WiFi directory that holds the device's secret.
pub const SECRET_FILE: &str = "secret";
pub const SECRET_MADE_LINE: &str = "wifi: a secret of the device's own made and kept";
const MULTICAST: u8 = 0x01;
const LOCALLY_ADMINISTERED: u8 = 0x02;

/// The secret a file holds, none for one of another length.
pub fn parse_secret(bytes: Vec<u8>) -> Option<[u8; SECRET_BYTES]> {
    bytes.try_into().ok()
}

/// The hardware address shown to a network of that name: the same at every join of it, another for every other, no maker's.
pub fn derive_address(crypto: &impl Primitives, secret: &[u8; SECRET_BYTES], name: &[u8]) -> [u8; ADDRESS_BYTES] {
    let hash = crypto.hmac_sha256(secret, &[name]);
    let mut address = [0u8; ADDRESS_BYTES];
    address.copy_from_slice(&hash[..ADDRESS_BYTES]);
    address[0] = address[0] & !MULTICAST | LOCALLY_ADMINISTERED;
    address
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::wpa::tests::Sums;

    #[test]
    fn secret_length() {
        assert_eq!(parse_secret(alloc::vec![7; SECRET_BYTES]), Some([7; SECRET_BYTES]));
        assert_eq!(parse_secret(alloc::vec![7; SECRET_BYTES - 1]), None);
    }

    #[test]
    fn derived_address() {
        let home = derive_address(&Sums, &[7; SECRET_BYTES], b"home");
        assert_eq!(home, derive_address(&Sums, &[7; SECRET_BYTES], b"home"), "the same at every join");
        assert!(
            home != derive_address(&Sums, &[7; SECRET_BYTES], b"cafe")
                && home != derive_address(&Sums, &[8; SECRET_BYTES], b"home"),
            "another for another network, and for another device"
        );
        for name in [&b"home"[..], b"cafe", b"", b"a network of thirty-two letters.."] {
            assert_eq!(
                derive_address(&Sums, &[7; SECRET_BYTES], name)[0] & 0x03,
                0x02,
                "one station's own, and no maker's"
            );
        }
        assert_eq!(
            home[1..],
            Sums.hmac_sha256(&[7; SECRET_BYTES], &[b"home"])[1..ADDRESS_BYTES],
            "the rest as the hash has it"
        );
    }
}
