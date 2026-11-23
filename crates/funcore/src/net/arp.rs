//! ARP (RFC 826): the kernel's askings for a station, when a neighbour that did not answer is asked again and how often; and a router's answer to the one host that asks for it.

use super::Address;
use super::ethernet::Hardware;
use alloc::vec::Vec;

/// What a join writes of the kernel's ARP settings: a neighbour that did not answer is asked again every 100 ms, the least RFC 8305 has two attempts lie apart, and 10 times in all, through the 1 s a join's proof of reach gets, where the kernel's own are 1 s and 3 times.
pub const JOIN_WRITES: [(&str, &str); 2] = [("retrans_time_ms", "100"), ("mcast_solicit", "10")];

// Ethernet's hardware addresses for IPv4's, six bytes for four.
const KINDS: [u8; 6] = [0, 1, 0x08, 0x00, 6, 4];
const REQUEST: [u8; 2] = [0, 1];
const REPLY: [u8; 2] = [0, 2];
const SENDER_AT: usize = 8;
const SENDER_ADDRESS_AT: usize = 14;
const TARGET_ADDRESS_AT: usize = 24;
const BYTES: usize = 28;

// The answer of the station at `address`, whose hardware address is `hardware`, to a message that asks who has that address, and the asker it goes to; none for any other message.
pub(crate) fn answer(message: &[u8], address: Address, hardware: Hardware) -> Option<(Hardware, Vec<u8>)> {
    let message: &[u8; BYTES] = message.first_chunk()?;
    if message[..6] != KINDS || message[6..SENDER_AT] != REQUEST || message[TARGET_ADDRESS_AT..] != address {
        return None;
    }
    let asker: Hardware = *message[SENDER_AT..].first_chunk()?;
    let mut reply = Vec::with_capacity(BYTES);
    reply.extend(KINDS);
    reply.extend(REPLY);
    reply.extend(hardware);
    reply.extend(address);
    reply.extend_from_slice(&message[SENDER_AT..SENDER_ADDRESS_AT + 4]);
    Some((asker, reply))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The kernel's own time between two askings and count of them, as the R1's `neigh/default` reads.
    const KERNEL_MS: u32 = 1000;
    const KERNEL_ASKS: u32 = 3;

    #[test]
    fn sooner_and_oftener_than_kernel_default() {
        let [(retry, ms), (count, asks)] = JOIN_WRITES;
        assert_eq!((retry, count), ("retrans_time_ms", "mcast_solicit"), "the kernel's names of the two");
        assert!(ms.parse::<u32>().is_ok_and(|ms| ms < KERNEL_MS), "in milliseconds, and sooner than its own");
        assert!(asks.parse::<u32>().is_ok_and(|asks| asks > KERNEL_ASKS), "and more askings than its own");
    }

    const ROUTER: Address = [192, 168, 42, 1];
    const HOST: Address = [192, 168, 42, 2];
    const ROUTER_HARDWARE: Hardware = [2, 0, 0, 0, 0, 9];
    const HOST_HARDWARE: Hardware = [2, 0, 0, 0, 0, 1];

    fn request(sender: Address, target: Address) -> Vec<u8> {
        [&KINDS[..], &REQUEST, &HOST_HARDWARE, &sender, &[0; 6], &target].concat()
    }

    #[test]
    fn router_answers_for_its_own_address() {
        let (asker, reply) = answer(&request(HOST, ROUTER), ROUTER, ROUTER_HARDWARE).expect("an answer");
        assert_eq!(asker, HOST_HARDWARE);
        assert_eq!(
            reply,
            [&KINDS[..], &REPLY, &ROUTER_HARDWARE, &ROUTER, &HOST_HARDWARE, &HOST].concat(),
            "the router's two addresses, then the asker's"
        );
        let mut padded = request(HOST, ROUTER);
        padded.extend([0; 18]);
        assert!(answer(&padded, ROUTER, ROUTER_HARDWARE).is_some(), "a frame's filling is passed over");
        assert_eq!(
            answer(&request([0; 4], HOST), ROUTER, ROUTER_HARDWARE),
            None,
            "the host's probe for its own address"
        );
        assert_eq!(answer(&request(HOST, [192, 168, 42, 3]), ROUTER, ROUTER_HARDWARE), None, "another's address");
        assert_eq!(answer(&reply, ROUTER, ROUTER_HARDWARE), None, "an answer asks nothing");
        assert_eq!(answer(&request(HOST, ROUTER)[..27], ROUTER, ROUTER_HARDWARE), None, "cut short");
        let mut other = request(HOST, ROUTER);
        other[1] = 6;
        assert_eq!(answer(&other, ROUTER, ROUTER_HARDWARE), None, "another kind of hardware");
    }
}
