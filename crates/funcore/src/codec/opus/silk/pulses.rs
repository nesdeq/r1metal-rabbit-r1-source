use super::super::range::RangeDecoder;
use super::MOST_FRAME;
use super::indices::Signal;
use super::tables::{
    LSB, MOST_PULSES, PDF_BITS, PULSE_COUNT, RATE_LEVEL, SIGN, SIGN_COUNTS, SPLIT_2, SPLIT_4, SPLIT_8, SPLIT_16,
    SPLIT_ENTRIES, split_start,
};

// A shell block's samples (4.2.7.8).
const BLOCK: usize = 16;
const MOST_BLOCKS: usize = MOST_FRAME / BLOCK;
// The pulse count that tells of one more LSB, and the levels of Table 46 the count is then read with (4.2.7.8.2).
const MORE_LSBS: usize = MOST_PULSES + 1;
const LSB_LEVEL: usize = 9;
const LAST_LSB_LEVEL: usize = 10;
const MOST_LSBS: u32 = 10;
// The quantization offset types of each signal type in Table 52.
const OFFSET_TYPES: usize = 2;
// Tables 47 to 50: partitions of 16, 8, 4 and 2 samples.
const SPLITS: [&[u8; SPLIT_ENTRIES]; 4] = [&SPLIT_16, &SPLIT_8, &SPLIT_4, &SPLIT_2];

// The pulses of a frame's excitation, signed, with their LSBs (4.2.7.8.1 to 4.2.7.8.5); a frame of 10 ms at 12 kHz has 8 samples more than it plays.
pub(super) fn read(range: &mut RangeDecoder, signal: Signal, offset: usize, length: usize) -> [i32; MOST_FRAME] {
    let mut pulses = [0; MOST_FRAME];
    let blocks = length.div_ceil(BLOCK);
    let level = range.icdf(&RATE_LEVEL[usize::from(signal == Signal::Voiced)], PDF_BITS);
    let (mut counts, mut lsbs) = ([0; MOST_BLOCKS], [0; MOST_BLOCKS]);
    for (count, lsb) in counts[..blocks].iter_mut().zip(&mut lsbs) {
        *count = range.icdf(&PULSE_COUNT[level], PDF_BITS);
        while *count == MORE_LSBS {
            *lsb += 1;
            let level = if *lsb == MOST_LSBS { LAST_LSB_LEVEL } else { LSB_LEVEL };
            *count = range.icdf(&PULSE_COUNT[level], PDF_BITS);
        }
    }
    for (block, &count) in pulses.as_chunks_mut::<BLOCK>().0.iter_mut().zip(&counts[..blocks]) {
        place(range, block, count, 0);
    }
    for (block, &lsb) in pulses.as_chunks_mut::<BLOCK>().0.iter_mut().zip(&lsbs[..blocks]) {
        for pulse in block {
            for _ in 0..lsb {
                *pulse = *pulse << 1 | range.icdf(&LSB, PDF_BITS) as i32;
            }
        }
    }
    let signs = &SIGN[(signal as usize * OFFSET_TYPES + offset) * SIGN_COUNTS..][..SIGN_COUNTS];
    for ((block, &count), &lsb) in pulses.as_chunks_mut::<BLOCK>().0.iter_mut().zip(&counts[..blocks]).zip(&lsbs) {
        if count == 0 && lsb == 0 {
            continue;
        }
        let pdf = &signs[count.min(SIGN_COUNTS - 1)];
        for pulse in block.iter_mut().filter(|pulse| **pulse > 0) {
            if range.icdf(pdf, PDF_BITS) == 0 {
                *pulse = -*pulse;
            }
        }
    }
    pulses
}

// Places a partition's pulses, the left half's count first and then each half the same way, a partition `depth` halvings below a block (4.2.7.8.3).
fn place(range: &mut RangeDecoder, pulses: &mut [i32], count: usize, depth: usize) {
    if pulses.len() == 1 {
        pulses[0] = count as i32;
        return;
    }
    if count == 0 {
        return;
    }
    let left = range.icdf(&SPLITS[depth][split_start(count)..][..count + 1], PDF_BITS);
    let (low, high) = pulses.split_at_mut(pulses.len() / 2);
    place(range, low, left, depth + 1);
    place(range, high, count - left, depth + 1);
}

#[cfg(test)]
mod tests {
    use super::super::super::range::RangeEncoder;
    use super::*;

    // The iCDF symbol of a table, written.
    fn put(encoder: &mut RangeEncoder, table: &[u8], symbol: usize) {
        encoder.icdf(symbol, table, PDF_BITS);
    }

