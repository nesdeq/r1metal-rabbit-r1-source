//! A burst of raw frames made into one, stage by stage, each stage in parts that run at once: the frames' planes, their pyramids, the reference, every other frame laid on it, the noise, the merge.

use crate::hw::camera::align;
use crate::hw::camera::align::Field;
use crate::hw::camera::align::Image;
use crate::hw::camera::align::Pyramid;
use crate::hw::camera::frame::Frame;
use crate::hw::camera::merge;
use crate::hw::camera::merge::Merger;
use crate::hw::camera::merge::Strip;
use crate::hw::camera::mosaic::Band;
use crate::hw::camera::mosaic::PLANES;
use crate::hw::camera::mosaic::Planes;
use crate::hw::camera::noise::Noise;
use crate::hw::camera::raw::Square;
use alloc::vec;
use alloc::vec::Vec;

// HDR+ takes the sharpest of the first three frames as the reference.
const REFERENCE_CHOICES: usize = 3;

/// The stages of a burst, in their order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Unpack,
    Pyramids,
    Align,
    Noise,
    MergeEven,
    MergeOdd,
    Fine,
}

impl Stage {
    pub const ALL: [Stage; 7] =
        [Stage::Unpack, Stage::Pyramids, Stage::Align, Stage::Noise, Stage::MergeEven, Stage::MergeOdd, Stage::Fine];
}

/// A burst's frames, what each stage made of them, and the merge.
pub struct Burst<'a> {
    frames: Vec<&'a [u8]>,
    frame: Frame,
    square: Square,
    planes: Vec<Planes>,
    pyramids: Vec<Option<Pyramid>>,
    sharpness: Vec<u64>,
    reference: usize,
    fields: Vec<Option<Field>>,
    noise: Noise,
    sums: [Vec<f32>; PLANES],
    merged: Planes,
    merger: Merger,
}

/// A part of a stage, to run beside the stage's other parts.
pub struct Part<'p>(Work<'p>);

enum Work<'p> {
    Unpack {
        band: Band<'p>,
        raw: &'p [u8],
        frame: Frame,
        square: Square,
    },
    Pyramid {
        planes: &'p Planes,
        pyramid: &'p mut Option<Pyramid>,
        sharpness: &'p mut u64,
    },
    Align {
        reference: &'p Pyramid,
        other: &'p Pyramid,
        field: &'p mut Option<Field>,
    },
    Noise {
        reference: &'p Image,
        other: &'p Image,
        field: &'p Field,
        noise: &'p mut Noise,
    },
    Merge {
        strip: Strip<'p>,
        merger: &'p Merger,
        frames: &'p [Planes],
        fields: &'p [Option<Field>],
        reference: usize,
        noise: Noise,
    },
    Fine {
        sums: [&'p [f32]; PLANES],
        band: Band<'p>,
        start: usize,
    },
}

impl Part<'_> {
    /// Does the part's work.
    pub fn run(&mut self) {
        match &mut self.0 {
            Work::Unpack { band, raw, frame, square } => band.unpack(raw, *frame, *square),
            Work::Pyramid { planes, pyramid, sharpness } => {
                let stride = planes.stride();
                let mut grey = vec![0u16; stride * stride];
                planes.grey(0..stride, &mut grey);
                let grey = Image::new(stride, stride, grey);
                **sharpness = grey.sharpness();
                **pyramid = Some(Pyramid::new(grey));
            }
            Work::Align { reference, other, field } => **field = Some(align::align(reference, other)),
            Work::Noise { reference, other, field, noise } => **noise = Noise::measure(reference, other, field),
            Work::Merge { strip, merger, frames, fields, reference, noise } => {
                merger.merge(strip, frames, fields, *reference, *noise);
            }
            Work::Fine { sums, band, start } => {
                for (samples, sums) in band.rows_mut().iter_mut().zip(sums.iter()) {
                    merge::to_fine(&sums[*start..*start + samples.len()], samples);
                }
            }
        }
    }
}

