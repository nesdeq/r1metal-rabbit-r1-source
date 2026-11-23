use super::tables::{SCALEFACTORS, split};
use super::transform::Transform;
use super::{
    BAND_COUNT_BITS, BANDS, BOOK_BITS, BOOKS, CODE_BOOKS, ELEMENT_BITS, END_ELEMENT, ESCAPE, ESCAPE_BITS, FRAME,
    GAIN_BITS, LARGEST_PACKET, LARGEST_VALUE, RATE, RUN_BITS, RUN_ESCAPE, SCALE_REACH, SCALES, SEQUENCE_BITS,
    SINGLE_CHANNEL, TAG_BITS, UNIT_SCALE, ZERO_BOOK, band, escape_ones, power,
};
use crate::base::bits::BitWriter;
use crate::base::math::dot;
use crate::base::math::log2;
use crate::base::math::sqrt;
use alloc::vec;
use alloc::vec::Vec;

const CODED_BANDS: usize = 42;
const ROUNDING: f32 = 0.4054;
const STEPS_PER_OCTAVE: f32 = 4.0 / 3.0;
const QUALITY: f32 = 24.0;
const SHRINK: i32 = 3;
const DEBT_PER_STEP: i64 = 2;
const CREDIT: i64 = 8;
const LEAP: u32 = 8;
const FIXED_BITS: u32 = ELEMENT_BITS + TAG_BITS + GAIN_BITS + WINDOW_BITS + TOOL_FLAGS + ELEMENT_BITS;
const WINDOW_BITS: u32 = 1 + SEQUENCE_BITS + 1 + BAND_COUNT_BITS + 1;
const TOOL_FLAGS: u32 = 3;
const SECTION_BITS: u32 = BOOK_BITS + RUN_BITS;
const NEVER: u32 = u32::MAX / 2;

type Costs = [u32; BOOKS + 1];

pub(crate) struct Encoder {
    transform: Transform,
    previous: Vec<f32>,
    current: Vec<f32>,
    spectrum: Vec<f32>,
    power_lines: Vec<f32>,
    lines: Vec<i32>,
    levels: [f32; CODED_BANDS],
    scales: [u8; CODED_BANDS],
    books: [u8; CODED_BANDS],
    frame_bits: i64,
    debt: i64,
}

fn book_bits(book: usize, lines: &[i32]) -> u32 {
    let book = &CODE_BOOKS[book - 1];
    lines
        .chunks_exact(book.group)
        .map(|group| {
            let signs = if book.is_signed { 0 } else { group.iter().filter(|&&line| line != 0).count() as u32 };
            let escapes: u32 = group
                .iter()
                .map(|line| line.unsigned_abs())
                .filter(|&size| book.largest == ESCAPE && size >= ESCAPE)
                .map(|size| 2 * escape_ones(size) + 1 + ESCAPE_BITS)
                .sum();
            book.code(book.index(group)).1 + signs + escapes
        })
        .sum()
}

fn costs(lines: &[i32]) -> Costs {
    let largest = lines.iter().map(|line| line.unsigned_abs()).max().unwrap_or(0);
    core::array::from_fn(|book| match book {
        ZERO_BOOK => {
            if largest == 0 {
                0
            } else {
                NEVER
            }
        }
        _ if largest > CODE_BOOKS[book - 1].largest && book < BOOKS => NEVER,
        _ => book_bits(book, lines) + u32::from(largest == 0),
    })
}

fn coded_scale(lines: &[i32], scale: u8, last: i32) -> i32 {
    if lines.iter().any(|&line| line != 0) {
        i32::from(scale).clamp(last - SCALE_REACH, last + SCALE_REACH)
    } else {
        last
    }
}

fn global_gain(lines: &[i32], scales: &[u8], bands: usize) -> i32 {
    (0..bands)
        .find(|&number| lines[band(number)].iter().any(|&line| line != 0))
        .map_or(0, |number| i32::from(scales[number]))
}