    #[test]
    fn shell_blocks_of_table_44() {
        let blocks = |khz: usize, ms: usize| (khz * ms).div_ceil(BLOCK);
        assert_eq!(
            [blocks(8, 10), blocks(12, 10), blocks(16, 10), blocks(8, 20), blocks(12, 20), blocks(16, 20)],
            [5, 8, 10, 10, 15, 20],
            "the shell blocks of a frame of each bandwidth and size"
        );
    }

    #[test]
    fn pulses_lsbs_and_signs() {
        let mut encoder = RangeEncoder::new(64);
        put(&mut encoder, &RATE_LEVEL[0], 3);
        // Block 0 holds 3 pulses, block 1 one pulse and two LSBs.
        put(&mut encoder, &PULSE_COUNT[3], 3);
        put(&mut encoder, &PULSE_COUNT[3], MORE_LSBS);
        put(&mut encoder, &PULSE_COUNT[LSB_LEVEL], MORE_LSBS);
        put(&mut encoder, &PULSE_COUNT[LSB_LEVEL], 1);
        // Block 0: 2 of 3 in the left 8, both in its left 4, one in each half of that, at samples 0 and 3; the right 8's one is in its left 4, at sample 9.
        for (table, count, left) in [(&SPLIT_16, 3, 2), (&SPLIT_8, 2, 2), (&SPLIT_4, 2, 1), (&SPLIT_2, 1, 1)] {
            put(&mut encoder, &table[split_start(count)..][..count + 1], left);
        }
        put(&mut encoder, &SPLIT_2[split_start(1)..][..2], 0);
        for (table, count, left) in [(&SPLIT_8, 1, 1), (&SPLIT_4, 1, 1), (&SPLIT_2, 1, 0)] {
            put(&mut encoder, &table[split_start(count)..][..count + 1], left);
        }
        // Block 1: its pulse at sample 15.
        for table in [&SPLIT_16, &SPLIT_8, &SPLIT_4, &SPLIT_2] {
            put(&mut encoder, &table[split_start(1)..][..2], 0);
        }
        // Two LSBs for each of block 1's samples: 1 then 0 at sample 0, 0 then 1 elsewhere.
        for sample in 0..BLOCK {
            put(&mut encoder, &LSB, usize::from(sample == 0));
            put(&mut encoder, &LSB, usize::from(sample != 0));
        }
        // Signs: block 0's pulses at 0, 3 and 9 are negative, positive, negative; block 1's samples are all positive.
        for sign in [0, 1, 0] {
            put(&mut encoder, &SIGN[3], sign);
        }
        for _ in 0..BLOCK {
            put(&mut encoder, &SIGN[1], 1);
        }
        let frame = encoder.finish().unwrap();
        let mut range = RangeDecoder::new(frame);
        let pulses = read(&mut range, Signal::Inactive, 0, 2 * BLOCK);
        let mut expected = [0; 2 * BLOCK];
        expected[..10].copy_from_slice(&[-1, 0, 0, 1, 0, 0, 0, 0, 0, -1]);
        expected[BLOCK] = 0b10;
        expected[BLOCK + 1..2 * BLOCK - 1].fill(1);
        expected[2 * BLOCK - 1] = 1 << 2 | 0b01;
        assert_eq!(
            pulses[..2 * BLOCK],
            expected,
            "pulses placed by halves, LSBs below them, signs by each block's count"
        );
        assert!(pulses[2 * BLOCK..].iter().all(|&pulse| pulse == 0));
    }

    #[test]
    fn lsbs_end_at_ten() {
        let mut encoder = RangeEncoder::new(64);
        put(&mut encoder, &RATE_LEVEL[1], 0);
        put(&mut encoder, &PULSE_COUNT[0], MORE_LSBS);
        for _ in 1..MOST_LSBS {
            put(&mut encoder, &PULSE_COUNT[LSB_LEVEL], MORE_LSBS);
        }
        put(&mut encoder, &PULSE_COUNT[LAST_LSB_LEVEL], 0);
        for _ in 0..BLOCK * MOST_LSBS as usize {
            put(&mut encoder, &LSB, 1);
        }
        for _ in 0..BLOCK {
            put(&mut encoder, &SIGN[SIGN_COUNTS * OFFSET_TYPES * Signal::Voiced as usize], 1);
        }
        let frame = encoder.finish().unwrap();
        let pulses = read(&mut RangeDecoder::new(frame), Signal::Voiced, 0, BLOCK);
        assert_eq!(pulses[..BLOCK], [(1 << MOST_LSBS) - 1; BLOCK], "ten LSBs, and level 10 has no more of them");
        assert_eq!(PULSE_COUNT[LAST_LSB_LEVEL][MORE_LSBS - 1], 0, "level 10 cannot tell of an eleventh");
    }
}