/// What a burst came to: the merged planes, at `merge::FINE_BITS` more bits than the sensor's, and how.
pub struct Merged {
    pub(super) planes: Planes,
    pub(super) noise: Noise,
    pub(super) frames: usize,
    reference: usize,
}

/// The error text where `Burst::new` makes none.
pub const NO_BURST: &str = "no burst of these frames";

impl Merged {
    /// The frame the others were laid on.
    pub const fn reference(&self) -> usize {
        self.reference
    }

    /// A raw sample's variance per code of light, and with none.
    pub const fn noise(&self) -> (f32, f32) {
        (self.noise.per_code(), self.noise.floor())
    }
}

impl<'a> Burst<'a> {
    /// A burst of the middle squares of `frames`; none without a frame or with one too short.
    pub fn new(frames: Vec<&'a [u8]>, frame: Frame) -> Option<Burst<'a>> {
        if frames.is_empty() || frames.iter().any(|bytes| bytes.len() < frame.bytes()) {
            return None;
        }
        let square = Square::middle_of(frame);
        let side = square.side as usize / 2;
        let count = frames.len();
        let merged = Planes::new(side);
        let stride = merged.stride();
        Some(Burst {
            planes: (0..count).map(|_| Planes::new(side)).collect(),
            pyramids: (0..count).map(|_| None).collect(),
            sharpness: vec![0; count],
            reference: 0,
            fields: (0..count).map(|_| None).collect(),
            noise: Noise::default(),
            sums: core::array::from_fn(|_| vec![0.0; stride * stride]),
            merged,
            merger: Merger::default(),
            frames,
            frame,
            square,
        })
    }

    /// The parts of `stage`, about `count` of them where the work divides so.
    pub fn parts(&mut self, stage: Stage, count: usize) -> Vec<Part<'_>> {
        let count = count.max(1);
        let stride = self.merged.stride();
        let rows = stride.div_ceil(count);
        match stage {
            Stage::Unpack => {
                let (frames, frame, square) = (&self.frames, self.frame, self.square);
                let per_frame = rows * frames.len().min(count);
                self.planes
                    .iter_mut()
                    .zip(frames)
                    .flat_map(|(planes, raw)| {
                        planes.bands(per_frame).map(move |band| Part(Work::Unpack { band, raw, frame, square }))
                    })
                    .collect()
            }
            Stage::Pyramids => self
                .planes
                .iter()
                .zip(&mut self.pyramids)
                .zip(&mut self.sharpness)
                .map(|((planes, pyramid), sharpness)| Part(Work::Pyramid { planes, pyramid, sharpness }))
                .collect(),
            Stage::Align => {
                let choices = &self.sharpness[..self.sharpness.len().min(REFERENCE_CHOICES)];
                self.reference =
                    (0..choices.len()).max_by_key(|&at| (choices[at], core::cmp::Reverse(at))).unwrap_or(0);
                let reference = self.pyramids[self.reference].as_ref().expect("the pyramids come first");
                let (pyramids, own) = (&self.pyramids, self.reference);
                self.fields
                    .iter_mut()
                    .enumerate()
                    .filter(|(at, _)| *at != own)
                    .map(|(at, field)| {
                        Part(Work::Align { reference, other: pyramids[at].as_ref().expect("a pyramid"), field })
                    })
                    .collect()
            }
            Stage::Noise => {
                let Some((other, field)) =
                    self.fields.iter().enumerate().find_map(|(at, field)| Some((at, field.as_ref()?)))
                else {
                    return Vec::new();
                };
                let finest = |at: usize| self.pyramids[at].as_ref().expect("a pyramid").finest();
                vec![Part(Work::Noise {
                    reference: finest(self.reference),
                    other: finest(other),
                    field,
                    noise: &mut self.noise,
                })]
            }
            Stage::MergeEven | Stage::MergeOdd => {
                let parity = usize::from(stage == Stage::MergeOdd);
                let (merger, frames, fields, reference, noise) =
                    (&self.merger, &self.planes[..], &self.fields[..], self.reference, self.noise);
                let [first, second, third, fourth] = self.sums.each_mut();
                let pairs =
                    [[first.as_mut_slice(), second.as_mut_slice()], [third.as_mut_slice(), fourth.as_mut_slice()]];
                pairs
                    .into_iter()
                    .enumerate()
                    .flat_map(|(pair, sums)| merge::strips(sums, stride, (pair, parity), count.div_ceil(2)))
                    .map(|strip| Part(Work::Merge { strip, merger, frames, fields, reference, noise }))
                    .collect()
            }
            Stage::Fine => {
                let sums = self.sums.each_ref().map(Vec::as_slice);
                self.merged
                    .bands(rows)
                    .map(|band| {
                        let start = band.first() * stride;
                        Part(Work::Fine { sums, band, start })
                    })
                    .collect()
            }
        }
    }

