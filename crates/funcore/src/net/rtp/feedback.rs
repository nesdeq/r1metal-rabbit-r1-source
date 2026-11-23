use super::rtcp::{PSFB, RTPFB, begin, finish};
use super::{SSRC_BYTES, WORD};
use crate::base::bytes::be32;
use alloc::vec::Vec;

// RFC 4585, 6.2 and 6.3, and RFC 5104, 4.3: the formats of the transport layer's and the payload's feedback.
pub(super) const NACK: u8 = 1;
pub(super) const PLI: u8 = 1;
pub(super) const FIR: u8 = 4;
pub(super) const AFB: u8 = 15;
// RFC 4585, 6.1: the sender's SSRC and the media source's ahead of the feedback control information.
const HEAD: usize = 2 * SSRC_BYTES;
// RFC 4585, 6.2.1: a lost packet's sequence number and a mask of the sixteen after it.
const MASK_BITS: u16 = u16::BITS as u16;
// RFC 5104, 4.3.1.1: an SSRC, the command's sequence number, three bytes reserved.
const FIR_ENTRY: usize = 8;
// draft-alvestrand-rmcat-remb-03, 2.2: the identifier, the count of SSRCs, six bits of exponent and eighteen of mantissa.
const REMB: &[u8; 4] = b"REMB";
const REMB_HEAD: usize = 8;
const COUNT_SHIFT: u32 = 24;
const MANTISSA_BITS: u32 = 18;
const EXPONENT_BITS: u32 = 0x3F;
const MOST_MANTISSA: u64 = (1 << MANTISSA_BITS) - 1;

// RFC 4585, 6.2.1: a generic NACK.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Nack<'a> {
    sender: u32,
    pub(super) media: u32,
    fci: &'a [u8],
}

// RFC 4585, 6.3.1: picture loss.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Pli {
    pub(super) sender: u32,
    pub(super) media: u32,
}

// RFC 5104, 4.3.1: a full intra request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Fir<'a> {
    sender: u32,
    entries: &'a [u8],
}

// draft-alvestrand-rmcat-remb-03: the receiver's estimate of the most it takes, in bits a second.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Remb {
    sender: u32,
    pub(super) bitrate: u64,
}

// A feedback message's sender, media source and feedback control information (RFC 4585, 6.1).
pub(super) fn parts(body: &[u8]) -> Option<(u32, u32, &[u8])> {
    Some((be32(body, 0)?, be32(body, SSRC_BYTES)?, body.get(HEAD..)?))
}

impl<'a> Nack<'a> {
    // None without an entry, which RFC 4585, 6.2.1 asks for.
    pub(super) fn read(sender: u32, media: u32, fci: &'a [u8]) -> Option<Nack<'a>> {
        (fci.len() >= WORD).then_some(Nack { sender, media, fci })
    }

    // The sequence numbers of the packets reported lost, each entry's first then those its mask names.
    pub(super) fn lost(&self) -> impl Iterator<Item = u16> + 'a {
        self.fci.as_chunks::<WORD>().0.iter().flat_map(|&[first_high, first_low, mask_high, mask_low]| {
            let (first, mask) =
                (u16::from_be_bytes([first_high, first_low]), u16::from_be_bytes([mask_high, mask_low]));
            let following =
                (0..MASK_BITS).filter(move |bit| mask >> bit & 1 == 1).map(move |bit| first.wrapping_add(bit + 1));
            core::iter::once(first).chain(following)
        })
    }
}

impl<'a> Fir<'a> {
    // None without an entry, which RFC 5104, 4.3.1 asks for; the media source's SSRC is nought there and not read.
    pub(super) fn read(sender: u32, fci: &'a [u8]) -> Option<Fir<'a>> {
        (fci.len() >= FIR_ENTRY).then_some(Fir { sender, entries: fci })
    }

    // Each entry's SSRC and command sequence number.
    pub(super) fn entries(&self) -> impl Iterator<Item = (u32, u8)> + 'a {
        self.entries
            .as_chunks::<FIR_ENTRY>()
            .0
            .iter()
            .map(|&[a, b, c, d, sequence, ..]| (u32::from_be_bytes([a, b, c, d]), sequence))
    }
}

