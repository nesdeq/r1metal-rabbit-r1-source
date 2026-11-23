use alloc::vec;
use alloc::vec::Vec;

const MODULUS: u16 = 0x11D;
const ORDER: usize = 255;

struct Tables {
    powers: [u8; 2 * ORDER],
    logarithms: [u8; ORDER + 1],
}

const TABLES: Tables = {
    let (mut powers, mut logarithms) = ([0u8; 2 * ORDER], [0u8; ORDER + 1]);
    let (mut value, mut power) = (1u16, 0);
    while power < ORDER {
        powers[power] = value as u8;
        powers[power + ORDER] = value as u8;
        logarithms[value as usize] = power as u8;
        value <<= 1;
        if value > 0xFF {
            value ^= MODULUS;
        }
        power += 1;
    }
    Tables { powers, logarithms }
};

const fn power(exponent: usize) -> u8 {
    TABLES.powers[exponent % ORDER]
}

const fn logarithm(value: u8) -> usize {
    TABLES.logarithms[value as usize] as usize
}

fn multiply(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 { 0 } else { TABLES.powers[logarithm(a) + logarithm(b)] }
}

fn divide(a: u8, b: u8) -> u8 {
    if a == 0 { 0 } else { TABLES.powers[logarithm(a) + ORDER - logarithm(b)] }
}

fn eval_high_first(coefficients: &[u8], x: u8) -> u8 {
    coefficients.iter().fold(0, |sum, &coefficient| multiply(sum, x) ^ coefficient)
}

fn eval_low_first(coefficients: &[u8], x: u8) -> u8 {
    coefficients.iter().rev().fold(0, |sum, &coefficient| multiply(sum, x) ^ coefficient)
}

