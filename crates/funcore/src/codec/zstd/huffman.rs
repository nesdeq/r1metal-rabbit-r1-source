use super::bits::Bits;
use super::fse::{self, Coder};
use alloc::vec;
use alloc::vec::Vec;

const MOST_BITS: u32 = 11;
const WEIGHTS_LOG: u32 = 6;
const DIRECT_LIMIT: usize = 128;
pub(super) const DIRECT: u8 = 127;
const WEIGHT_BITS: u32 = 4;
const CODED_LIMIT: usize = 128;
const STREAMS: usize = 4;

#[derive(Clone, Copy)]
enum Part {
    Byte(u8),
    Pair(u32, u32),
}

fn lengths(counts: &[u32; 256]) -> [u8; 256] {
    let mut bytes: Vec<(u64, u8)> = (0..=255u8)
        .filter(|&byte| counts[byte as usize] > 0)
        .map(|byte| (u64::from(counts[byte as usize]), byte))
        .collect();
    bytes.sort_unstable();
    let leaves: Vec<(u64, Part)> = bytes.iter().map(|&(weight, byte)| (weight, Part::Byte(byte))).collect();
    let mut levels = vec![leaves.clone()];
    for _ in 1..MOST_BITS {
        let previous = &levels[levels.len() - 1];
        let packages = (0..previous.len() / 2).map(|at| {
            let (first, second) = (2 * at, 2 * at + 1);
            (previous[first].0 + previous[second].0, Part::Pair(first as u32, second as u32))
        });
        let (mut leaves, mut packages) = (leaves.iter().copied().peekable(), packages.peekable());
        let mut level = Vec::with_capacity(leaves.len() + previous.len() / 2);
        loop {
            let is_leaf = match (leaves.peek(), packages.peek()) {
                (Some(leaf), Some(package)) => leaf.0 <= package.0,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            level.extend(if is_leaf { leaves.next() } else { packages.next() });
        }
        levels.push(level);
    }
    // The first 2n - 2 parts of the top level: every byte in them is one bit of its code.
    let top = levels.len() - 1;
    let mut lengths = [0u8; 256];
    let mut parts: Vec<(usize, u32)> = (0..2 * bytes.len() as u32 - 2).map(|at| (top, at)).collect();
    while let Some((level, at)) = parts.pop() {
        match levels[level][at as usize].1 {
            Part::Byte(byte) => lengths[byte as usize] += 1,
            Part::Pair(first, second) => parts.extend([(level - 1, first), (level - 1, second)]),
        }
    }
    lengths
}

fn codes(weights: &[u8; 256], max_length: u32) -> [(u32, u32); 256] {
    let mut start = [0u32; MOST_BITS as usize + 2];
    let mut next = 0;
    for weight in 1..=max_length {
        start[weight as usize] = next;
        next += weights.iter().filter(|&&w| u32::from(w) == weight).count() as u32 * (1 << (weight - 1));
    }
    let mut codes = [(0, 0); 256];
    for (byte, &weight) in weights.iter().enumerate() {
        if weight > 0 {
            let weight = u32::from(weight);
            codes[byte] = (start[weight as usize] >> (weight - 1), max_length + 1 - weight);
            start[weight as usize] += 1 << (weight - 1);
        }
    }
    codes
}

fn code_weights(weights: &[u8]) -> Option<Vec<u8>> {
    if weights.len() < 2 {
        return None;
    }
    let mut counts = [0u32; MOST_BITS as usize + 1];
    for &weight in weights {
        counts[weight as usize] += 1;
    }
    let last = counts.iter().rposition(|&count| count > 0)?;
    let distribution = fse::normalize(&counts[..=last], WEIGHTS_LOG)?;
    let mut coded = fse::describe(&distribution, WEIGHTS_LOG);
    coded.extend(fse::code_in_two_states(&Coder::new(&distribution, WEIGHTS_LOG), weights));
    Some(coded)
}

fn describe_weights(weights: &[u8]) -> Option<Vec<u8>> {
    let direct = (weights.len() <= DIRECT_LIMIT).then(|| {
        let mut description = vec![DIRECT + weights.len() as u8];
        description.extend(weights.chunks(2).map(|pair| pair[0] << WEIGHT_BITS | pair.get(1).copied().unwrap_or(0)));
        description
    });
    let coded = code_weights(weights).filter(|coded| coded.len() < CODED_LIMIT).map(|coded| {
        let mut description = vec![coded.len() as u8];
        description.extend(coded);
        description
    });
    [direct, coded].into_iter().flatten().min_by_key(Vec::len)
}

fn stream(literals: &[u8], codes: &[(u32, u32); 256]) -> Vec<u8> {
    let mut bits = Bits::default();
    for &literal in literals.iter().rev() {
        let (code, length) = codes[literal as usize];
        bits.add(u64::from(code), length);
    }
    bits.close()
}

pub(super) fn compress_literals(literals: &[u8], is_single: bool) -> Option<Vec<u8>> {
    let mut counts = [0u32; 256];
    for &literal in literals {
        counts[literal as usize] += 1;
    }
    if counts.iter().filter(|&&count| count > 0).count() < 2 {
        return None;
    }
    let lengths = lengths(&counts);
    let max_length = u32::from(*lengths.iter().max()?);
    let weights = lengths.map(|length| if length == 0 { 0 } else { (max_length + 1 - u32::from(length)) as u8 });
    // The last byte that has a weight is not told: its weight is what makes the others' sum a power of two.
    let last = weights.iter().rposition(|&weight| weight > 0)?;
    let mut coded = describe_weights(&weights[..last])?;
    let codes = codes(&weights, max_length);
    if is_single {
        coded.extend(stream(literals, &codes));
    } else {
        let streams: Vec<Vec<u8>> =
            literals.chunks(literals.len().div_ceil(STREAMS)).map(|part| stream(part, &codes)).collect();
        for stream in &streams[..STREAMS - 1] {
            coded.extend((stream.len() as u16).to_le_bytes());
        }
        streams.iter().for_each(|stream| coded.extend(stream));
    }
    Some(coded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_limited_and_complete() {
        // Counts that grow as the Fibonacci numbers do would want codes of up to 24 bits.
        let mut counts = [0u32; 256];
        let (mut a, mut b) = (1u32, 1u32);
        for count in counts.iter_mut().take(25) {
            *count = a;
            (a, b) = (b, a + b);
        }
        let lengths = lengths(&counts);
        assert_eq!(lengths.iter().copied().max(), Some(11), "no code is longer than eleven bits");
        let room: u32 =
            lengths.iter().filter(|&&length| length > 0).map(|&length| 1 << (MOST_BITS - u32::from(length))).sum();
        assert_eq!(room, 1 << MOST_BITS, "the code is whole: every leaf of a tree of eleven levels is some byte's");
        assert!(lengths[24] <= lengths[0], "the commonest byte has a code no longer than the rarest's");
        let mut even = [0u32; 256];
        even[b'a' as usize] = 5;
        even[b'b' as usize] = 5;
        let two = super::lengths(&even);
        assert_eq!((two[b'a' as usize], two[b'b' as usize]), (1, 1), "two bytes are a bit each");
    }

    #[test]
    fn rfc_codes() {
        let mut weights = [0u8; 256];
        weights[..6].copy_from_slice(&[4, 3, 2, 0, 1, 1]);
        let codes = codes(&weights, 4);
        assert_eq!(
            [codes[0], codes[1], codes[2], codes[4], codes[5]],
            [(0b1, 1), (0b01, 2), (0b001, 3), (0b0000, 4), (0b0001, 4)],
            "RFC 8878, table 25"
        );
        assert_eq!(
            stream(&[0, 1, 4, 5], &codes),
            [0b0000_0001, 0b0000_1101],
            "the literals 0145 by table 25's codes, which zstd reads; table 26 prints those of 4 and 5 the other way round"
        );
    }
}