impl Remb {
    // None for an application's message that is no REMB, or whose SSRCs do not fit.
    pub(super) fn read(sender: u32, fci: &[u8]) -> Option<Remb> {
        let word = be32(fci, REMB.len()).filter(|_| fci.starts_with(REMB))?;
        let count = (word >> COUNT_SHIFT) as usize;
        fci.get(REMB_HEAD..REMB_HEAD + count * SSRC_BYTES)?;
        let (exponent, mantissa) = (word >> MANTISSA_BITS & EXPONENT_BITS, u64::from(word) & MOST_MANTISSA);
        let bitrate = u64::try_from(u128::from(mantissa) << exponent).unwrap_or(u64::MAX);
        Some(Remb { sender, bitrate })
    }
}

// Appends a generic NACK of sequence numbers in the order they were sent, each entry the first not yet named and the sixteen after it; false, appending nothing, for none or more than one packet carries.
pub(super) fn write_nack(out: &mut Vec<u8>, sender: u32, media: u32, lost: &[u16]) -> bool {
    let Some((&first, rest)) = lost.split_first() else {
        return false;
    };
    let start = begin(out, NACK, RTPFB);
    out.extend(sender.to_be_bytes());
    out.extend(media.to_be_bytes());
    let (mut entry, mut mask) = (first, 0u16);
    for &sequence in rest {
        let distance = sequence.wrapping_sub(entry);
        if (1..=MASK_BITS).contains(&distance) {
            mask |= 1 << (distance - 1);
        } else {
            out.extend(entry.to_be_bytes());
            out.extend(mask.to_be_bytes());
            (entry, mask) = (sequence, 0);
        }
    }
    out.extend(entry.to_be_bytes());
    out.extend(mask.to_be_bytes());
    finish(out, start)
}

