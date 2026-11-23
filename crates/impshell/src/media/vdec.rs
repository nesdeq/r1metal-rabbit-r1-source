use crate::log;
use crate::media::codec::{Driver, Lock, Owner};
use crate::media::ion::Buffer;
use crate::sys::{self, Registers, lock};
use r1_funcore::codec::Flaw;
use r1_funcore::codec::h264::dpb::Output;
use r1_funcore::codec::h264::read::Sequence;
use r1_funcore::hw::Block;
use r1_funcore::hw::vdec::{
    self, BUSY_TEXT, LINE_BYTES, Outcome, READER_BYTES, STUCK_LINE, StartOutcome, Stream, UNSTOPPED_LINE, Watch, Work,
};
use std::io;
use std::mem::ManuallyDrop;
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const PORT: i32 = 10;
const SHARED_BASE: u64 = 0x1700_0000;
const SHARED_BYTES: usize = 0x1000;
const DECODER_BASE: u64 = 0x1704_0000;
const DECODER_BYTES: usize = 0x6000;
const DECODER_AT: usize = (DECODER_BASE - SHARED_BASE) as usize;
// The shared registers end before the decoder's begin: one mapping from the first holds both.
const _: () = assert!(DECODER_AT >= SHARED_BYTES);
const BLOCKS: [(Block, usize, usize); 2] = [(Block::Codec, 0, SHARED_BYTES), (Block::Vdec, DECODER_AT, DECODER_BYTES)];
const FLOOR: RawFd = 0;

struct Decode {
    start: Instant,
    outcome: Outcome,
}

fn to_error(flaw: Flaw) -> io::Error {
    sys::invalid(flaw.to_string())
}

struct Hardware {
    registers: Registers,
    lock: Lock,
    stream: Buffer,
    reader: Buffer,
    lines: [Buffer; 2],
    _motion: Vec<Buffer>,
    pictures: Vec<Buffer>,
}

/// The hardware of decoders whose feed would not rest, the codec block held: the kernel's release would wait for such a feed without end.
#[derive(Clone, Default)]
pub struct StuckList(Arc<Mutex<Vec<Hardware>>>);

impl StuckList {
    /// Polls each decoder's feed, breaking its picture off first, and frees those that rest: the codec block, then the buffers.
    pub fn retry(&self) {
        let mut list = lock(&self.0);
        let count = list.len();
        list.retain_mut(|hardware| !vdec::is_at_rest(&mut hardware.registers, true));
        vdec::to_retry_line(count, list.len()).iter().for_each(|line| log!("{line}"));
    }
}

/// A stream being decoded; dropping it stops the decoder before its buffers go, or moves the hardware to the `StuckList`.
pub struct Decoder {
    hardware: ManuallyDrop<Hardware>,
    stuck_list: StuckList,
    stream: Stream,
    motion_at: Vec<u32>,
    pictures_at: Vec<u32>,
    decode: Option<Decode>,
}

