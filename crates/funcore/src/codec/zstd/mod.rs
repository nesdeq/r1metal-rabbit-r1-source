mod bits;
mod find;
mod fse;
mod huffman;

use alloc::vec;
use alloc::vec::Vec;
use bits::Bits;
use find::{Finder, REACH, Sequence};
use fse::Coder;

const MAGIC: u32 = 0xFD2F_B528;
// The frame's descriptor (RFC 8878, 3.1.1.1.1): the flag of the content size's field from bit 6, one segment at bit 5.
const CONTENT_SIZE_SHIFT: u32 = 6;
const SINGLE_SEGMENT: u8 = 1 << 5;
// The content sizes a field of two bytes holds, told less the least of them (RFC 8878, 3.1.1.1.4).
const TWO_BYTES_LEAST: u64 = 256;
const TWO_BYTES_MOST: u64 = TWO_BYTES_LEAST + u16::MAX as u64;
const BLOCK: usize = 128 << 10;
// A block's head (RFC 8878, 3.1.1.2): its type from bit 1, its size from bit 3.
const BLOCK_TYPE_SHIFT: u32 = 1;
const BLOCK_SIZE_SHIFT: u32 = 3;
const RAW: u32 = 0;
const COMPRESSED: u32 = 2;
const RAW_LITERALS: u32 = 0;
const RLE_LITERALS: u32 = 1;
const HUFFMAN_LITERALS: u32 = 2;
// A literals section's head (RFC 8878, 3.1.1.3.1.1): the form of its sizes from bit 2, a size from bit 3 behind a form of one bit and from bit 4 behind one of two.
const FORMAT_SHIFT: u32 = 2;
const SHORT_SIZE_SHIFT: u32 = 3;
const SIZE_SHIFT: u32 = 4;
// The bits of a raw or run-length section's size under the form of one bit and under form 1; form 3 has room for any block's.
const PLAIN_SIZE_BITS: [u32; 2] = [5, 12];
// The bits of each of a coded section's two sizes under forms 0 and 1, 2, and 3; form 0 is one stream.
const CODED_SIZE_BITS: [u32; 3] = [10, 14, 18];
const SINGLE_STREAM_BYTES: usize = (1 << CODED_SIZE_BITS[0]) - 1;
// A count of sequences (RFC 8878, 3.1.1.3.2.1): a byte under 128, two bytes under 0x7F00 with 128 on the first, else 255 before the count less 0x7F00.
const SHORT_COUNT: usize = 128;
const LONG_COUNT: usize = 0x7F00;
const LONG_COUNT_MARK: u8 = 255;
// The byte of the three codes' modes, each predefined.
const PREDEFINED_MODES: u8 = 0;
const FIRST_OFFSETS: [u32; 3] = [1, 4, 8];

const LITERAL_BASES: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64, 128, 256, 512, 1024,
    2048, 4096, 8192, 16384, 32768, 65536,
];
const LITERAL_BITS: [u32; 36] =
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
const MATCH_BASES: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027, 2051, 4099, 8195, 16387, 32771, 65539,
];
const MATCH_BITS: [u32; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2,
    3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];

struct Coders {
    literals: Coder,
    offsets: Coder,
    lengths: Coder,
}

fn block_head(is_last: bool, kind: u32, size: usize) -> [u8; 3] {
    let head = u32::from(is_last) | kind << BLOCK_TYPE_SHIFT | (size as u32) << BLOCK_SIZE_SHIFT;
    [head as u8, (head >> 8) as u8, (head >> 16) as u8]
}

fn plain_head(kind: u32, count: usize) -> Vec<u8> {
    let (count, [short, middle]) = (count as u32, PLAIN_SIZE_BITS);
    if count >> short == 0 {
        vec![(kind | count << SHORT_SIZE_SHIFT) as u8]
    } else if count >> middle == 0 {
        (kind | 1 << FORMAT_SHIFT | count << SIZE_SHIFT).to_le_bytes()[..2].to_vec()
    } else {
        (kind | 3 << FORMAT_SHIFT | count << SIZE_SHIFT).to_le_bytes()[..3].to_vec()
    }
}

fn coded_head(literals: usize, coded: usize, is_single: bool) -> Vec<u8> {
    let [short, middle, long] = CODED_SIZE_BITS;
    let (format, width) = match literals.max(coded) {
        _ if is_single => (0, short),
        size if size >> short == 0 => (1, short),
        size if size >> middle == 0 => (2, middle),
        _ => (3, long),
    };
    let sizes = (literals as u64) << SIZE_SHIFT | (coded as u64) << (SIZE_SHIFT + width);
    let head = u64::from(HUFFMAN_LITERALS) | format << FORMAT_SHIFT | sizes;
    head.to_le_bytes()[..(SIZE_SHIFT + 2 * width) as usize / 8].to_vec()
}

