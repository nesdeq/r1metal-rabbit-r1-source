use super::{Context, Shared};
use crate::media::still::decode;
use crate::storage::Data;
use crate::{log, sys};
use r1_funcore::codec::jpeg::Jpeg;
use r1_funcore::domain::gallery::items::Item;
use r1_funcore::domain::gallery::state::Open;
use r1_funcore::domain::gallery::stills::{self, Cache, Next, Still};
use r1_funcore::domain::gallery::{Kind, Label, Thumbnail};
use r1_funcore::gfx::{to_cover, to_fit};
use r1_funcore::storage::PICTURES;
use std::io::{self, Read};
use std::time::Instant;

#[derive(Default)]
pub(super) struct DecodeBuffers {
    bytes: Vec<u8>,
    planes: [Vec<u8>; 3],
}

pub(super) fn read(data: &Data, name: &str, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut file = data.open(PICTURES, name)?;
    bytes.clear();
    bytes.reserve(file.metadata()?.len() as usize);
    file.read_to_end(bytes).map(drop)
}

pub(super) fn photo_size(data: &Data, name: &str) -> io::Result<(usize, usize)> {
    let mut bytes = Vec::new();
    read(data, name, &mut bytes)?;
    Jpeg::read(&bytes).map(|jpeg| jpeg.size()).map_err(|e| sys::invalid(e.to_string()))
}

// Reads a photo's file into buffers of the decoders', which get them back, and gives what `decode` makes of its bytes with their planes.
fn with_file<T>(
    context: Context,
    name: &str,
    decode: impl FnOnce(&[u8], &mut [Vec<u8>; 3]) -> io::Result<T>,
) -> io::Result<T> {
    let mut buffers = context.decoders.take_buffers();
    let result =
        read(context.data, name, &mut buffers.bytes).and_then(|()| decode(&buffers.bytes, &mut buffers.planes));
    context.decoders.put_buffers(buffers);
    result
}

fn load_picture(context: Context, name: &str, screen: (usize, usize)) -> Option<Still> {
    let start = Instant::now();
    let result = with_file(context, name, |bytes, planes| {
        let read_time = start.elapsed();
        let crew = Some(&context.decoders.crew);
        let still = decode(bytes, |size| to_fit(size, screen), context.format, crew, planes)
            .map_err(|e| sys::invalid(e.to_string()))?;
        let decode_time = start.elapsed() - read_time;
        log!("{}", stills::to_decoded_line(name, bytes.len(), read_time, &still.picture, decode_time));
        Ok(still)
    });
    result.inspect_err(|e| log!("gallery: {name}: {e}")).ok()
}

fn decode_file(
    context: Context,
    name: &str,
    to_size: impl FnOnce((usize, usize)) -> (usize, usize),
) -> io::Result<Still> {
    with_file(context, name, |bytes, planes| {
        decode(bytes, to_size, context.format, None, planes).map_err(|e| sys::invalid(e.to_string()))
    })
}

// The small picture of the file `name`, which shows for the photo whose file as taken is `file`.
pub(super) fn read_thumbnail(
    context: Context,
    (name, file): (&str, &str),
    label: Label,
    place: (usize, usize),
) -> io::Result<Thumbnail> {
    let still = decode_file(context, name, |size| to_cover(size, place))?;
    Ok(Thumbnail { picture: still.picture, label, size: still.size, file: file.into() })
}

