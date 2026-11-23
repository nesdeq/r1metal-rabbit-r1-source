use crate::media::codec::{Driver, Lock, Owner};
use crate::media::ion::{Buffer, Ion};
use crate::sys::{PAGE, Registers};
use r1_funcore::hw::Block;
use r1_funcore::hw::venc::{self, Ending, Memory, Settings, Shape};
use std::io;
use std::os::fd::RawFd;

const REGISTERS_AT: libc::off_t = 0x1702_0000;
const REGISTER_BYTES: usize = 0x1000;
const SOURCE_PORT: i32 = 15;
const STREAM_PORT: i32 = 10;
const LUMA_PORT: i32 = 17;
const CHROMA_PORT: i32 = 18;
const MOTION_PORT: i32 = 11;
// A stream buffer of whole pages is of whole units of the size its register is told in.
const _: () = assert!(PAGE.is_multiple_of(1 << venc::STREAM_UNIT_SHIFT));

struct Buffers {
    source: Buffer,
    stream: Buffer,
    _work: [[Buffer; 2]; 3],
    memory: Memory,
}

impl Buffers {
    fn new(ion: &Ion, shape: Shape) -> io::Result<Buffers> {
        let (luma_bytes, chroma_bytes, motion_bytes) = shape.work();
        let source = ion.buffer(shape.source_bytes(), SOURCE_PORT)?;
        let stream = ion.buffer(shape.stream_bytes(), STREAM_PORT)?;
        let pair = |bytes: usize, port: i32| Ok::<_, io::Error>([ion.buffer(bytes, port)?, ion.buffer(bytes, port)?]);
        let (luma, chroma, motion) =
            (pair(luma_bytes, LUMA_PORT)?, pair(chroma_bytes, CHROMA_PORT)?, pair(motion_bytes, MOTION_PORT)?);
        let addresses = |pair: &[Buffer; 2]| [pair[0].device_address(), pair[1].device_address()];
        let memory = Memory {
            source: source.device_address(),
            stream: stream.device_address(),
            stream_bytes: stream.len() as u32,
            luma: addresses(&luma),
            chroma: addresses(&chroma),
            motion: addresses(&motion),
        };
        if !memory.is_aligned() {
            return Err(io::Error::other(memory.to_unaligned_text()));
        }
        Ok(Buffers { source, stream, _work: [luma, chroma, motion], memory })
    }
}

/// The encoder locked, powered and mapped, with its buffers for one kind of stream; dropping it releases the block before the buffers are freed.
pub struct Venc {
    registers: Registers,
    lock: Lock,
    buffers: Buffers,
    settings: Settings,
}

impl Venc {
    /// Takes the block, maps the registers and makes the buffers from an ION node above its own, so a dying process stops the encoder before it frees them.
    pub fn open(floor: RawFd, settings: Settings) -> io::Result<Venc> {
        let lock = Lock::take(&Owner::default(), Driver::H264Encoder, floor)?;
        let registers = Registers::default().map(lock.fd(), REGISTERS_AT, &[(Block::Venc, 0, REGISTER_BYTES)])?;
        let buffers = Buffers::new(&lock.ion()?, settings.shape)?;
        Ok(Venc { registers, lock, buffers, settings })
    }

    pub const fn settings(&self) -> Settings {
        self.settings
    }

    /// The picture to encode next, as `Shape::source_bytes` lays it out, for the caller to fill.
    pub fn source(&mut self) -> &mut [u8] {
        self.buffers.source.bytes_mut(0..self.settings.shape.source_bytes())
    }

    /// Starts frame `index`, from 0, at `quantiser` on the filled picture; its interrupt marks the frame's end.
    pub fn start(&mut self, index: u32, quantiser: u32) -> io::Result<()> {
        self.buffers.source.flush(0..self.settings.shape.source_bytes())?;
        venc::start(&mut self.registers, &self.settings, &self.buffers.memory, index, quantiser);
        Ok(())
    }

    /// Sleeps until the frame has ended and tells how; after `venc::TIMEOUT_MS` the block is reset and a late interrupt forgotten.
    pub fn wait(&mut self) -> io::Result<Ending> {
        if let Some(status) = self.lock.wait_interrupt(venc::TIMEOUT_MS)? {
            return Ok(Ending::from_status(status));
        }
        venc::reset(&mut self.registers);
        self.lock.wait_interrupt(0)?;
        Err(io::Error::other(venc::to_late_text()))
    }

    /// The ended frame's stream and clock cycles; an error for a frame whose stream its buffer does not hold.
    pub fn stream(&mut self, ending: Ending) -> io::Result<(&[u8], u32)> {
        let (bytes, cycles) = venc::finish(&mut self.registers);
        let bytes = bytes as usize;
        if !ending.fits(bytes, self.buffers.stream.len()) {
            return Err(io::Error::other(ending.to_unfit_text(bytes)));
        }
        Ok((self.buffers.stream.dma_bytes(0..bytes)?, cycles))
    }
}
