use crate::crew::Crew;
use crate::sys::lock;
use flate2::read::ZlibDecoder;
use r1_funcore::base::crew::Part;
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::codec::jpeg::{self, Jpeg};
use r1_funcore::domain::gallery::stills::Still;
use r1_funcore::gfx::png::Png;
use r1_funcore::gfx::{Picture, bands, share};
use r1_funcore::hw::PARTS;
use std::io::{self, Read};
use std::sync::Mutex;

fn run_parts<'a>(crew: Option<&Crew>, parts: impl Iterator<Item = Part<'a>>) {
    match crew {
        Some(crew) => crew.work(parts),
        None => parts.for_each(|part| part()),
    }
}

/// Decodes a baseline JPEG to the size `to_size` makes of the size its file has, in `format`, on the crew's threads or the caller's alone.
pub fn decode(
    bytes: &[u8],
    to_size: impl FnOnce((usize, usize)) -> (usize, usize),
    format: PixelFormat,
    crew: Option<&Crew>,
    planes: &mut [Vec<u8>; 3],
) -> Result<Still, jpeg::Error> {
    let jpeg = Jpeg::read(bytes)?;
    let (width, height) = to_size(jpeg.size());
    let plan = jpeg.plan(width, height);
    let (parts, _) = plan.parts();
    let starts = jpeg.part_starts(&plan)?;
    let failure: Mutex<Option<jpeg::Error>> = Mutex::new(None);
    {
        let mut jobs = plan.pieces(planes);
        let (jpeg, plan, starts, failure) = (&jpeg, &plan, &starts, &failure);
        let groups = jobs.chunks_mut(share(parts, PARTS)).map(|group| {
            Box::new(move || {
                for (part, [luma, blue, red]) in group {
                    if let Err(e) = jpeg.decode(plan, *part, starts[*part], [&mut **luma, &mut **blue, &mut **red]) {
                        *lock(failure) = Some(e);
                        return;
                    }
                }
            }) as Part<'_>
        });
        run_parts(crew, groups);
    }
    if let Some(e) = failure.into_inner().unwrap_or_else(|e| e.into_inner()) {
        return Err(e);
    }
    let mut words = vec![0u32; width * height];
    let columns = jpeg.columns(&plan);
    let (jpeg, plan, planes) = (&jpeg, (&plan, &columns), [&planes[0][..], &planes[1][..], &planes[2][..]]);
    let painters = bands(&mut words, width, PARTS)
        .map(|(lines, band)| Box::new(move || jpeg.paint(plan, planes, format, lines, band)) as Part<'_>);
    run_parts(crew, painters);
    Ok(Still { picture: Picture { words, width }, size: jpeg.size() })
}

/// Inflates a PNG's image data into its rows, for `Png::picture`.
pub fn inflate(png: &Png) -> io::Result<Vec<u8>> {
    let mut rows = Vec::with_capacity(png.inflated_bytes());
    ZlibDecoder::new(png.data.as_slice()).take(png.read_limit()).read_to_end(&mut rows)?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::gfx::to_cover;

    #[test]
    fn decoded_to_cover_a_place() {
        let file = &include_bytes!("../../../funcore/assets/jpeg/colours_420.jpg")[..];
        let to_size = |size| to_cover(size, (20, 20));
        let still = decode(file, to_size, PixelFormat::XRGB8888, None, &mut Default::default()).unwrap();
        assert_eq!(
            (still.picture.width, still.picture.words.len(), still.size),
            (28, 28 * 20, (72, 52)),
            "72 by 52 pixels made to cover 20 by 20, and the size they have in the file"
        );
    }
}