pub(super) fn show_pictures(shared: &Shared, context: Context, items: &[Item], generation: u32, cache: &mut Cache) {
    loop {
        let open = shared.board.lock().next_open(items.len(), generation);
        let (request, at) = match open {
            Open::Closed => return cache.reopen(),
            Open::Stale => return,
            Open::Item(request, at) => (request, at),
        };
        match cache.next(items, at, request.turn, request.is_original) {
            Next::Show(file) => {
                let shown = (at, items[at].original.as_str());
                match cache.picture(file) {
                    Some(still) => shared.show(&mut still.picture.clone(), Kind::Photo, shown),
                    None => shared.show_unreadable(Kind::Photo, shown),
                }
                cache.mark_shown(at, request.turn, file);
            }
            Next::Decode(file) => {
                let pose = shared.board.lock().pose;
                cache.add(file, load_picture(context, file, request.room.large_size(Kind::Photo, pose)));
            }
            Next::Wait => shared.board.wait(|board| board.has_moved(request, generation).then_some(())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{FORMAT, SCREEN, decoders};
    use super::*;
    use crate::sys::lock;
    use r1_funcore::codec::jpeg;

    #[test]
    fn crew_decode_equals_one_thread() {
        let decoders = decoders();
        let mut buffers = decoders.take_buffers();
        for file in [
            &include_bytes!("../../../funcore/assets/jpeg/restarts_420.jpg")[..],
            include_bytes!("../../../funcore/assets/jpeg/colours_420.jpg"),
            include_bytes!("../../../funcore/assets/jpeg/grey.jpg"),
        ] {
            for screen in [(36, 26), (72, 52), (480, 640), (20, 100)] {
                let (width, height) = to_fit((72, 52), screen);
                let single = decode(file, |size| to_fit(size, screen), FORMAT, None, &mut Default::default()).unwrap();
                assert_eq!(
                    (single.picture.width, single.picture.words.len(), single.size),
                    (width, width * height, (72, 52))
                );
                for crew in [Some(&decoders.crew), None] {
                    let still = decode(file, |size| to_fit(size, screen), FORMAT, crew, &mut buffers.planes).unwrap();
                    assert_eq!(
                        (still.picture.width, still.picture.words == single.picture.words),
                        (width, true),
                        "fitted to {screen:?} into the planes of the picture before, on the crew's threads: {}",
                        crew.is_some()
                    );
                }
            }
        }
        assert_eq!(
            decode(b"no picture", |size| to_fit(size, SCREEN), FORMAT, Some(&decoders.crew), &mut buffers.planes).err(),
            Some(jpeg::Error::NotJpeg)
        );
        let short_file = &include_bytes!("../../../funcore/assets/jpeg/restarts_420.jpg")[..0x400];
        for crew in [Some(&decoders.crew), None] {
            assert!(
                decode(short_file, |size| to_fit(size, SCREEN), FORMAT, crew, &mut buffers.planes).is_err(),
                "a picture that ends before its last part"
            );
        }
        let grey = include_bytes!("../../../funcore/assets/jpeg/grey.jpg");
        decode(
            include_bytes!("../../../funcore/assets/jpeg/colours_420.jpg"),
            |size| size,
            FORMAT,
            None,
            &mut buffers.planes,
        )
        .unwrap();
        assert_eq!(
            decode(grey, |size| size, FORMAT, Some(&decoders.crew), &mut buffers.planes).unwrap().picture.words,
            decode(grey, |size| size, FORMAT, None, &mut Default::default()).unwrap().picture.words,
            "planes that held a picture in colour before leave nothing of it in a grey one"
        );
        decoders.put_buffers(buffers);
        let count = lock(&decoders.buffers).len();
        let buffers = decoders.take_buffers();
        assert!(
            count == 1 && buffers.planes[0].capacity() > 0,
            "what a picture was decoded with is there for the next"
        );
        decoders.put_buffers(buffers);
        decoders.clear();
        let count = lock(&decoders.buffers).len();
        assert!(count == 0 && decoders.take_buffers().planes[0].capacity() == 0, "until the decoders are cleared");
    }

    #[test]
    fn thumbnail_buffers_put_back() {
        let decoders = decoders();
        let context = Context { data: &Data::unusable(), format: FORMAT, refresh_ns: 1000, decoders: &decoders };
        assert!(
            read_thumbnail(context, ("a.jpg", "a.jpg"), Label::None, (228, 173)).is_err(),
            "without a partition there is no file to read"
        );
        assert_eq!(lock(&decoders.buffers).len(), 1, "the kit is there for the next that needs it");
    }
}