fn literals_section(literals: &[u8]) -> Vec<u8> {
    if let [first, rest @ ..] = literals
        && !rest.is_empty()
        && rest.iter().all(|byte| byte == first)
    {
        let mut rle = plain_head(RLE_LITERALS, literals.len());
        rle.push(*first);
        return rle;
    }
    let mut section = plain_head(RAW_LITERALS, literals.len());
    section.extend(literals);
    let is_single = literals.len() <= SINGLE_STREAM_BYTES;
    if let Some(coded) =
        huffman::compress_literals(literals, is_single).filter(|coded| !is_single || coded.len() <= SINGLE_STREAM_BYTES)
    {
        let mut huffman = coded_head(literals.len(), coded.len(), is_single);
        huffman.extend(coded);
        if huffman.len() < section.len() {
            section = huffman;
        }
    }
    section
}

fn length_code(length: u32, bases: &[u32]) -> usize {
    bases.iter().rposition(|&base| base <= length).unwrap_or(0)
}

fn offset_code(offset: u32) -> u32 {
    31 - offset.leading_zeros()
}

fn add_extra_bits(sequence: &Sequence, (literal, offset, length): (usize, u32, usize), bits: &mut Bits) {
    bits.add(u64::from(sequence.literals - LITERAL_BASES[literal]), LITERAL_BITS[literal]);
    bits.add(u64::from(sequence.length - MATCH_BASES[length]), MATCH_BITS[length]);
    bits.add(u64::from(sequence.offset - (1 << offset)), offset);
}

fn sequences_section(sequences: &[Sequence], coders: &Coders) -> Vec<u8> {
    let count = sequences.len();
    let mut section = match count {
        0..SHORT_COUNT => vec![count as u8],
        SHORT_COUNT..LONG_COUNT => vec![(count >> 8) as u8 + SHORT_COUNT as u8, count as u8],
        _ => vec![LONG_COUNT_MARK, (count - LONG_COUNT) as u8, ((count - LONG_COUNT) >> 8) as u8],
    };
    if count == 0 {
        return section;
    }
    section.push(PREDEFINED_MODES);
    let codes: Vec<(usize, u32, usize)> = sequences
        .iter()
        .map(|sequence| {
            (
                length_code(sequence.literals, &LITERAL_BASES),
                offset_code(sequence.offset),
                length_code(sequence.length, &MATCH_BASES),
            )
        })
        .collect();
    let mut bits = Bits::default();
    let (literal, offset, length) = codes[count - 1];
    let mut lengths = coders.lengths.begin(length);
    let mut offsets = coders.offsets.begin(offset as usize);
    let mut literals = coders.literals.begin(literal);
    add_extra_bits(&sequences[count - 1], codes[count - 1], &mut bits);
    for at in (0..count - 1).rev() {
        let (literal, offset, length) = codes[at];
        coders.offsets.code(&mut offsets, offset as usize, &mut bits);
        coders.lengths.code(&mut lengths, length, &mut bits);
        coders.literals.code(&mut literals, literal, &mut bits);
        add_extra_bits(&sequences[at], codes[at], &mut bits);
    }
    coders.lengths.end(lengths, &mut bits);
    coders.offsets.end(offsets, &mut bits);
    coders.literals.end(literals, &mut bits);
    section.extend(bits.close());
    section
}

fn compress_block(input: &[u8], (start, end): (usize, usize), sequences: &[Sequence], coders: &Coders) -> Vec<u8> {
    let mut literals = Vec::new();
    let mut at = start;
    for sequence in sequences {
        literals.extend_from_slice(&input[at..at + sequence.literals as usize]);
        at += (sequence.literals + sequence.length) as usize;
    }
    literals.extend_from_slice(&input[at..end]);
    let mut block = literals_section(&literals);
    block.extend(sequences_section(sequences, coders));
    block
}

pub(crate) fn compress(input: &[u8]) -> Vec<u8> {
    compress_frame(input, REACH)
}

