use super::Shared;
use super::frames::Frames;
use crate::crew::Crew;
use crate::log;
use crate::media::ion::Buffer;
use crate::media::jpeg::{Jpeg, file_buffer, planes_buffer};
use crate::media::still;
use crate::sys::{self, PAGE};
use r1_funcore::base::crew::Part;
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::base::planes::{self, Shrink, part_lines, to_planes};
use r1_funcore::codec::jpeg::{UNIT_PIXELS, oriented, picture_bytes};
use r1_funcore::domain::camera::{self, Developed, Development, Job, Outcome, Review, Shot, Sight, Taken, Work};
use r1_funcore::domain::gallery::stills;
use r1_funcore::domain::magic::{self, Order, Variant};
use r1_funcore::gfx::{bands, to_fit};
use r1_funcore::hw::camera::burst::{Burst, Merged, NO_BURST, Stage};
use r1_funcore::hw::camera::develop::{self, Developer, MODE, NO_DEVELOPER};
use r1_funcore::hw::camera::exposure::Still;
use r1_funcore::hw::camera::raw::Square;
use r1_funcore::hw::camera::sharpen::Sharpener;
use r1_funcore::hw::{CORES, PARTS};
use r1_funcore::openai::models::image;
use r1_funcore::storage::{PICTURE_EXTENSION, PICTURES};
use std::io;
use std::time::{Duration, Instant};

// The encoder takes the photo and the pixel cam's picture in whole units, and the planes of the latter fill whole pages.
const _: () = assert!(
    develop::SIDE.is_multiple_of(UNIT_PIXELS as usize)
        && image::SIDE.is_multiple_of(UNIT_PIXELS as usize)
        && picture_bytes(image::SIDE * image::SIDE).is_multiple_of(PAGE)
);

struct PhotoBuffers {
    encoder: Jpeg,
    planes: Buffer,
    small_planes: Buffer,
    file: Buffer,
    // The developed luma, which the sharpening reads while it writes the planes.
    luma: Vec<u8>,
}

impl PhotoBuffers {
    fn new() -> io::Result<PhotoBuffers> {
        let (pixels, small_pixels) = (develop::SIDE * develop::SIDE, image::SIDE * image::SIDE);
        let encoder = Jpeg::open(0)?;
        let ion = encoder.ion()?;
        Ok(PhotoBuffers {
            planes: planes_buffer(&ion, pixels)?,
            small_planes: planes_buffer(&ion, small_pixels)?,
            file: file_buffer(&ion, pixels)?,
            luma: vec![0; pixels],
            encoder,
        })
    }
}

// The encoder and a photo's buffers, claimed by the first work that needs them.
fn claim(buffers: &mut Option<PhotoBuffers>) -> io::Result<&mut PhotoBuffers> {
    match buffers {
        Some(buffers) => Ok(buffers),
        slot => Ok(slot.insert(PhotoBuffers::new()?)),
    }
}

// Shrinks a picture's planes of `side` pixels a side to `small` a side, in parts on the crew.
fn shrink_planes(
    crew: &Crew,
    (luma, chroma): (&[u8], &[u8]),
    side: usize,
    small: usize,
    (small_luma, small_chroma): (&mut [u8], &mut [u8]),
) {
    let shrink = &Shrink::new((side, side), (small, small));
    let parts = planes::parts(small_luma, small_chroma, small, part_lines(small, PARTS));
    crew.work(parts.map(|(lines, small_luma, small_chroma)| {
        Box::new(move || shrink.shrink((luma, chroma), lines, (small_luma, small_chroma))) as Part<'_>
    }));
}

fn shrink<'r>(crew: &Crew, buffers: &'r mut PhotoBuffers) -> io::Result<&'r [u8]> {
    let (side, small) = (develop::SIDE, image::SIDE);
    let (pixels, small_pixels) = (side * side, small * small);
    let photo = buffers.planes.bytes(0..picture_bytes(pixels)).split_at(pixels);
    let small_planes = buffers.small_planes.bytes_mut(0..picture_bytes(small_pixels)).split_at_mut(small_pixels);
    shrink_planes(crew, photo, side, small, small_planes);
    buffers.encoder.encode(&buffers.small_planes, small, &buffers.file)
}