pub(super) fn pack(lines: &[i32], scales: &[u8], books: &[u8], bands: usize, packet: &mut Vec<u8>) {
    let mut bits = BitWriter::with_bytes(core::mem::take(packet));
    let gain = global_gain(lines, scales, bands);
    bits.put(SINGLE_CHANNEL, ELEMENT_BITS);
    bits.put(0, TAG_BITS);
    bits.put(gain as u32, GAIN_BITS);
    bits.put(0, 1 + SEQUENCE_BITS + 1);
    bits.put(bands as u32, BAND_COUNT_BITS);
    bits.put(0, 1);
    let mut at = 0;
    while at < bands {
        let mut run = books[at..bands].iter().take_while(|&&book| book == books[at]).count() as u32;
        bits.put(u32::from(books[at]), BOOK_BITS);
        at += run as usize;
        while run >= RUN_ESCAPE {
            bits.put(RUN_ESCAPE, RUN_BITS);
            run -= RUN_ESCAPE;
        }
        bits.put(run, RUN_BITS);
    }
    let coded_bands = || (0..bands).filter(|&number| usize::from(books[number]) != ZERO_BOOK);
    let mut last = gain;
    for number in coded_bands() {
        let scale = coded_scale(&lines[band(number)], scales[number], last);
        let (code, length) = split(SCALEFACTORS[(scale - last + SCALE_REACH) as usize]);
        bits.put(code, length);
        last = scale;
    }
    bits.put(0, TOOL_FLAGS);
    for number in coded_bands() {
        let book = &CODE_BOOKS[usize::from(books[number]) - 1];
        for group in lines[band(number)].chunks_exact(book.group) {
            let (code, length) = book.code(book.index(group));
            bits.put(code, length);
            if !book.is_signed {
                group.iter().filter(|&&line| line != 0).for_each(|&line| bits.put(u32::from(line < 0), 1));
            }
            for size in
                group.iter().map(|line| line.unsigned_abs()).filter(|&size| book.largest == ESCAPE && size >= ESCAPE)
            {
                let ones = escape_ones(size);
                bits.put((1 << ones) - 1, ones);
                bits.put(0, 1);
                bits.put(size & ((1 << (ones + ESCAPE_BITS)) - 1), ones + ESCAPE_BITS);
            }
        }
    }
    bits.put(END_ELEMENT, ELEMENT_BITS);
    *packet = bits.finish();
}

impl Encoder {
    pub(crate) fn new(bits_per_second: u32) -> Encoder {
        Encoder {
            transform: Transform::new(),
            previous: vec![0.0; FRAME],
            current: vec![0.0; FRAME],
            spectrum: vec![0.0; FRAME],
            power_lines: vec![0.0; FRAME],
            lines: vec![0; FRAME],
            levels: [0.0; CODED_BANDS],
            scales: [0; CODED_BANDS],
            books: [0; CODED_BANDS],
            frame_bits: i64::from(bits_per_second) * FRAME as i64 / i64::from(RATE),
            debt: 0,
        }
    }

    pub(crate) fn encode(&mut self, samples: &[i16], packet: &mut Vec<u8>) {
        packet.clear();
        for (to, &sample) in self.current.iter_mut().zip(samples) {
            *to = f32::from(sample);
        }
        self.transform.forward(&self.previous, &self.current, &mut self.spectrum);
        core::mem::swap(&mut self.previous, &mut self.current);
        for (power_line, line) in self.power_lines.iter_mut().zip(&self.spectrum) {
            let size = line.abs();
            *power_line = sqrt(size * sqrt(size));
        }
        let mut loudest = f32::MIN;
        for (number, level) in self.levels.iter_mut().enumerate() {
            let lines = &self.spectrum[band(number)];
            let mean = dot(lines, lines) / lines.len() as f32;
            *level = if mean < f32::MIN_POSITIVE { f32::MIN } else { STEPS_PER_OCTAVE / 2.0 * log2(mean) };
            loudest = loudest.max(*level);
        }
        let mut coarseness = (self.debt.max(0) / (DEBT_PER_STEP * self.frame_bits.max(1))) as i32;
        let (bands, bits) = loop {
            let (bands, bits) = self.plan(loudest, coarseness);
            let limit = (u8::BITS as usize * LARGEST_PACKET) as u32;
            if bits <= limit {
                break (bands, bits);
            }
            coarseness += 1 + (LEAP * (bits - limit) / bits) as i32;
        };
        self.debt = (self.debt + i64::from(bits) - self.frame_bits).max(-CREDIT * self.frame_bits);
        pack(&self.lines, &self.scales, &self.books, bands, packet);
    }