pub(super) fn write_pli(out: &mut Vec<u8>, sender: u32, media: u32) -> bool {
    let start = begin(out, PLI, PSFB);
    out.extend(sender.to_be_bytes());
    out.extend(media.to_be_bytes());
    finish(out, start)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::net::rtp::rtcp::tests::one;
    use crate::net::rtp::rtcp::{Packet, read};
    use crate::net::rtp::tests::bytes;
    use alloc::vec;

    // A REMB's count of SSRCs in its one byte (draft-alvestrand-rmcat-remb-03, 2.2).
    const MOST_REMB_SSRCS: usize = u8::MAX as usize;

    // Appends a full intra request of each SSRC and command sequence number, the media source nought; false, appending nothing, for none or more than one packet carries.
    pub(in crate::net::rtp) fn write_fir(out: &mut Vec<u8>, sender: u32, entries: &[(u32, u8)]) -> bool {
        if entries.is_empty() {
            return false;
        }
        let start = begin(out, FIR, PSFB);
        out.extend(sender.to_be_bytes());
        out.extend(0u32.to_be_bytes());
        for (ssrc, sequence) in entries {
            out.extend(ssrc.to_be_bytes());
            out.extend([*sequence, 0, 0, 0]);
        }
        finish(out, start)
    }

    // Appends a REMB of a bitrate for SSRCs, the media source nought, the bitrate in the fewest bits of exponent, its least bits dropped as they must be; false, appending nothing, for more SSRCs than its count tells.
    pub(in crate::net::rtp) fn write_remb(out: &mut Vec<u8>, sender: u32, bitrate: u64, ssrcs: &[u32]) -> bool {
        if ssrcs.len() > MOST_REMB_SSRCS {
            return false;
        }
        let exponent = (u64::BITS - bitrate.leading_zeros()).saturating_sub(MANTISSA_BITS);
        let start = begin(out, AFB, PSFB);
        out.extend(sender.to_be_bytes());
        out.extend(0u32.to_be_bytes());
        out.extend_from_slice(REMB);
        let word = (ssrcs.len() as u32) << COUNT_SHIFT | exponent << MANTISSA_BITS | (bitrate >> exponent) as u32;
        out.extend(word.to_be_bytes());
        out.extend(ssrcs.iter().flat_map(|ssrc| ssrc.to_be_bytes()));
        finish(out, start)
    }

    // The NACK payloads of the calling server's nack.rs tests, after the sender's SSRC, read and written as the server reads and writes them.
    #[test]
    fn nacks_of_the_calling_server() {
        let long = vec![
            0x0506, 0x0508, 0x0509, 0x050B, 0x050C, 0x050E, 0x050F, 0x0511, 0x0513, 0x0515, 0x0516, 0x0518, 0x0519,
            0x051B, 0x051C, 0x051D, 0x0525, 0x0526, 0x0527, 0x0528,
        ];
        for (lost, entries) in
            [(vec![0, 1, 3, 8, 16], "00008085"), (long, "0506d5b6 0518f01d"), (vec![0xFFFF, 0, 1], "ffff0003")]
        {
            let mut out = Vec::new();
            assert!(write_nack(&mut out, 0, 0x0102_0304, &lost));
            assert_eq!(out[12..], bytes(entries)[..], "the entries of {lost:?}");
            let Packet::Nack(nack) = one(&out) else { panic!("a NACK") };
            assert_eq!((nack.sender, nack.media, nack.lost().collect::<Vec<_>>()), (0, 0x0102_0304, lost));
        }
        assert!(!write_nack(&mut Vec::new(), 0, 0, &[]), "a NACK of none");
        let empty = bytes("81cd0002 00000000 01020304");
        assert!(read(&empty).next().is_none(), "a NACK without an entry is passed over");
    }

    #[test]
    fn picture_loss_and_full_intra() {
        let mut out = Vec::new();
        assert!(write_pli(&mut out, 0, 1002));
        assert_eq!(out, bytes("81ce0002 00000000 000003ea"), "RFC 4585, 6.3.1: no FCI, the length 2");
        assert_eq!(one(&out), Packet::Pli(Pli { sender: 0, media: 1002 }));
        let mut out = Vec::new();
        assert!(write_fir(&mut out, 1, &[(1003, 7), (2003, 255)]));
        assert_eq!(
            out,
            bytes("84ce0006 00000001 00000000 000003eb 07000000 000007d3 ff000000"),
            "RFC 5104, 4.3.1.1: the length 2 + 2 * 2, the media source nought"
        );
        let Packet::Fir(fir) = one(&out) else { panic!("a FIR") };
        assert_eq!((fir.sender, fir.entries().collect::<Vec<_>>()), (1, vec![(1003, 7), (2003, 255)]));
        assert!(!write_fir(&mut out, 1, &[]) && out.len() == 28);
        assert!(read(&bytes("84ce0002 00000001 00000000")).next().is_none(), "a FIR without an entry");
    }

    #[test]
    fn receiver_estimates() {
        let mut out = Vec::new();
        assert!(write_remb(&mut out, 1, 1_000_000, &[1003]));
        assert_eq!(
            out,
            bytes("8fce0005 00000001 00000000 52454d42 010bd090 000003eb"),
            "one SSRC, exponent 2, mantissa 250 000"
        );
        let Packet::Remb(remb) = one(&out) else { panic!("a REMB") };
        assert_eq!((remb.sender, remb.bitrate), (1, 1_000_000));
        let mut out = Vec::new();
        assert!(write_remb(&mut out, 1, (1 << 18) - 1, &[]));
        assert_eq!(out[16..20], [0, 3, 0xFF, 0xFF], "eighteen bits need no exponent");
        let mut out = Vec::new();
        assert!(write_remb(&mut out, 1, 1_000_001, &[]));
        let Packet::Remb(remb) = one(&out) else { panic!("a REMB") };
        assert_eq!(remb.bitrate, 1_000_000, "the least bits dropped");
        let saturated = bytes("8fce0004 00000001 00000000 52454d42 00ffffff");
        let Packet::Remb(remb) = one(&saturated) else { panic!("a REMB") };
        assert_eq!(remb.bitrate, u64::MAX, "a mantissa shifted past 64 bits");
        assert!(read(&bytes("8fce0004 00000001 00000000 52454d43 00000000")).next().is_none(), "another application's");
        assert!(
            read(&bytes("8fce0004 00000001 00000000 52454d42 02000000")).next().is_none(),
            "two SSRCs told, none there"
        );
        assert!(!write_remb(&mut Vec::new(), 1, 1, &[0; 256]));
    }
}