fn develop(crew: &Crew, buffers: &mut PhotoBuffers, merged: &Merged, developer: &Developer) {
    let side = develop::SIDE;
    let pixels = side * side;
    let (luma, chroma) = buffers.planes.bytes_mut(0..picture_bytes(pixels)).split_at_mut(pixels);
    let parts = planes::parts(luma, chroma, side, part_lines(side, PARTS));
    crew.work(
        parts.map(|(lines, luma, chroma)| Box::new(move || developer.develop(merged, lines, luma, chroma)) as Part<'_>),
    );
}

fn sharpen(crew: &Crew, buffers: &mut PhotoBuffers) {
    let side = develop::SIDE;
    let luma = buffers.planes.bytes_mut(0..side * side);
    buffers.luma.copy_from_slice(luma);
    let (sharpener, source) = (Sharpener::new(&buffers.luma, side), &buffers.luma);
    let sharpener = &sharpener;
    crew.work(
        bands(luma, side, PARTS)
            .map(|(lines, out)| Box::new(move || sharpener.sharpen(source, side, lines, out)) as Part<'_>),
    );
}

// The developed photo shrunk to the review's side, in the screen's pixel words, standing as the review shows it, and its width.
fn review_picture(
    crew: &Crew,
    buffers: &PhotoBuffers,
    Sight { side, format, orientation }: Sight,
) -> (Vec<u32>, usize) {
    let (pixels, small_pixels) = (develop::SIDE * develop::SIDE, side * side);
    let photo = buffers.planes.bytes(0..picture_bytes(pixels)).split_at(pixels);
    let (mut small_luma, mut small_chroma) = (vec![0u8; small_pixels], vec![0u8; small_pixels / 2]);
    shrink_planes(crew, photo, develop::SIDE, side, (&mut small_luma, &mut small_chroma));
    let mut words = vec![0u32; small_pixels];
    planes::to_words(&small_luma, &small_chroma, side, format, &mut words);
    oriented(words, side, orientation)
}

// Puts a photo's copy for the pixel cam where it waits to be sent, and gives the pixel cam `order`; returns the copy's size.
fn hand_over(shared: &Shared, photo: &str, copy: &[u8], order: Order) -> io::Result<usize> {
    shared.data.put(magic::DIRECTORY, &magic::to_waiting_name(photo), copy)?;
    shared.order(order);
    Ok(copy.len())
}

// Develops a burst and hands the photo on: saved, given to the pixel cam and shown for its review, or, without a name, to the robot.
fn develop_burst(
    shared: &Shared,
    crew: &Crew,
    buffers: &mut Option<PhotoBuffers>,
    (frames, still, turn): (&Frames, &Still, r1_funcore::base::sampling::Turn),
    (photo, clicked_ns): (Option<&Taken>, u64),
) -> io::Result<()> {
    let since = || Duration::from_nanos(sys::monotonic_ns().saturating_sub(clicked_ns));
    let begun = since();
    shared.set_development(Development::Merging);
    let frame = MODE.frame();
    let mut burst = Burst::new(frames.frames(still.frames)?, frame).ok_or_else(|| io::Error::other(NO_BURST))?;
    for stage in Stage::ALL {
        let mut parts = burst.parts(stage, PARTS);
        crew.for_each(&mut parts, |_, part| part.run());
    }
    let merged = burst.into_merged();
    let merged_at = since();
    shared.set_development(Development::Finishing);
    let lens = Some((MODE.lens(), Square::middle_of(frame)));
    let developer = Developer::new(&merged, turn, still.recipe, still.under, lens, |parts| crew.work(parts))
        .ok_or_else(|| io::Error::other(NO_DEVELOPER))?;
    let buffers = claim(buffers)?;
    develop(crew, buffers, &merged, &developer);
    let developed_at = since();
    sharpen(crew, buffers);
    let sharpened_at = since();
    let file = buffers.encoder.encode(&buffers.planes, develop::SIDE, &buffers.file)?;
    let (length, encoded_at) = (file.len(), since());
    let times = [begun, merged_at, developed_at, sharpened_at, encoded_at];
    let developed = |name| Developed { name, length, reference: merged.reference(), noise: merged.noise(), times };
    let Some(Taken { name: photo, sight }) = photo else {
        let small = shrink(crew, buffers)?;
        shared.hand_shot(Shot { jpeg: small.to_vec() });
        log!("{}", developed(camera::LOOK).to_log_line(camera::LOOKED_TEXT));
        return Ok(());
    };
    let name = shared.data.save(PICTURES, &magic::to_stem(photo, Variant::Original), PICTURE_EXTENSION, file)?;
    let (picture, width) = review_picture(crew, buffers, *sight);
    shared.set_review(Review { photo: photo.clone(), picture, width });
    let saved_at = since();
    // The photo is safe; what the pixel cam needs of it comes after, kept so that it outlives a power off, and a failure there loses no photo.
    let result = shrink(crew, buffers).and_then(|small| hand_over(shared, photo, small, Order::Add(photo.into())));
    let handover = match result {
        Ok(bytes) => camera::to_handed_text(bytes, since() - saved_at),
        Err(e) => camera::to_unhanded_text(e),
    };
    log!("{}", developed(&name).to_log_line(&camera::to_saved_text(saved_at - encoded_at, &handover)));
    Ok(())
}

// A saved photo's file decoded on the crew, at the size `to_size` makes of its own, in `format`.
fn decode(
    shared: &Shared,
    crew: &Crew,
    file: &str,
    to_size: impl FnOnce((usize, usize)) -> (usize, usize),
    format: PixelFormat,
) -> io::Result<stills::Still> {
    let bytes = shared.data.read(PICTURES, file)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    still::decode(&bytes, to_size, format, Some(crew), &mut Default::default()).map_err(|e| sys::invalid(e.to_string()))
}

// Shows the pixel art of the photo a review shows, as the review shows it.
fn show_art(shared: &Shared, crew: &Crew, photo: String, Sight { side, format, orientation }: Sight) -> io::Result<()> {
    let file = magic::to_name(&photo, Variant::Pixel, PICTURE_EXTENSION);
    let still = decode(shared, crew, &file, |size| to_fit(size, (side, side)), format)?;
    let (picture, width) = oriented(still.picture.words, still.picture.width, orientation);
    shared.set_review(Review { photo, picture, width });
    Ok(())
}

// Makes a saved photo's copy for the pixel cam anew from its file, as a burst's is made, and hands it over for its redraw; returns the copy's size.
fn copy(shared: &Shared, crew: &Crew, buffers: &mut Option<PhotoBuffers>, photo: &str) -> io::Result<usize> {
    let side = image::SIDE;
    let file = magic::to_name(photo, Variant::Original, PICTURE_EXTENSION);
    let still = decode(shared, crew, &file, |_| (side, side), PixelFormat::XRGB8888)?;
    if let Some(refusal) = magic::to_refusal(still.size) {
        return Err(sys::invalid(refusal));
    }
    let buffers = claim(buffers)?;
    let pixels = side * side;
    let (luma, chroma) = buffers.small_planes.bytes_mut(0..picture_bytes(pixels)).split_at_mut(pixels);
    to_planes(&still.picture.words, side, luma, chroma);
    let small = buffers.encoder.encode(&buffers.small_planes, side, &buffers.file)?;
    hand_over(shared, photo, small, Order::Redraw(photo.into()))
}

// The darkroom's thread: develops every burst the camera's thread leaves, one at a time, on a crew of its own, shows the art a review asks for, makes the copies redraws ask for, and lets go of the encoder once nothing waits.
pub(super) fn run(shared: &Shared) {
    let crew = Crew::with_cores("darkroom", &CORES[1..]);
    let mut buffers: Option<PhotoBuffers> = None;
    loop {
        match shared.board.wait(|state| state.darkroom.take_work()) {
            Work::Develop(job) => {
                let session = job.session();
                let Job { burst, still, turn, photo, clicked_ns, .. } = job;
                let result =
                    develop_burst(shared, &crew, &mut buffers, (&burst, &still, turn), (photo.as_ref(), clicked_ns));
                if let Err(e) = result {
                    log!("camera: photo {}: {e}", camera::to_log_name(photo.as_ref()));
                    shared.set_outcome(Outcome::NoPhoto);
                }
                shared.set_development(Development::Done);
                let spent = shared.board.lock().darkroom.give_back(burst, session);
                drop(spent);
                shared.board.notify();
                shared.board.raise();
            }
            Work::Art(photo, sight) => {
                if let Err(e) = show_art(shared, &crew, photo, sight) {
                    log!("camera: the review's pixel art: {e}");
                }
            }
            Work::Copy(photo) => {
                let start = Instant::now();
                let handover = match copy(shared, &crew, &mut buffers, &photo) {
                    Ok(bytes) => camera::to_handed_text(bytes, start.elapsed()),
                    Err(e) => {
                        shared.note_uncopied();
                        camera::to_unhanded_text(e)
                    }
                };
                log!("{}", camera::to_redraw_line(&photo, &handover));
            }
            Work::Release => {
                buffers = None;
                shared.board.raise();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crew_shrink_equals_one_thread() {
        let (side, small) = (64, 24);
        let luma: Vec<u8> = (0..side * side).map(|at| (at * 31 + at / 7) as u8).collect();
        let chroma: Vec<u8> = (0..side * side / 2).map(|at| (at * 17 + at / 5) as u8).collect();
        let (mut whole_luma, mut whole_chroma) = (vec![0u8; small * small], vec![0u8; small * small / 2]);
        Shrink::new((side, side), (small, small)).shrink(
            (&luma, &chroma),
            0..small,
            (&mut whole_luma, &mut whole_chroma),
        );
        assert!(whole_luma.iter().any(|sample| *sample != whole_luma[0]), "a picture with something in it");
        for threads in [PARTS - 1, 0] {
            let crew = Crew::new("test", threads).unwrap();
            let (mut small_luma, mut small_chroma) = (vec![0u8; small * small], vec![0u8; small * small / 2]);
            shrink_planes(&crew, (&luma, &chroma), side, small, (&mut small_luma, &mut small_chroma));
            assert!(
                small_luma == whole_luma && small_chroma == whole_chroma,
                "in parts on {threads} threads as in one piece"
            );
        }
    }
}
