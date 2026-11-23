//! A recording on its way into a video: the records the device sends, made into whole pictures at the rate of its screen.

use crate::Result;
use r1_funcore::domain::recording::{Decoder, Kind, Records, StartBody, Timeline};
use std::io::{self, Write};
use std::path::Path;

// What a recording shows as it stands: what its start told, the picture, and the grid of the video's rate its frames are laid on.
struct Showing {
    started: StartBody,
    picture: Decoder,
    timeline: Timeline,
}

// Writes the picture as it stands `due` times to `pictures`, which `open` makes the first time.
fn show<W: Write>(
    pictures: &mut Option<W>,
    open: &mut impl FnMut(&StartBody) -> io::Result<W>,
    showing: &Showing,
    due: u64,
) -> Result<()> {
    if due == 0 {
        return Ok(());
    }
    let sink = match pictures {
        Some(sink) => sink,
        None => pictures.insert(open(&showing.started)?),
    };
    for _ in 0..due {
        sink.write_all(showing.picture.planes())?;
    }
    Ok(())
}

// The pictures of a recording written to the sink `open` makes for the first of them.
pub(super) struct Video<W, F> {
    // The time the recording that was asked for begins at, which its start carries: what comes before that start is an earlier recording's.
    begins_us: u64,
    records: Records,
    showing: Option<Showing>,
    pub(super) pictures: Option<W>,
    open: F,
    // What came: frames, frames the device passed over, bytes.
    frames: u64,
    skipped: u64,
    bytes: u64,
}

impl<W: Write, F: FnMut(&StartBody) -> io::Result<W>> Video<W, F> {
    pub(super) fn new(begins_us: u64, open: F) -> Self {
        let records = Records::default();
        Video { begins_us, records, showing: None, pictures: None, open, frames: 0, skipped: 0, bytes: 0 }
    }

    // Takes more of the stream and writes every picture that is due; true once the recording has ended.
    pub(super) fn take(&mut self, bytes: &[u8]) -> Result<bool> {
        self.bytes += bytes.len() as u64;
        self.records.push(bytes);
        while let Some((head, body)) = self.records.take() {
            match (head.kind, &mut self.showing) {
                (Kind::Start, None) if head.time_us == self.begins_us => {
                    let started = StartBody::read(&body).ok_or("the device's start tells no size or rate")?;
                    let picture = Decoder::new(started.width, started.height).ok_or("a start of no size")?;
                    self.showing = Some(Showing { started, picture, timeline: Timeline::new(started.rate) });
                }
                (Kind::Frame, Some(showing)) => {
                    let due = showing.timeline.fill_before(head.time_us);
                    show(&mut self.pictures, &mut self.open, showing, due)?;
                    showing.picture.apply(&body).map_err(|_| "the device sent a malformed frame")?;
                    (self.frames, self.skipped) = (self.frames + 1, self.skipped + u64::from(head.skips));
                }
                (Kind::End, Some(showing)) => {
                    let due = showing.timeline.fill_until(head.time_us);
                    show(&mut self.pictures, &mut self.open, showing, due)?;
                    return Ok(true);
                }
                // An earlier recording's leftovers.
                _ => {}
            }
        }
        Ok(false)
    }

    // Ends a recording whose end never came: its last frame is seen once.
    pub(super) fn cut(&mut self) -> Result<()> {
        match &mut self.showing {
            Some(showing) => {
                let due = showing.timeline.fill_last();
                show(&mut self.pictures, &mut self.open, showing, due)
            }
            None => Ok(()),
        }
    }

    // The line that tells what was recorded.
    pub(super) fn told(&self, out: &Path) -> String {
        let (rate, filled) =
            self.showing.as_ref().map_or((0, 0), |showing| (showing.started.rate, showing.timeline.places()));
        let seconds = if rate == 0 { 0.0 } else { filled as f64 / f64::from(rate) };
        format!(
            "{}: {seconds:.1} s at {rate} frames a second, {} frames from the device and {} it passed over, {} bytes over USB",
            out.display(),
            self.frames,
            self.skipped,
            self.bytes
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::base::pixel::PixelFormat;
    use r1_funcore::domain::recording::Encoder;

    const SIDE: usize = 16;
    const PERIOD_US: u64 = 16_667;

    // The pictures written, each as its first luma sample.
    fn lumas(written: &[u8]) -> Vec<u8> {
        written.chunks(SIDE * SIDE * 3 / 2).map(|picture| picture[0]).collect()
    }

    #[test]
    fn pictures_at_screen_rate() {
        let mut encoder = Encoder::new(SIDE, SIDE, PixelFormat::XRGB8888).unwrap();
        let (black, white) = (vec![0u32; SIDE * SIDE], vec![0x00FF_FFFFu32; SIDE * SIDE]);
        let mut stream = encoder.start(100).to_vec();
        stream.extend_from_slice(encoder.frame(&white, SIDE, 150, 0).unwrap());
        stream.extend_from_slice(encoder.end(160, 0));
        stream.extend_from_slice(encoder.start(1_000));
        stream.extend_from_slice(encoder.frame(&black, SIDE, 1_000_000, 0).unwrap());
        stream.extend_from_slice(encoder.frame(&white, SIDE, 1_000_000 + 3 * PERIOD_US, 2).unwrap());
        stream.extend_from_slice(encoder.end(1_000_000 + 5 * PERIOD_US, 0));
        let opened = std::cell::Cell::new(0);
        let mut video = Video::new(1_000, |started: &StartBody| {
            opened.set(opened.get() + 1);
            assert_eq!(*started, StartBody { width: SIDE, height: SIDE, rate: 60 });
            Ok(Vec::new())
        });
        let (first, rest) = stream.split_at(stream.len() / 3);
        assert!(!video.take(first).unwrap());
        assert!(video.take(rest).unwrap(), "the end of the recording asked for");
        assert_eq!(opened.get(), 1, "the video is opened once, for the first picture");
        assert_eq!(
            lumas(video.pictures.as_ref().unwrap()),
            [16, 16, 16, 235, 235],
            "black until the white frame's place, then white until the end; the earlier recording is left out"
        );
        assert_eq!((video.frames, video.skipped), (2, 2));
    }

    #[test]
    fn cut_recording() {
        let mut encoder = Encoder::new(SIDE, SIDE, PixelFormat::XRGB8888).unwrap();
        let mut stream = encoder.start(7).to_vec();
        let mut video = Video::new(7, |_: &StartBody| Ok(Vec::new()));
        assert!(!video.take(&stream).unwrap());
        video.cut().unwrap();
        assert!(video.pictures.is_none(), "no frame, no video");
        stream = encoder.frame(&[0x00FF_FFFF; SIDE * SIDE], SIDE, 50, 0).unwrap().to_vec();
        assert!(!video.take(&stream).unwrap());
        video.cut().unwrap();
        assert_eq!(lumas(video.pictures.as_ref().unwrap()), [235]);
        assert!(video.told(Path::new("demo.mp4")).starts_with("demo.mp4: 0.0 s at 60 frames a second, 1 frames"));
    }
}