impl Decoder {
    /// Makes the buffers for a stream with these sequence parameters, with `spare` pictures that may be out to be shown at once.
    pub fn open(sequence: &Sequence, spare: usize, stuck_list: &StuckList) -> io::Result<Decoder> {
        let stream = Stream::new(sequence, spare)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, vdec::to_unfit_text(sequence)))?;
        // The block is taken once, which tells whether the kernel gives it at all, and let go again.
        let mut lock = Lock::take(&Owner::default(), Driver::H264Decoder, FLOOR)?;
        let registers = Registers::default().map(lock.fd(), SHARED_BASE as libc::off_t, &BLOCKS)?;
        lock.unlock()?;
        // The buffers' descriptors lie above the codec node's, so a dying process closes the decoder before it frees their memory.
        let ion = lock.ion()?;
        let buffer = |bytes: usize| ion.buffer(bytes, PORT);
        let buffers =
            |count: usize, bytes: usize| (0..count).map(|_| buffer(bytes)).collect::<io::Result<Vec<Buffer>>>();
        let (layout, (motions, pictures)) = (stream.layout(), stream.buffers());
        let (motion, pictures) = (buffers(motions, layout.motion_bytes())?, buffers(pictures, layout.picture_bytes())?);
        let addresses = |buffers: &[Buffer]| buffers.iter().map(Buffer::device_address).collect::<Vec<u32>>();
        let (motion_at, pictures_at) = (addresses(&motion), addresses(&pictures));
        let hardware = Hardware {
            registers,
            lock,
            stream: buffer(layout.stream_bytes())?,
            reader: buffer(READER_BYTES)?,
            lines: [buffer(LINE_BYTES[0])?, buffer(LINE_BYTES[1])?],
            _motion: motion,
            pictures,
        };
        Ok(Decoder {
            hardware: ManuallyDrop::new(hardware),
            stuck_list: stuck_list.clone(),
            stream,
            motion_at,
            pictures_at,
            decode: None,
        })
    }

    /// True if the buffers hold the pictures of a stream with these sequence parameters.
    pub fn fits(&self, sequence: &Sequence) -> bool {
        self.stream.fits(sequence)
    }

    /// True while no picture is in work and the codec block is released.
    pub fn is_at_rest(&self) -> bool {
        self.decode.is_none() && !self.hardware.lock.is_held()
    }

    /// Forgets the stream, its parameter sets and frames, for another stream that fits the buffers.
    pub fn reset(&mut self) {
        self.stream.reset();
    }

    /// Takes a parameter set from outside the samples.
    pub fn take_parameter_set(&mut self, unit: &[u8]) -> io::Result<()> {
        self.stream.add_set(unit).map_err(to_error)
    }

    /// Starts a sample's picture, stamped `stamp`, holding the block until `end`; or tells why the sample makes no picture.
    pub fn begin(&mut self, units: &[&[u8]], stamp: u64) -> io::Result<StartOutcome> {
        if self.decode.is_some() {
            return Err(io::Error::other(BUSY_TEXT));
        }
        let hardware = &mut *self.hardware;
        let size = hardware.stream.len();
        let picture = match self.stream.begin(units, stamp, hardware.stream.bytes_mut(0..size)) {
            Ok(picture) => picture,
            Err(outcome) => return Ok(outcome),
        };
        hardware.stream.flush(0..picture.feed_bytes())?;
        let work = Work {
            stream: hardware.stream.device_address(),
            reader: hardware.reader.device_address(),
            lines: [hardware.lines[0].device_address(), hardware.lines[1].device_address()],
            motion: &self.motion_at,
            pictures: &self.pictures_at,
        };
        let start = Instant::now();
        hardware.lock.acquire()?;
        let outcome = picture.start(&mut hardware.registers, &work);
        self.decode = Some(Decode { start, outcome });
        Ok(StartOutcome::Done)
    }

    fn wait_picture(&mut self) -> io::Result<bool> {
        let (mut watch, mut position) = (Watch::default(), None);
        while let Some(wait_ms) = watch.next_wait(position) {
            if self.hardware.lock.wait_interrupt(wait_ms)?.is_some() {
                return Ok(true);
            }
            position = Some(vdec::last_macroblock(&mut self.hardware.registers));
        }
        Ok(false)
    }

    /// Waits for the picture begun last, releases the block and tells how the sample went and which pictures show now.
    pub fn end(&mut self) -> io::Result<Outcome> {
        let Some(Decode { start, mut outcome }) = self.decode.take() else {
            return Ok(Outcome::default());
        };
        // The encoders share the block: the decoder's registers are handed to funcore only while it is held.
        debug_assert!(self.hardware.lock.is_held(), "a picture in work without the codec block");
        if outcome.stall.is_none() && self.wait_picture()? {
            outcome.ending = Some(self.stream.ending(&mut self.hardware.registers));
        }
        let is_broken = outcome.ending.is_none();
        let is_at_rest = vdec::is_at_rest(&mut self.hardware.registers, is_broken);
        if is_broken {
            self.hardware.lock.wait_interrupt(0)?;
        }
        if is_at_rest {
            self.hardware.lock.unlock()?;
        } else {
            log!("{UNSTOPPED_LINE}");
        }
        outcome.lock_micros = start.elapsed().as_micros() as u64;
        if let Some(buffer) = outcome.buffer {
            self.hardware.pictures[buffer].refresh(0..self.stream.layout().picture_bytes())?;
        }
        self.stream.finish(&mut outcome);
        Ok(outcome)
    }

    /// The pictures that wait to be shown when the stream has ended.
    pub fn drain(&mut self) -> Vec<Output> {
        self.stream.drain()
    }

    /// Forgets the stream's frames, to decode from a key picture again; pictures out to be shown stay out.
    pub fn rewind(&mut self) {
        self.stream.rewind();
    }

    /// A decoded picture as the decoder stores it.
    pub fn picture(&self, buffer: usize) -> &[u8] {
        let bytes = self.stream.layout().picture_bytes();
        self.hardware.pictures.get(buffer).map_or(&[], |picture| picture.bytes(0..bytes))
    }

    /// Takes back a shown picture.
    pub fn give_back(&mut self, buffer: usize) {
        self.stream.give_back(buffer);
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // The hardware is taken here alone, once, as the decoder goes.
        let mut hardware = unsafe { ManuallyDrop::take(&mut self.hardware) };
        if hardware.lock.is_held() && !vdec::is_at_rest(&mut hardware.registers, true) {
            log!("{STUCK_LINE}");
            lock(&self.stuck_list.0).push(hardware);
        }
    }
}
