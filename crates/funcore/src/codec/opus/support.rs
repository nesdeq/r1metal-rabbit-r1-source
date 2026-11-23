use alloc::vec::Vec;

// The packets of a file as `opus_demo` writes it: each behind its length and its encoder's final range, four bytes each, highest first.
pub(super) fn read_demo(mut bytes: &[u8]) -> Vec<(&[u8], u32)> {
    let mut packets = Vec::new();
    while let Some((head, rest)) = bytes.split_first_chunk::<8>() {
        let (length, range) = head.split_at(4);
        let length = u32::from_be_bytes(length.try_into().expect("four")) as usize;
        let (packet, rest) = rest.split_at(length);
        packets.push((packet, u32::from_be_bytes(range.try_into().expect("four"))));
        bytes = rest;
    }
    assert!(bytes.is_empty(), "a file of whole packets");
    packets
}

// The packets of a fixture of ours: each behind its length, two bytes, highest first.
pub(crate) fn read_packets(mut bytes: &[u8]) -> Vec<&[u8]> {
    let mut packets = Vec::new();
    while let Some((length, rest)) = bytes.split_first_chunk::<2>() {
        let (packet, rest) = rest.split_at(usize::from(u16::from_be_bytes(*length)));
        packets.push(packet);
        bytes = rest;
    }
    assert!(bytes.is_empty(), "a file of whole packets");
    packets
}

// A fixture's sound as 16 bit samples, lowest byte first.
pub(super) fn read_samples(bytes: &[u8]) -> Vec<i16> {
    let (samples, rest) = bytes.as_chunks::<2>();
    assert!(rest.is_empty(), "a file of whole samples");
    samples.iter().map(|sample| i16::from_le_bytes(*sample)).collect()
}

#[test]
fn files_read() {
    let demo = [0, 0, 0, 2, 0x12, 0x34, 0x56, 0x78, 9, 8, 0, 0, 0, 0, 0, 0, 0, 1];
    assert_eq!(read_demo(&demo), [(&[9, 8][..], 0x1234_5678), (&[][..], 1)], "a packet of no bytes was not sent");
    assert_eq!(read_packets(&[0, 1, 7, 0, 0, 0, 2, 5, 6]), [&[7][..], &[], &[5, 6]]);
    assert_eq!(read_samples(&[0x34, 0x12, 0xFF, 0xFF]), [0x1234, -1]);
}