    /// The merge, once every stage has run.
    pub fn into_merged(self) -> Merged {
        Merged { planes: self.merged, noise: self.noise, frames: self.frames.len(), reference: self.reference }
    }
}

#[cfg(test)]
pub(super) mod fake {
    use super::*;
    use crate::hw::camera::merge::FINE_BITS;

    // The merge of one raw frame alone, its samples in the merge's sixteenths.
    pub(crate) fn merged(raw: &[u8], frame: Frame) -> Merged {
        let square = Square::middle_of(frame);
        let mut planes = Planes::new(square.side as usize / 2);
        for mut band in planes.bands(4) {
            band.unpack(raw, frame, square);
            for samples in band.rows_mut().iter_mut() {
                samples.iter_mut().for_each(|sample| *sample <<= FINE_BITS);
            }
        }
        Merged { planes, noise: Noise::default(), frames: 1, reference: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::xorshift32;
    use crate::hw::camera::frame::BLACK;
    use crate::hw::camera::frame::fake::packed;
    use crate::hw::camera::merge::FINE_BITS;

    fn truth(x: u32, y: u32) -> u32 {
        BLACK + 200 + 3 * x + 2 * y
    }

    #[test]
    fn stages_merge_a_burst() {
        let frame = Frame::whole(160, 128);
        let frames: Vec<Vec<u8>> = (0..4u32)
            .map(|at| {
                packed(frame, |x, y| {
                    let noise = xorshift32((x * 7919 + y * 104_729 + at * 15_485_863) | 1) % 41;
                    (truth(x, y) + noise).saturating_sub(20)
                })
            })
            .collect();
        let mut burst = Burst::new(frames.iter().map(Vec::as_slice).collect(), frame).unwrap();
        for stage in Stage::ALL {
            let mut parts = burst.parts(stage, 4);
            assert!(!parts.is_empty(), "{stage:?} has work");
            parts.iter_mut().for_each(Part::run);
        }
        let merged = burst.into_merged();
        assert_eq!((merged.frames, merged.planes.side()), (4, 64), "four frames, of a square of 128");
        let square = Square::middle_of(frame);
        let (mut merged_error, mut single_error) = (0u64, 0u64);
        let raw = crate::hw::camera::raw::Raw::new(&frames[merged.reference], frame).unwrap();
        for y in 8..120u32 {
            for x in 8..120u32 {
                let expected = truth(x + square.left, y);
                let fine = u32::from(merged.planes.at(x as usize, y as usize));
                merged_error += u64::from((fine >> FINE_BITS).abs_diff(expected));
                single_error += u64::from(raw.at((x + square.left) as usize, y as usize).abs_diff(expected));
            }
        }
        assert!(
            merged_error * 3 < single_error * 2,
            "the merge lies nearer the truth: {merged_error} against {single_error}"
        );
        assert!(Burst::new(Vec::new(), frame).is_none() && Burst::new(vec![&frames[0][..10]], frame).is_none());
    }
}