    fn quantize(&mut self, number: usize, scale: i32) -> u32 {
        let shrink = power(-SHRINK * (scale - UNIT_SCALE));
        let mut largest = 0;
        for at in band(number) {
            let size = (self.power_lines[at] * shrink + ROUNDING) as u32;
            largest = largest.max(size);
            let size = size.min(LARGEST_VALUE) as i32;
            self.lines[at] = if self.spectrum[at] < 0.0 { -size } else { size };
        }
        largest
    }

    fn plan(&mut self, loudest: f32, coarseness: i32) -> (usize, u32) {
        let mut bands = 0;
        for number in 0..CODED_BANDS {
            let level = self.levels[number].max(loudest - SCALE_REACH as f32);
            let target = UNIT_SCALE as f32 + QUALITY + level + coarseness as f32 + 0.5;
            let mut scale = (target as i32).clamp(*SCALES.start(), *SCALES.end());
            let mut largest = self.quantize(number, scale);
            while largest > LARGEST_VALUE && scale < *SCALES.end() {
                scale += 1;
                largest = self.quantize(number, scale);
            }
            self.scales[number] = scale as u8;
            if largest > 0 {
                bands = number + 1;
            }
        }
        let mut band_costs = [[NEVER; BOOKS + 1]; CODED_BANDS];
        let mut sources = [[0u8; BOOKS + 1]; CODED_BANDS];
        let mut best: Costs = [NEVER; BOOKS + 1];
        for number in 0..bands {
            band_costs[number] = costs(&self.lines[band(number)]);
            let (cheapest, least) =
                (0..=BOOKS).map(|book| (book, best[book])).min_by_key(|&(_, bits)| bits).unwrap_or((0, 0));
            let new_section = if number == 0 { SECTION_BITS } else { least + SECTION_BITS };
            best = core::array::from_fn(|book| {
                let (cost, source) =
                    if number > 0 && best[book] <= new_section { (best[book], book) } else { (new_section, cheapest) };
                sources[number][book] = source as u8;
                (cost + band_costs[number][book]).min(NEVER)
            });
        }
        let mut book = (0..=BOOKS).min_by_key(|&book| best[book]).unwrap_or(ZERO_BOOK);
        for number in (0..bands).rev() {
            self.books[number] = book as u8;
            book = usize::from(sources[number][book]);
        }
        let mut bits = FIXED_BITS;
        let mut last = global_gain(&self.lines, &self.scales, bands);
        for (number, costs) in band_costs.iter().enumerate().take(bands) {
            let book = usize::from(self.books[number]);
            bits += costs[book];
            if number == 0 || self.books[number - 1] != self.books[number] {
                let run =
                    self.books[number..bands].iter().take_while(|&&other| usize::from(other) == book).count() as u32;
                bits += BOOK_BITS + RUN_BITS * (run / RUN_ESCAPE + 1);
            }
            let lines = &self.lines[band(number)];
            if book != ZERO_BOOK && lines.iter().any(|&line| line != 0) {
                let scale = coded_scale(lines, self.scales[number], last);
                bits += split(SCALEFACTORS[(scale - last + SCALE_REACH) as usize]).1;
                last = scale;
            }
        }
        (bands, bits)
    }
}

const _: () = assert!(CODED_BANDS <= BANDS);