fn compress_frame(input: &[u8], reach: usize) -> Vec<u8> {
    // Room for what text packs into, under half of it; a frame of noise grows past that.
    let mut frame = Vec::with_capacity(input.len() / 2);
    frame.extend(MAGIC.to_le_bytes());
    // The size in the fewest bytes the frame's header has a field of, 1, 2, 4 or 8 by its flag (RFC 8878, tables 4 and 7).
    let size = input.len() as u64;
    match size {
        0..TWO_BYTES_LEAST => frame.extend([SINGLE_SEGMENT, size as u8]),
        TWO_BYTES_LEAST..=TWO_BYTES_MOST => {
            frame.push(1 << CONTENT_SIZE_SHIFT | SINGLE_SEGMENT);
            frame.extend(((size - TWO_BYTES_LEAST) as u16).to_le_bytes());
        }
        _ if size <= u64::from(u32::MAX) => {
            frame.push(2 << CONTENT_SIZE_SHIFT | SINGLE_SEGMENT);
            frame.extend((size as u32).to_le_bytes());
        }
        _ => {
            frame.push(3 << CONTENT_SIZE_SHIFT | SINGLE_SEGMENT);
            frame.extend(size.to_le_bytes());
        }
    }
    if input.is_empty() {
        frame.extend(block_head(true, RAW, 0));
        return frame;
    }
    let coders = Coders {
        literals: Coder::new(&fse::LITERAL_LENGTHS, fse::LITERAL_LOG),
        offsets: Coder::new(&fse::OFFSETS, fse::OFFSET_LOG),
        lengths: Coder::new(&fse::MATCH_LENGTHS, fse::MATCH_LOG),
    };
    let (mut finder, mut offsets) = (Finder::new(), FIRST_OFFSETS);
    for start in (0..input.len()).step_by(BLOCK) {
        let end = (start + BLOCK).min(input.len());
        let previous_offsets = offsets;
        let sequences = if end <= reach { finder.find(input, start, end, &mut offsets) } else { Vec::new() };
        let block = compress_block(input, (start, end), &sequences, &coders);
        let is_last = end == input.len();
        if block.len() < end - start {
            frame.extend(block_head(is_last, COMPRESSED, block.len()));
            frame.extend(block);
        } else {
            // A block that goes as it is leaves the offsets of the reader as they were.
            offsets = previous_offsets;
            frame.extend(block_head(is_last, RAW, end - start));
            frame.extend(&input[start..end]);
        }
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::checksum::fnv;
    use crate::base::math::xorshift64;
    use alloc::string::String;

    #[test]
    fn predefined_tables() {
        assert_eq!(
            (0..36).map(|code| LITERAL_BASES[code] + (1 << LITERAL_BITS[code]) - 1).max(),
            Some(131_071),
            "literal lengths up to 131071"
        );
        assert_eq!(MATCH_BASES[52] + (1 << MATCH_BITS[52]) - 1, 131_074, "and match lengths up to 131074");
        for (length, expected) in [(0, 0), (15, 15), (16, 16), (17, 16), (18, 17), (63, 24), (64, 25), (131_071, 35)] {
            assert_eq!(length_code(length, &LITERAL_BASES), expected, "literals {length}");
        }
        for (length, expected) in [(3, 0), (34, 31), (35, 32), (36, 32), (98, 41), (99, 42), (131_074, 52)] {
            assert_eq!(length_code(length, &MATCH_BASES), expected, "match {length}");
        }
        assert_eq!([4, 5, 7, 8, 1 << 20].map(offset_code), [2, 2, 2, 3, 20], "an offset's code is its highest bit");
    }

    fn noise(length: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        (0..length)
            .map(|_| {
                state = xorshift64(state);
                state as u8
            })
            .collect()
    }

    fn thread(length: usize) -> Vec<u8> {
        let mut text = String::new();
        let mut number = 0u64;
        let repeated = noise(48, 7);
        while text.len() < length {
            number += 1;
            text.push_str(&alloc::format!(
                "{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"[Friday {} October 2026, 14:{:02}, Wien, spoken] Wie wird das Wetter morgen? Grüße aus Österreich, Straße {}.\"}}]}}\n",
                number % 28 + 1,
                number % 60,
                number * 7
            ));
            text.push_str(&alloc::format!(
                "{{\"id\":\"rs_{number}\",\"type\":\"reasoning\",\"encrypted_content\":\"{}\"}}\n",
                crate::base::code::base64(&noise(96, number)),
            ));
            text.push_str(&alloc::format!(
                "{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"Regen ab Mittag, {} Grad. {}\"}}]}}\n",
                number % 30,
                crate::base::code::base64(&repeated)
            ));
        }
        text.into_bytes()
    }

    fn inputs() -> Vec<Vec<u8>> {
        let text = thread(3 * BLOCK);
        let mut far = thread(BLOCK / 2);
        far.extend(noise(BLOCK + 17, 3));
        far.extend(thread(BLOCK / 2));
        let cycle: Vec<u8> = (0..2 * BLOCK).map(|at| (at % 256) as u8).collect();
        vec![
            Vec::new(),
            vec![7],
            b"abc".to_vec(),
            b"Wie wird das Wetter morgen? Wie wird das Wetter heute?".to_vec(),
            vec![b'a'; 300_000],
            text.clone(),
            text[..BLOCK].to_vec(),
            text[..BLOCK + 1].to_vec(),
            text[..1023].to_vec(),
            text[..1024].to_vec(),
            text[..256].to_vec(),
            text[..65_792].to_vec(),
            far,
            noise(300_000, 11),
            crate::base::code::base64(&noise(200_000, 13)).into_bytes(),
            quaternary(200_000),
            cycle,
        ]
    }

    fn quaternary(length: usize) -> Vec<u8> {
        noise(length, 17).iter().map(|byte| byte & 3).collect()
    }

    #[test]
    fn literals_section_forms() {
        let section = literals_section(&quaternary(4096));
        assert_eq!(
            (section[0] & 3, section[0] >> 2 & 3, section[4]),
            (HUFFMAN_LITERALS as u8, 2, huffman::DIRECT + 3),
            "four streams under a head of four bytes, the weights of three bytes told four bits each: those of equal weights code with no FSE"
        );
        let text = thread(4096);
        let section = literals_section(&text);
        assert!(
            section[0] & 3 == HUFFMAN_LITERALS as u8 && section[4] <= huffman::DIRECT,
            "the weights of text with letters past 128 coded with FSE"
        );
        assert_eq!(
            literals_section(&[9; 40]),
            [0x85, 0x02, 9],
            "one byte forty times: a head of two bytes and the byte"
        );
        assert_eq!(literals_section(&noise(40, 5))[..2], [0x84, 0x02], "noise as it is, behind a head of two bytes");
    }

    fn frames() -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut frames: Vec<(Vec<u8>, Vec<u8>)> = inputs().into_iter().map(|input| (compress(&input), input)).collect();
        let text = thread(3 * BLOCK);
        frames.push((compress_frame(&text, BLOCK), text));
        frames
    }

    const FRAME_HASHES: [u64; 18] = [
        0x608d_6184_a883_29b1,
        0x5ff5_4fc6_d971_abd9,
        0xaf0a_8819_0027_a166,
        0x6e8d_1f56_96bb_4794,
        0x10c2_64b4_412b_131a,
        0x2f4a_509b_22e6_ef4d,
        0x27c9_208e_2753_8f59,
        0xce56_3f8b_ea02_2b0c,
        0x8fb3_92a6_4139_ca9e,
        0x4171_6d01_9977_f425,
        0x791d_ac11_244b_af31,
        0x113f_83e9_7d58_ca8a,
        0x28ce_2961_7393_334f,
        0xea4b_28b2_3086_c08b,
        0x97c0_dd7b_f843_2659,
        0x1846_b9c5_f798_f65d,
        0x00be_d234_c8b1_caf2,
        0xb273_d90c_0646_e446,
    ];

    #[test]
    fn frame_hashes() {
        let frames = frames();
        assert_eq!(frames.len(), FRAME_HASHES.len(), "a hash for every frame");
        for (at, (frame, input)) in frames.iter().enumerate() {
            assert_eq!(fnv(frame), FRAME_HASHES[at], "frame {at}, of {} bytes, as it was packed", input.len());
        }
    }

    #[test]
    fn text_and_noise_sizes() {
        let text = thread(3 * BLOCK);
        let frame = compress(&text);
        assert!(frame.len() * 3 < text.len(), "{} bytes of the thread in {}", text.len(), frame.len());
        assert!(
            compress_frame(&text, BLOCK).len() > frame.len(),
            "blocks past the places the table holds go without matches"
        );
        let noise = noise(300_000, 11);
        let blocks = noise.len().div_ceil(BLOCK);
        assert_eq!(
            compress(&noise).len(),
            4 + 1 + 4 + 3 * blocks + noise.len(),
            "the blocks as they are, behind their heads"
        );
        assert_eq!(
            compress(&[]),
            [0x28, 0xB5, 0x2F, 0xFD, 0x20, 0x00, 0x01, 0x00, 0x00],
            "nothing is one raw block of nothing"
        );
    }

    #[test]
    #[ignore = "has the host's zstd unpack every frame packed here"]
    fn zstd_unpacks_frames() {
        extern crate std;
        use std::io::Write;
        use std::process::{Command, Stdio};
        for (frame, input) in frames() {
            let mut child = Command::new("zstd")
                .args(["-d", "-c", "-q"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("zstd on the host");
            let mut stdin = child.stdin.take().expect("its input");
            let writer = std::thread::spawn(move || stdin.write_all(&frame));
            let output = child.wait_with_output().unwrap();
            writer.join().unwrap().unwrap();
            assert!(output.status.success(), "{} bytes: {}", input.len(), String::from_utf8_lossy(&output.stderr));
            assert!(output.stdout == input, "{} bytes come back as {}", input.len(), output.stdout.len());
        }
    }
}
