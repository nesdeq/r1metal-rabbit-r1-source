use super::frames::Frames;
use super::isp::Isp;
use super::sensor::Sensor;
use super::swivel::Swivel;
use super::{Shared, Stats, Stop, draw, to_error};
use crate::crew::Crew;
use crate::media::ion::Ion;
use crate::{log, sys};
use r1_funcore::base::handoff::Latch;
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::base::time;
use r1_funcore::domain::camera::{Outcome, Viewfinder};
use r1_funcore::hw::camera::camsv::{self, BufferQueue, Capture, CaptureStep, NO_VSYNC_LINE, Rest, SLOTS};
use r1_funcore::hw::camera::exposure::{Auto, Exposure, Pace};
use r1_funcore::hw::camera::frame::Frame;
use r1_funcore::hw::camera::measure::Measure;
use r1_funcore::hw::camera::mode::Mode;
use r1_funcore::hw::camera::raw::{Finder, NO_FINDER};
use r1_funcore::hw::camera::seninf::{self, NO_CALIBRATION, Trim};
use r1_funcore::hw::camera::stream::{self, FrameSync};
use r1_funcore::hw::camera::swivel::{Facing, angle, viewfinder_turn};
use r1_funcore::hw::camera::tone::Tone;
use r1_funcore::hw::camera::{TOO_WARM_TEXT, WARM_BYTES, is_too_warm};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

const WARMTH_FILE: &str = "/proc/driver/cl_cam_status";

pub(super) struct Rig<'a> {
    crew: &'a Crew,
    isp: Isp,
    sensor: Sensor,
    ion: Ion,
    trim: Trim,
    is_receiving: Latch<bool>,
    is_sensor_streaming: Latch<bool>,
    is_armed: Latch<bool>,
    is_warmth_read: Latch<bool>,
    timeout: Duration,
    refresh_ns: u64,
}

pub(super) struct Stream<'s> {
    slots: &'s Frames,
    finder: &'s Finder,
    queue: BufferQueue,
    tone: Tone,
    picture: Vec<u32>,
    sync: FrameSync,
}

impl Stream<'_> {
    pub(super) fn picture(&self) -> (&[u32], usize) {
        (&self.picture, self.finder.size().0)
    }

    pub(super) const fn frame_count(&self) -> u64 {
        self.sync.frames()
    }

    pub(super) const fn tone(&self) -> &Tone {
        &self.tone
    }

    pub(super) const fn frame(&self) -> Frame {
        self.slots.frame()
    }

    pub(super) fn show(&mut self, shared: &Shared, viewfinder: Viewfinder) {
        if self.sync.is_showing() {
            shared.show(&mut self.picture, viewfinder, self.sync.is_in_step());
        }
    }
}

pub(super) struct FullSlot {
    pub(super) slot: usize,
    pub(super) count: usize,
    pub(super) drops: u32,
}

pub(super) struct DrawnFrame<'s> {
    slot: usize,
    pub(super) raw: &'s [u8],
    measure: Measure,
    elapsed: Duration,
}

impl<'a> Rig<'a> {
    pub(super) fn open(trim: Trim, crew: &'a Crew, refresh_ns: u64) -> io::Result<Rig<'a>> {
        let isp = Isp::open()?;
        let sensor = Sensor::open(isp.fd())?;
        let ion = isp.ion()?;
        Ok(Rig {
            crew,
            isp,
            sensor,
            ion,
            trim,
            is_receiving: Latch::with(false),
            is_sensor_streaming: Latch::with(false),
            is_armed: Latch::with(false),
            is_warmth_read: Latch::with(true),
            timeout: Duration::ZERO,
            refresh_ns,
        })
    }

    pub(super) const fn crew(&self) -> &'a Crew {
        self.crew
    }

    pub(super) fn floor(&self) -> RawFd {
        self.isp.fd()
    }

    pub(super) fn errors(&self) -> io::Result<u32> {
        self.isp.errors()
    }

    pub(super) fn configure(&mut self, mode: Mode, exposure: Exposure, pace: Pace) -> io::Result<()> {
        if self.is_sensor_streaming.change(false).is_some() {
            self.sensor.stream_off()?;
        }
        self.is_receiving.change(true);
        seninf::mux_on(&mut self.isp.registers);
        if !seninf::receiver_on(&mut self.isp.registers, self.trim) {
            return Err(io::Error::other(NO_CALIBRATION));
        }
        self.sensor.mode(mode)?;
        if pace.overrides_driver(mode) {
            self.sensor.frame_lines(pace.frame_lines)?;
        }
        self.expose(exposure, pace)
    }

    fn expose(&mut self, exposure: Exposure, pace: Pace) -> io::Result<()> {
        self.timeout = stream::timeout(exposure.frame_micros(pace));
        self.sensor.expose(exposure.lines)?;
        self.sensor.gain(exposure.gain)
    }