pub(super) fn divisor(degree: usize) -> Vec<u8> {
    let mut product = vec![0u8; degree];
    product[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree {
        for at in 0..degree {
            product[at] = multiply(product[at], root);
            if let Some(&next) = product.get(at + 1) {
                product[at] ^= next;
            }
        }
        root = multiply(root, 2);
    }
    product
}

pub(super) fn remainder(data: &[u8], divisor: &[u8]) -> Vec<u8> {
    let mut register = vec![0u8; divisor.len()];
    for &byte in data {
        let factor = byte ^ register.remove(0);
        register.push(0);
        for (place, &coefficient) in register.iter_mut().zip(divisor) {
            *place ^= multiply(coefficient, factor);
        }
    }
    register
}

fn locator(syndromes: &[u8]) -> Vec<u8> {
    let size = syndromes.len() + 1;
    let (mut current, mut previous) = (vec![0u8; size], vec![0u8; size]);
    (current[0], previous[0]) = (1, 1);
    let (mut length, mut shift, mut last_discrepancy) = (0, 1, 1u8);
    for step in 0..syndromes.len() {
        let discrepancy = (0..=length).fold(0, |sum, at| sum ^ multiply(current[at], syndromes[step - at]));
        if discrepancy == 0 {
            shift += 1;
            continue;
        }
        let (factor, copy) = (divide(discrepancy, last_discrepancy), current.clone());
        for at in 0..size - shift {
            current[at + shift] ^= multiply(factor, previous[at]);
        }
        if 2 * length <= step {
            (length, previous, last_discrepancy, shift) = (step + 1 - length, copy, discrepancy, 1);
        } else {
            shift += 1;
        }
    }
    current.truncate(length + 1);
    current
}

pub(super) fn correct(block: &mut [u8], parity: usize) -> bool {
    let syndromes: Vec<u8> = (0..parity).map(|at| eval_high_first(block, power(at))).collect();
    if syndromes.iter().all(|&syndrome| syndrome == 0) {
        return true;
    }
    let locator = locator(&syndromes);
    let errors = locator.len() - 1;
    if 2 * errors > parity || block.len() > ORDER {
        return false;
    }
    // The codeword at `at` is the coefficient of x to the power of the codewords after it.
    let places: Vec<usize> = (0..block.len())
        .filter(|&at| eval_low_first(&locator, power(ORDER - (block.len() - 1 - at) % ORDER)) == 0)
        .collect();
    if places.len() != errors {
        return false;
    }
    // Forney: the evaluator is the syndromes times the locator to the parity's degree, and the locator's derivative keeps its odd terms.
    let mut evaluator = vec![0u8; parity];
    for (at, &syndrome) in syndromes.iter().enumerate() {
        for (offset, &coefficient) in locator.iter().enumerate().take(parity - at) {
            evaluator[at + offset] ^= multiply(syndrome, coefficient);
        }
    }
    let derivative: Vec<u8> = locator
        .iter()
        .enumerate()
        .skip(1)
        .map(|(at, &coefficient)| if at % 2 == 1 { coefficient } else { 0 })
        .collect();
    let mut output = block.to_vec();
    for &at in &places {
        let place = power(block.len() - 1 - at);
        let inverse = divide(1, place);
        let denominator = eval_low_first(&derivative, inverse);
        if denominator == 0 {
            return false;
        }
        output[at] ^= divide(multiply(place, eval_low_first(&evaluator, inverse)), denominator);
    }
    if (0..parity).any(|at| eval_high_first(&output, power(at)) != 0) {
        return false;
    }
    block.copy_from_slice(&output);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(length: usize, parity: usize, seed: usize) -> Vec<u8> {
        let data: Vec<u8> = (0..length).map(|at| ((at * 131 + seed * 977 + at * at * 7) % 256) as u8).collect();
        [data.clone(), remainder(&data, &divisor(parity))].concat()
    }

    #[test]
    fn field_tables() {
        let mut visited = [false; 256];
        for at in 0..ORDER {
            assert!(!visited[usize::from(power(at))], "2 to the {at} came before");
            visited[usize::from(power(at))] = true;
        }
        assert!(!visited[0] && visited[1..].iter().all(|&visited| visited));
        assert_eq!((power(8), power(ORDER), multiply(0x80, 2)), (0x1D, 1, 0x1D), "x^8 is x^4 + x^3 + x^2 + 1");
        for a in 1..=255u8 {
            assert_eq!(multiply(a, divide(1, a)), 1, "{a} times its inverse");
            assert_eq!((multiply(a, 0), multiply(0, a), divide(0, a)), (0, 0, 0));
            for b in [1u8, 2, 3, 0x53, 0xCA, 0xFF] {
                assert_eq!(divide(multiply(a, b), b), a);
                assert_eq!(multiply(a, b), multiply(b, a));
            }
        }
    }

    #[test]
    fn iso_parity_example() {
        // ISO/IEC 18004, annex I: the data of 1-M "01234567" and its ten parity codewords.
        let example = [0x10, 0x20, 0x0C, 0x56, 0x61, 0x80, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11];
        let parity = remainder(&example, &divisor(10));
        assert_eq!(parity, [0xA5, 0x24, 0xD4, 0xC1, 0xED, 0x36, 0xC7, 0x87, 0x2C, 0x55]);
        for (length, degree) in [(19, 7), (16, 10), (55, 15), (15, 30), (122, 30)] {
            let block = block(length, degree, degree);
            assert!(
                (0..degree).all(|at| eval_high_first(&block, power(at)) == 0),
                "{length} codewords of data and {degree} of parity"
            );
        }
    }

    #[test]
    fn corrects_up_to_half_parity() {
        for (length, parity) in [(9, 17), (19, 7), (36, 22), (43, 26), (116, 30), (122, 30), (15, 30)] {
            let clean = block(length, parity, parity);
            for errors in 0..=parity / 2 {
                for seed in 0..12 {
                    let mut block = clean.clone();
                    let mut places: Vec<usize> = (0..block.len()).collect();
                    for at in 0..errors {
                        let pick = at + (seed * 37 + at * 101) % (places.len() - at);
                        places.swap(at, pick);
                        block[places[at]] ^= (1 + (seed * 13 + at * 29) % 255) as u8;
                    }
                    assert!(correct(&mut block, parity), "{errors} errors in {length} + {parity}, seed {seed}");
                    assert_eq!(block, clean, "{errors} errors in {length} + {parity}, seed {seed}");
                }
            }
        }
    }

    #[test]
    fn refuses_too_many_errors() {
        let (length, parity) = (36, 22);
        let clean = block(length, parity, 5);
        let mut refused = 0;
        for seed in 0..200 {
            let mut block = clean.clone();
            let length = block.len();
            for at in 0..=parity / 2 {
                block[(seed * 7 + at * 5) % length] ^= (1 + (seed + at * 3) % 255) as u8;
            }
            let input = block.clone();
            if correct(&mut block, parity) {
                assert!(
                    (0..parity).all(|at| eval_high_first(&block, power(at)) == 0),
                    "what is taken as corrected is a block of the code"
                );
            } else {
                refused += 1;
                assert_eq!(block, input, "a block that is refused is left as it came");
            }
        }
        assert!(refused > 190, "{refused} of 200 refused");
    }
}
