// A systematic Reed-Solomon code over GF(2^16) on 16 interleaved polynomials.

use crate::base::proto;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use alloc::vec::Vec;

const MODULUS: u32 = 0x1100B;
const POLYNOMIALS: usize = 16;
const CHUNK_BYTES: usize = 2 * POLYNOMIALS;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Chunk {
    index: u16,
    data: [u8; CHUNK_BYTES],
}

fn multiply(a: u16, b: u16) -> u16 {
    let mut product = 0u32;
    for bit in 0..16 {
        if b >> bit & 1 == 1 {
            product ^= u32::from(a) << bit;
        }
    }
    for bit in (16..32).rev() {
        if product >> bit & 1 == 1 {
            product ^= MODULUS << (bit - 16);
        }
    }
    product as u16
}

fn inverse(element: u16) -> u16 {
    let mut result = 1;
    for bit in (0..16).rev() {
        result = multiply(result, result);
        if (0xFFFEu16 >> bit) & 1 == 1 {
            result = multiply(result, element);
        }
    }
    result
}

fn interpolate(points: &[(u16, u16)], at: u16) -> u16 {
    let mut value = 0;
    for (index, &(x, y)) in points.iter().enumerate() {
        let (mut numerator, mut denominator) = (1, 1);
        for (other, &(other_x, _)) in points.iter().enumerate() {
            if other != index {
                numerator = multiply(numerator, at ^ other_x);
                denominator = multiply(denominator, x ^ other_x);
            }
        }
        value ^= multiply(y, multiply(numerator, inverse(denominator)));
    }
    value
}