    pub(super) fn start_sensor(&mut self, frame: Frame) -> io::Result<()> {
        self.is_sensor_streaming.change(true);
        self.sensor.stream_on()?;
        let health = seninf::await_stream(&mut self.isp.registers, self.timeout);
        health.to_fault(frame).map_or(Ok(()), |fault| Err(io::Error::other(fault)))
    }

    fn start_camsv(&mut self, frame: Frame, slots: impl Iterator<Item = u32>, header: u32) -> io::Result<()> {
        camsv::set_window(&mut self.isp.registers, frame);
        self.isp.clear_bookkeeping()?;
        self.is_armed.change(true);
        camsv::arm(&mut self.isp.registers, frame).map_err(to_error)?;
        for slot in slots {
            camsv::queue(&mut self.isp.registers, frame, slot, header).map_err(to_error)?;
        }
        camsv::clocks_on(&mut self.isp.registers).map_err(to_error)?;
        self.isp.view_finder(true)
    }

    pub(super) fn start_stream<'s>(
        &mut self,
        slots: &'s Frames,
        finder: &'s Finder,
        auto: &Auto,
    ) -> io::Result<Stream<'s>> {
        self.start_camsv(slots.frame(), (0..SLOTS).map(|slot| slots.address(slot)), slots.header())?;
        let (width, height) = finder.size();
        Ok(Stream {
            slots,
            finder,
            queue: BufferQueue::all(SLOTS),
            tone: Tone::new(auto.recipe()),
            picture: vec![0; width * height],
            sync: FrameSync::new(auto.pace(), self.refresh_ns),
        })
    }

    // Captures one frame into each of `addresses` in turn, as many queued at once as a stream queues.
    pub(super) fn capture(&mut self, frame: Frame, addresses: &[u32], header: u32) -> io::Result<()> {
        let mut capture = Capture::new(addresses.len(), SLOTS);
        self.start_camsv(frame, addresses[..capture.queued()].iter().copied(), header)?;
        loop {
            match capture.step(camsv::counts(&mut self.isp.registers)) {
                CaptureStep::Queue(index) => {
                    camsv::queue(&mut self.isp.registers, frame, addresses[index], header).map_err(to_error)?;
                }
                CaptureStep::Wait => self.wait_frame()?,
                CaptureStep::Look => {}
                CaptureStep::Done => break,
            }
        }
        self.halt();
        Ok(())
    }

    // The ION the camera's node opened above itself, for the buffers CAMSV0 writes.
    pub(super) const fn ion(&self) -> &Ion {
        &self.ion
    }

    pub(super) fn rest(&mut self) -> io::Result<()> {
        self.halt();
        if self.is_sensor_streaming.change(false).is_some() {
            self.sensor.stream_off()?;
        }
        Ok(())
    }

    fn queue(&mut self, slots: &Frames, slot: usize) -> io::Result<()> {
        camsv::queue(&mut self.isp.registers, slots.frame(), slots.address(slot), slots.header()).map_err(to_error)
    }

    pub(super) fn next_full(&mut self, stream: &mut Stream, stats: &mut Stats) -> io::Result<Option<FullSlot>> {
        let counts = camsv::counts(&mut self.isp.registers);
        let count = counts.full(stream.queue.len());
        let Some((slot, stale_slots)) = stream.queue.take(count) else {
            self.wait_frame()?;
            return Ok(None);
        };
        for stale_slot in stale_slots {
            self.queue(stream.slots, stale_slot)?;
            stats.note_skipped();
        }
        Ok(Some(FullSlot { slot, count, drops: counts.drops }))
    }

    pub(super) fn draw_frame<'s>(
        &mut self,
        shared: &Shared,
        stream: &mut Stream<'s>,
        slot: usize,
        stats: &mut Stats,
    ) -> io::Result<DrawnFrame<'s>> {
        let slots: &'s Frames = stream.slots;
        let start = Instant::now();
        let raw = slots.bytes(slot)?;
        stats.note_refreshed(start.elapsed());
        let (width, height) = stream.finder.size();
        stream.picture.resize(width * height, 0);
        let measure = draw(self.crew, stream.finder, raw, &stream.tone, &mut stream.picture);
        let elapsed = start.elapsed();
        let flip_ns = shared.board.lock().flipped_ns;
        if let Some(lines) = stream.sync.take_ready(sys::monotonic_ns(), flip_ns) {
            self.sensor.frame_lines(lines)?;
        }
        Ok(DrawnFrame { slot, raw, measure, elapsed })
    }

    pub(super) fn requeue(
        &mut self,
        stream: &mut Stream,
        drawn: DrawnFrame<'_>,
        auto: &mut Auto,
        stats: &mut Stats,
    ) -> Result<bool, Stop> {
        self.queue(stream.slots, drawn.slot)?;
        stream.queue.put(drawn.slot);
        if let Some(exposure) = auto.take_measure(&drawn.measure) {
            self.expose(exposure, auto.pace())?;
        }
        let is_tone_changed = stream.tone.update(auto.recipe());
        stream.sync.note_frame();
        if stats.note_frame(drawn.measure, drawn.elapsed) {
            self.check_warmth()?;
        }
        Ok(is_tone_changed)
    }

    // Stops the stream where the kernel tells the board too warm; a warmth that cannot be read is told in the log once, until it reads again.
    fn check_warmth(&mut self) -> Result<(), Stop> {
        let mut word = [0u8; WARM_BYTES];
        let read = File::open(WARMTH_FILE).and_then(|mut file| file.read(&mut word));
        if self.is_warmth_read.change(read.is_ok()).is_some()
            && let Err(e) = &read
        {
            log!("camera: the board's warmth is not read, and the stream goes on unwatched: {e}");
        }
        if is_too_warm(read.ok().map(|count| &word[..count])) {
            return Err(Stop { error: io::Error::other(TOO_WARM_TEXT), outcome: Some(Outcome::TooWarm) });
        }
        Ok(())
    }

    fn timeout_ms(&self) -> u32 {
        time::timeout_ms(self.timeout).unsigned_abs()
    }

    fn wait_frame(&mut self) -> io::Result<()> {
        match self.isp.wait(camsv::FRAME_DONE, self.timeout_ms())? {
            true => Ok(()),
            false => Err(io::Error::other(format!("no frame for {} ms", self.timeout_ms()))),
        }
    }

    pub(super) fn halt(&mut self) {
        if self.is_armed.change(false).is_none() {
            return;
        }
        if camsv::is_streaming(&mut self.isp.registers) {
            if let Err(e) = self.isp.view_finder(false) {
                log!("camera: stopping the stream: {e}");
            }
            let mut rest = Rest::default();
            loop {
                match self.isp.wait(camsv::VSYNC, self.timeout_ms()) {
                    Ok(true) if rest.take_vsync(camsv::is_idle(&mut self.isp.registers)) => {}
                    Ok(true) => break,
                    Ok(false) => {
                        log!("{NO_VSYNC_LINE}");
                        break;
                    }
                    Err(e) => {
                        log!("camera: waiting for CAMSV0 to come to rest: {e}");
                        break;
                    }
                }
            }
        }
        camsv::disarm(&mut self.isp.registers);
        if let Err(e) = self.isp.clear_bookkeeping() {
            log!("camera: clearing the kernel's frame counters: {e}");
        }
        if let Err(e) = camsv::shut(&mut self.isp.registers) {
            log!("camera: {}", e.message());
        }
    }

    pub(super) fn power_down(&mut self) {
        self.halt();
        if self.is_sensor_streaming.change(false).is_some()
            && let Err(e) = self.sensor.stream_off()
        {
            log!("camera: suspending the sensor: {e}");
        }
        if self.is_receiving.change(false).is_some() {
            seninf::receiver_off(&mut self.isp.registers);
            seninf::analog_to_reset(&mut self.isp.registers);
        }
        if let Err(e) = self.sensor.power_off() {
            log!("camera: sensor power off: {e}");
        }
    }

    // Brings the camera up for the stream of `viewfinder`: power, its mode, the stream's slots, and the finder that draws its view in `format`.
    pub(super) fn bring_up<'s>(
        &mut self,
        swivel: &mut Swivel,
        (viewfinder, format): (Viewfinder, PixelFormat),
        auto: &Auto,
        slots: &'s mut Option<Frames>,
    ) -> io::Result<(&'s Frames, Finder)> {
        self.power_up(swivel, viewfinder.facing)?;
        let mode = viewfinder.mode();
        let frame = mode.frame();
        self.configure(mode, auto.exposure(), auto.pace())?;
        let slots = &*slots.insert(Frames::new(self.ion(), frame, SLOTS)?);
        let ((width, height), turn) = (viewfinder.frame_size(), viewfinder_turn(viewfinder.facing));
        let finder = Finder::new(frame, viewfinder.view(), width, height, turn, format)
            .ok_or_else(|| io::Error::other(NO_FINDER))?
            .with_lens(mode.lens());
        Ok((slots, finder))
    }

    fn power_up(&mut self, swivel: &mut Swivel, facing: Facing) -> io::Result<()> {
        self.isp.translate()?;
        // The turn ends before the sensor's rails come on: both inrushes at once pull a nearly empty cell under the PMIC's 2.6 V undervoltage lockout.
        swivel.begin(angle(facing))?.finish()?;
        self.sensor.master_clock(true)?;
        seninf::master_clock(&mut self.isp.registers);
        self.sensor.power_on()
    }
}