#[cfg(test)]
pub(super) mod samples {
    use super::*;
    use crate::base::math::{next_random, sine_cosine};

    // Frames of tones, each a frequency and a size, under noise of that size.
    pub(crate) fn sound(frames: usize, tones: &[(f64, f64)], hiss: f64) -> Vec<i16> {
        let mut state = 1u32;
        (0..frames * FRAME)
            .map(|at| {
                let noise = (f64::from(next_random(&mut state) >> 8) / f64::from(1u32 << 23) - 1.0) * hiss;
                let tone = |&(hertz, size): &(f64, f64)| size * sine_cosine(hertz * at as f64 / f64::from(RATE)).0;
                (tones.iter().map(tone).sum::<f64>() + noise) as i16
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::super::{DELAY, Decoder};
    use super::samples::sound;
    use super::*;

    fn round_trip(samples: &[i16], bits_per_second: u32) -> (Vec<i16>, Vec<usize>) {
        let (mut encoder, mut decoder) = (Encoder::new(bits_per_second), Decoder::new());
        let (mut packet, mut output, mut sizes) = (Vec::new(), vec![0i16; samples.len()], Vec::new());
        for (frame, out) in samples.as_chunks::<FRAME>().0.iter().zip(output.as_chunks_mut::<FRAME>().0) {
            encoder.encode(frame, &mut packet);
            decoder.decode(&packet, out).unwrap();
            sizes.push(packet.len());
        }
        (output, sizes)
    }

    fn snr_db(samples: &[i16], output: &[i16]) -> f64 {
        let (mut signal, mut noise) = (0.0f64, 0.0f64);
        for (&input, &result) in samples.iter().zip(&output[DELAY..]).skip(FRAME) {
            signal += f64::from(input) * f64::from(input);
            noise += (f64::from(input) - f64::from(result)).powi(2);
        }
        10.0 * (signal / noise.max(1.0)).log10()
    }

    #[test]
    fn stream_round_trip_snr() {
        let samples = sound(40, &[(440.0, 8000.0), (1000.0, 4000.0), (3100.0, 2000.0), (9000.0, 500.0)], 100.0);
        let (output, sizes) = round_trip(&samples, 128_000);
        assert!(snr_db(&samples, &output) > 30.0, "{} dB", snr_db(&samples, &output));
        assert!(sizes.iter().all(|&bytes| bytes <= LARGEST_PACKET));
        let (coarse, _) = round_trip(&samples, 24_000);
        assert!(snr_db(&samples, &coarse) > 15.0, "{} dB at a fifth of the bits", snr_db(&samples, &coarse));
        assert!(snr_db(&samples, &coarse) < snr_db(&samples, &output));
    }

    #[test]
    fn silence_four_bytes() {
        let (output, sizes) = round_trip(&[0; 4 * FRAME], 64_000);
        assert_eq!(sizes, [4; 4]);
        assert!(output.iter().all(|&sample| sample == 0));
    }

    #[test]
    fn packets_keep_rate() {
        let samples = sound(300, &[(300.0, 3000.0), (2500.0, 1500.0)], 3000.0);
        for rate in [32_000u32, 64_000, 96_000] {
            let (_, sizes) = round_trip(&samples, rate);
            let steady = &sizes[100..];
            let actual = steady.iter().sum::<usize>() as f64 * 8.0 * f64::from(RATE) / (steady.len() * FRAME) as f64;
            assert!((actual / f64::from(rate) - 1.0).abs() < 0.1, "{actual} bits a second for {rate}");
        }
    }

    #[test]
    fn loudest_noise_fits_packet() {
        let samples = sound(12, &[], 32_000.0);
        let (output, sizes) = round_trip(&samples, 512_000);
        assert!(sizes.iter().all(|&bytes| bytes <= LARGEST_PACKET), "{sizes:?}");
        assert!(sizes.iter().skip(1).all(|&bytes| bytes > LARGEST_PACKET / 2), "{sizes:?}");
        assert!(snr_db(&samples, &output) > 3.0);
    }
}
