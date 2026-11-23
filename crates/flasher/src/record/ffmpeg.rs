//! ffmpeg, which encodes the pictures of a recording into an H.264 MP4.

use crate::Result;
use r1_funcore::domain::recording::StartBody;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command as Program, Stdio};

// The program that encodes the video, and how the pictures it is given are laid out and coloured: I420 in BT.709's limited range, as the device sends them.
const FFMPEG: &str = "ffmpeg";
const PICTURES: &str = "yuv420p";
const RANGE: &str = "tv";
const COLOURS: &str = "bt709";

// ffmpeg, encoding the pictures it is given into the video's file; in a process group of its own, so that a Ctrl-C meant for `r1` does not end it.
pub(super) struct Ffmpeg {
    child: Child,
    input: ChildStdin,
}

impl Ffmpeg {
    pub(super) fn open(started: &StartBody, out: &Path) -> io::Result<Ffmpeg> {
        let size = format!("{}x{}", started.width, started.height);
        let rate = started.rate.to_string();
        let colours =
            ["-color_range", RANGE, "-colorspace", COLOURS, "-color_primaries", COLOURS, "-color_trc", COLOURS];
        let mut child = Program::new(FFMPEG)
            .args(["-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", PICTURES])
            .args(["-video_size", &size, "-framerate", &rate])
            .args(colours)
            .args(["-i", "pipe:0", "-c:v", "libx264", "-pix_fmt", PICTURES])
            .args(colours)
            .args(["-movflags", "+faststart", "-y"])
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("{FFMPEG}, which encodes the video: {e}")))?;
        let input = child.stdin.take().ok_or_else(|| io::Error::other("ffmpeg without its input"))?;
        Ok(Ffmpeg { child, input })
    }

    // Ends the input and waits until ffmpeg has written the file.
    pub(super) fn finish(self) -> Result<()> {
        let Ffmpeg { mut child, input } = self;
        drop(input);
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("{FFMPEG} ended with {status}").into());
        }
        Ok(())
    }
}

impl Write for Ffmpeg {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.input.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.input.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::super::video::Video;
    use super::*;
    use r1_funcore::base::pixel::PixelFormat;
    use r1_funcore::domain::recording::Encoder;

    const SIDE: usize = 16;

    #[test]
    #[ignore = "has the host's ffmpeg encode a recording and ffprobe read the file"]
    fn ffmpeg_encodes_recording() {
        const RED: u32 = 0x00FF_0000;
        let out = std::env::temp_dir().join(format!("r1-record-{}.mp4", std::process::id()));
        let mut encoder = Encoder::new(SIDE, SIDE, PixelFormat::XRGB8888).unwrap();
        let mut stream = encoder.start(1).to_vec();
        stream.extend_from_slice(encoder.frame(&[RED; SIDE * SIDE], SIDE, 1_000_000, 0).unwrap());
        stream.extend_from_slice(encoder.end(2_000_000, 0));
        let mut video = Video::new(1, |started: &StartBody| Ffmpeg::open(started, &out));
        assert!(video.take(&stream).unwrap());
        video.pictures.take().unwrap().finish().unwrap();
        let probed = Program::new("ffprobe")
            .args(["-v", "error", "-show_entries"])
            .arg("stream=codec_name,width,height,r_frame_rate,nb_frames,pix_fmt,color_range,color_space,color_transfer,color_primaries")
            .args(["-of", "default=noprint_wrappers=1"])
            .arg(&out)
            .output()
            .unwrap();
        let told = String::from_utf8(probed.stdout).unwrap();
        for fact in [
            "codec_name=h264",
            "width=16",
            "height=16",
            "r_frame_rate=60/1",
            "nb_frames=60",
            "pix_fmt=yuv420p",
            "color_range=tv",
            "color_space=bt709",
            "color_transfer=bt709",
            "color_primaries=bt709",
        ] {
            assert!(told.lines().any(|line| line == fact), "{fact} in\n{told}");
        }
        let decoded = Program::new(FFMPEG)
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .output()
            .unwrap();
        std::fs::remove_file(&out).unwrap();
        // A half level of rounding in luma and both differences comes back through BT.709's inverse as at most two levels.
        let near = |got: u8, wanted: u8| got.abs_diff(wanted) <= 2;
        let middle = &decoded.stdout[(SIDE / 2 * SIDE + SIDE / 2) * 3..][..3];
        assert!(near(middle[0], 255) && near(middle[1], 0) && near(middle[2], 0), "red decoded as {middle:?}");
    }
}
