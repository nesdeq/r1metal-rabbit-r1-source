use super::{Resources, Shared};
use crate::media::still::{decode, inflate};
use crate::storage::Data;
use crate::{log, sys};
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::codec::jpeg::oriented;
use r1_funcore::domain::signal::pictures::{Drawn, area, decoded_size, drawn};
use r1_funcore::domain::signal::state::State;
use r1_funcore::gfx::Picture;
use r1_funcore::gfx::palette::Roles;
use r1_funcore::storage::paths::split_file;
use std::io;

// The picture a photo's file is decoded to, standing `wide` pixels across or less; none where the file is missing or none the R1 draws.
fn picture(data: &Data, format: PixelFormat, roles: Roles, (file, wide): (&str, usize)) -> io::Result<Option<Picture>> {
    let (directory, name) = split_file(file);
    let Some(bytes) = data.read(directory, name)? else {
        return Ok(None);
    };
    match drawn(&bytes) {
        Drawn::Jpeg(orientation) => {
            let size = |size| decoded_size(size, orientation, wide);
            let still =
                decode(&bytes, size, format, None, &mut Default::default()).map_err(|e| sys::invalid(e.to_string()))?;
            let (words, width) = oriented(still.picture.words, still.picture.width, orientation);
            Ok(Some(Picture { words, width }))
        }
        Drawn::Png(png) => {
            let picture = png.picture(inflate(&png)?, roles.background, area(wide), format);
            picture.map(Some).map_err(|e| sys::invalid(e.to_string()))
        }
        Drawn::Not => Ok(None),
    }
}

// The thread that decodes the photos a conversation shows, one after the other while they are wanted.
pub(super) fn work(shared: &Shared, resources: &Resources, format: PixelFormat) -> ! {
    loop {
        let (file, wide, roles) = shared.board.wait(State::take_picture);
        let drawn = picture(&resources.data, format, roles, (&file, wide));
        let drawn = drawn.inspect_err(|e| log!("signal: a photo's file is not decoded: {e}")).ok().flatten();
        if drawn.is_none() {
            log!("signal: a photo is not drawn");
        }
        shared.board.lock().give_pictures(vec![(file, drawn)], wide);
        shared.board.raise();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::scratch;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use r1_funcore::gfx::{Picture as Words, to_fit};
    use std::fs;
    use std::io::Write;

    const FORMAT: PixelFormat = PixelFormat::XRGB8888;
    const BASELINE: &[u8] = include_bytes!("../../../funcore/assets/jpeg/colours_420.jpg");

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        [&(data.len() as u32).to_be_bytes()[..], kind, data, &crc.sum().to_be_bytes()].concat()
    }

    // A PNG of that size, grey with no transparency (RFC 2083).
    fn grey_png(width: u32, height: u32) -> Vec<u8> {
        let header = [&width.to_be_bytes()[..], &height.to_be_bytes(), &[8, 0, 0, 0, 0]].concat();
        let line = [&[0u8][..], &vec![0x80; width as usize]].concat();
        let mut packed = ZlibEncoder::new(Vec::new(), Compression::default());
        (0..height).for_each(|_| packed.write_all(&line).unwrap());
        let data = packed.finish().unwrap();
        let signature = b"\x89PNG\r\n\x1a\n".to_vec();
        [signature, chunk(b"IHDR", &header), chunk(b"IDAT", &data), chunk(b"IEND", &[])].concat()
    }

    #[test]
    fn photo_width_bounded_by_room() {
        let root = scratch("pictures");
        let data = Data::with_root(&root);
        data.put("attachments", "a.jpg", BASELINE).unwrap();
        data.put("attachments", "big.png", &grey_png(10, 4)).unwrap();
        data.put("attachments", "bad.jpg", b"not a picture").unwrap();
        let roles = Roles::default();
        let wide = |file: &str, room: usize| {
            picture(&data, FORMAT, roles, (&format!("attachments/{file}"), room))
                .unwrap()
                .map(|Words { words, width }| (width, words.len() / width))
        };
        assert_eq!(wide("a.jpg", 480), Some((72, 52)), "a JPEG of 72 by 52 keeps its size in a room wider than it is");
        assert_eq!(
            wide("a.jpg", 36),
            Some(to_fit((72, 52), (36, 4096))),
            "and is made to stand as wide as a narrower one"
        );
        assert_eq!(wide("big.png", 480), Some((10, 4)), "a PNG too");
        assert_eq!(wide("big.png", 5), Some(to_fit((10, 4), (5, 4096))));
        assert_eq!(wide("bad.jpg", 480), None, "a file that is no picture keeps its word");
        assert_eq!(wide("gone.jpg", 480), None, "nor one that is not there");
        fs::remove_dir_all(root).unwrap();
    }
}