fn point_count(symbols: usize, polynomial: usize) -> usize {
    symbols / POLYNOMIALS + usize::from(polynomial < symbols % POLYNOMIALS)
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Encoder {
    object: Vec<u8>,
    next: u32,
}

impl Encoder {
    pub(super) fn new(object: &[u8]) -> Option<Encoder> {
        (object.len().is_multiple_of(2) && object.len() <= (1 << 16) * CHUNK_BYTES)
            .then(|| Encoder { object: object.to_vec(), next: 0 })
    }

    fn symbol(&self, at: usize) -> u16 {
        u16::from_be_bytes([self.object[2 * at], self.object[2 * at + 1]])
    }

    fn chunk_at(&self, index: u16) -> Chunk {
        let symbols = self.object.len() / 2;
        let mut data = [0u8; CHUNK_BYTES];
        for polynomial in 0..POLYNOMIALS {
            let count = point_count(symbols, polynomial);
            let value = if usize::from(index) < count {
                self.symbol(usize::from(index) * POLYNOMIALS + polynomial)
            } else {
                let points: Vec<(u16, u16)> =
                    (0..count).map(|x| (x as u16, self.symbol(x * POLYNOMIALS + polynomial))).collect();
                interpolate(&points, index)
            };
            data[2 * polynomial..2 * polynomial + 2].copy_from_slice(&value.to_be_bytes());
        }
        Chunk { index, data }
    }

    pub(super) fn next_chunk(&mut self) -> Chunk {
        let chunk = self.chunk_at(self.next as u16);
        self.next = self.next.wrapping_add(1);
        chunk
    }

    pub(crate) fn write(&self) -> Vec<u8> {
        Writer::new().bytes(1, &self.object).varint(2, u64::from(self.next)).finish()
    }

    pub(crate) fn read(bytes: &[u8]) -> Option<Encoder> {
        let message = Message::parse(bytes)?;
        let mut encoder = Encoder::new(message.bytes(1)?)?;
        encoder.next = message.u32(2)?;
        Some(encoder)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Decoder {
    length: usize,
    points: [Vec<(u16, u16)>; POLYNOMIALS],
}

impl Decoder {
    pub(super) fn new(length: usize) -> Option<Decoder> {
        length.is_multiple_of(2).then(|| Decoder { length, points: Default::default() })
    }

    pub(super) fn length(&self) -> usize {
        self.length
    }

    fn required_points(&self, polynomial: usize) -> usize {
        point_count(self.length / 2, polynomial)
    }

    pub(crate) fn add(&mut self, chunk: &Chunk) {
        for polynomial in 0..POLYNOMIALS {
            let required = self.required_points(polynomial);
            let x = chunk.index;
            let y = u16::from_be_bytes([chunk.data[2 * polynomial], chunk.data[2 * polynomial + 1]]);
            let points = &mut self.points[polynomial];
            if (usize::from(x) < required || points.len() < required)
                && let Err(at) = points.binary_search_by_key(&x, |&(x, _)| x)
            {
                points.insert(at, (x, y));
            }
        }
    }

    pub(super) fn decode(&self) -> Option<Vec<u8>> {
        if (0..POLYNOMIALS).any(|polynomial| self.points[polynomial].len() < self.required_points(polynomial)) {
            return None;
        }
        let mut object = Vec::with_capacity(self.length);
        for at in 0..self.length / 2 {
            let (polynomial, x) = (at % POLYNOMIALS, (at / POLYNOMIALS) as u16);
            let points = &self.points[polynomial];
            let y = match points.binary_search_by_key(&x, |&(x, _)| x) {
                Ok(position) => points[position].1,
                Err(_) => interpolate(&points[..self.required_points(polynomial)], x),
            };
            object.extend_from_slice(&y.to_be_bytes());
        }
        Some(object)
    }

    pub(crate) fn write(&self) -> Vec<u8> {
        let mut writer = Writer::new().varint(1, self.length as u64);
        for points in &self.points {
            let bytes: Vec<u8> =
                points.iter().flat_map(|(x, y)| x.to_be_bytes().into_iter().chain(y.to_be_bytes())).collect();
            writer = writer.bytes(2, &bytes);
        }
        writer.finish()
    }

    pub(crate) fn read(bytes: &[u8]) -> Option<Decoder> {
        let message = Message::parse(bytes)?;
        let mut decoder = Decoder::new(usize::try_from(message.varint(1)?).ok()?)?;
        let lists: Vec<&[u8]> = message.repeated(2).collect();
        if lists.len() != POLYNOMIALS {
            return None;
        }
        for (points, bytes) in decoder.points.iter_mut().zip(lists) {
            if bytes.len() % 4 != 0 {
                return None;
            }
            *points = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|point| (u16::from_be_bytes([point[0], point[1]]), u16::from_be_bytes([point[2], point[3]])))
                .collect();
        }
        Some(decoder)
    }
}

pub(super) fn write_chunk(chunk: &Chunk, out: &mut Vec<u8>) {
    proto::write_varint(out, u64::from(chunk.index));
    out.extend_from_slice(&chunk.data);
}

pub(super) fn read_chunk(bytes: &[u8]) -> Option<Chunk> {
    let (index, length) = proto::read_varint(bytes)?;
    let data = bytes.get(length..length + CHUNK_BYTES)?.try_into().ok()?;
    Some(Chunk { index: u16::try_from(index).ok()?, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_arithmetic() {
        assert_eq!(multiply(0x8000, 2), 0x100B, "x^16 is x^12 + x^3 + x + 1");
        for element in [1u16, 2, 3, 0x1234, 0xFFFF, 0x8000] {
            assert_eq!(multiply(element, inverse(element)), 1);
        }
        assert_eq!(multiply(0x1234, 0x5678), multiply(0x5678, 0x1234));
    }

    fn object(length: usize) -> Vec<u8> {
        (0..length).map(|at| (at * 7 + 3) as u8).collect()
    }

    #[test]
    fn systematic_chunks() {
        let ciphertext = object(960);
        let mut encoder = Encoder::new(&ciphertext).unwrap();
        for index in 0..30 {
            let chunk = encoder.next_chunk();
            assert_eq!(chunk.index, index);
            assert_eq!(&chunk.data[..], &ciphertext[32 * usize::from(index)..32 * usize::from(index) + 32]);
        }
    }

    #[test]
    fn rebuild_from_any_chunks() {
        for length in [96, 160, 960, 1152, 50] {
            let original = object(length);
            let encoder = Encoder::new(&original).unwrap();
            let count = length.div_ceil(CHUNK_BYTES);
            for skip in [0, 1, count / 2, count] {
                let mut decoder = Decoder::new(length).unwrap();
                let indices: Vec<u16> = (0..count as u16 + skip as u16)
                    .filter(|index| usize::from(*index) >= skip || *index % 2 == 1)
                    .take(count)
                    .collect();
                for &index in &indices[..indices.len() - 1] {
                    decoder.add(&encoder.chunk_at(index));
                    assert_eq!(decoder.decode(), None);
                }
                decoder.add(&encoder.chunk_at(*indices.last().unwrap()));
                assert_eq!(decoder.decode(), Some(original.clone()), "{length} bytes, {skip} skipped");
            }
        }
    }

    #[test]
    fn parity_on_polynomials() {
        let original = object(96);
        let encoder = Encoder::new(&original).unwrap();
        let parity = encoder.chunk_at(7);
        for polynomial in 0..POLYNOMIALS {
            let points: Vec<(u16, u16)> = (0..3u16)
                .map(|x| {
                    let at = 2 * (usize::from(x) * POLYNOMIALS + polynomial);
                    (x, u16::from_be_bytes([original[at], original[at + 1]]))
                })
                .collect();
            let mut coefficients = [0u16; 3];
            for (index, &(x, y)) in points.iter().enumerate() {
                let others: Vec<u16> =
                    points.iter().enumerate().filter(|(other, _)| *other != index).map(|(_, p)| p.0).collect();
                let scale = multiply(y, inverse(multiply(x ^ others[0], x ^ others[1])));
                coefficients[0] ^= multiply(scale, multiply(others[0], others[1]));
                coefficients[1] ^= multiply(scale, others[0] ^ others[1]);
                coefficients[2] ^= scale;
            }
            let at = 7u16;
            let value = coefficients[0] ^ multiply(coefficients[1], at) ^ multiply(coefficients[2], multiply(at, at));
            assert_eq!(u16::from_be_bytes([parity.data[2 * polynomial], parity.data[2 * polynomial + 1]]), value);
        }
    }

    #[test]
    fn chunk_bytes() {
        let encoder = Encoder::new(&object(160)).unwrap();
        let mut out = Vec::new();
        write_chunk(&encoder.chunk_at(300), &mut out);
        assert_eq!(out.len(), 2 + CHUNK_BYTES);
        assert_eq!(read_chunk(&out), Some(encoder.chunk_at(300)));
    }
}
