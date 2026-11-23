//! Automatic exposure and white balance: from what the frames measure to what the sensor is set to and how its colours are weighed.

use super::{BLUE, GREEN, RED, UNITY};
use crate::base::time::MICROS_PER_SECOND;
use crate::hw::camera::balance;
use crate::hw::camera::balance::Estimate;
use crate::hw::camera::colour;
use crate::hw::camera::colour::DAYLIGHT;
use crate::hw::camera::frame::BLACK;
use crate::hw::camera::frame::WHITE;
use crate::hw::camera::measure::Measure;
use crate::hw::camera::measure::STEP_CODES;
use crate::hw::camera::mode::Mode;
use crate::hw::camera::tone::Recipe;
use alloc::format;
use alloc::string::String;

pub(super) const PIXEL_CLOCK_HZ: u64 = 288_000_000;
pub(super) const LINE_LENGTH: u64 = 3800;
pub(super) const FRAMES_PER_SECOND: u32 = 60;
// The frames a second of a call's stream, as many as its far side is sent pictures.
pub(crate) const CALL_FRAMES_PER_SECOND: u32 = 30;
const MAINS_HZ: u64 = 50;
const FLICKER_LINES: u32 = (PIXEL_CLOCK_HZ / (2 * MAINS_HZ) / LINE_LENGTH + 1) as u32;
const MIN_LINES: u32 = 6;
const FRAME_MARGIN: u32 = 6;
const GAIN_UNIT: u32 = 64;
const GAIN_TOP: u32 = 16 * GAIN_UNIT;
const GAIN_STEP: u32 = 4;
const LONG_GAIN: u32 = 4 * GAIN_UNIT;
const LIFT_UNIT: u32 = UNITY as u32;
const LIFT_TOP: u32 = 8 * LIFT_UNIT;
pub(super) const TARGET: u32 = (WHITE - BLACK) * 18 / 100;
const TARGET_HIGH: u32 = 2 * TARGET;
const BRIGHT_ONE_IN: u32 = 100;
const BRIGHT_AIM: u32 = (WHITE - BLACK) * 85 / 100;
const DEAD_BAND: u32 = 8;
const STEP_LIMIT: u64 = 4;
const SETTLE_FRAMES: u32 = 3;
const WHITE_EVERY: u32 = 4;
const SLOWNESS: i32 = 8;
const DARK_ONE_IN: u32 = 200;
const VEIL_NUMERATOR: i64 = 4;
const VEIL_DENOMINATOR: i64 = 5;
const VEIL_MAX: i32 = UNITY / 32;
// HDR+ merges bursts of two to eight frames, taken up to eight times below the light the scene asks for.
const FEWEST_FRAMES: usize = 2;
/// The most frames a photo's burst takes.
pub const MOST_FRAMES: usize = 8;
const UNDER_TOP: u64 = 8 * UNITY as u64;

const fn lines_micros(lines: u32) -> u64 {
    lines as u64 * LINE_LENGTH * MICROS_PER_SECOND / PIXEL_CLOCK_HZ
}

/// How fast the frames of a stream come and how far an exposure may stretch them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pace {
    /// Lines of a frame while no exposure stretches it.
    pub frame_lines: u32,
    max_periods: u32,
}

impl Pace {
    const STILL: Pace = Pace { frame_lines: Mode::Capture.frame_lines(), max_periods: 6 };
    pub(crate) const LIVE: Pace =
        Pace { frame_lines: (PIXEL_CLOCK_HZ / FRAMES_PER_SECOND as u64 / LINE_LENGTH) as u32, max_periods: 1 };
    // A call's stream: the high speed mode's frames made as long as its rate takes, none stretched.
    pub(crate) const CALL: Pace =
        Pace { frame_lines: (PIXEL_CLOCK_HZ / CALL_FRAMES_PER_SECOND as u64 / LINE_LENGTH) as u32, max_periods: 1 };

    const fn frame_periods(self) -> u32 {
        (self.frame_lines - FRAME_MARGIN) / FLICKER_LINES
    }

    /// True where frames at this pace differ from the driver's table for the mode, so they are set past the driver.
    pub const fn overrides_driver(self, mode: Mode) -> bool {
        self.frame_lines != mode.frame_lines()
    }

    pub(super) const fn start(self) -> Exposure {
        Exposure { lines: self.frame_periods() * FLICKER_LINES, gain: GAIN_UNIT }
    }

    pub(super) const fn clock(self) -> (u32, u32) {
        let frame = LINE_LENGTH * self.frame_lines as u64;
        let (mut divisor, mut remainder) = (PIXEL_CLOCK_HZ, frame);
        while remainder != 0 {
            (divisor, remainder) = (remainder, divisor % remainder);
        }
        ((frame / divisor) as u32, (PIXEL_CLOCK_HZ / divisor) as u32)
    }
}

/// What the sensor is set to: the exposure time in lines and the analog gain in 64ths.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Exposure {
    pub lines: u32,
    pub gain: u32,
}

impl Exposure {
    const fn total(self) -> u64 {
        self.lines as u64 * self.gain as u64
    }

    fn from_total(total: u64, pace: Pace) -> Exposure {
        let period = u64::from(FLICKER_LINES);
        if total < period * u64::from(GAIN_UNIT) {
            return Exposure { lines: ((total / u64::from(GAIN_UNIT)) as u32).max(MIN_LINES), gain: GAIN_UNIT };
        }
        let frame_periods = u64::from(pace.frame_periods());
        let short_periods = (total / (period * u64::from(GAIN_UNIT))).min(frame_periods);
        let periods = if total > short_periods * period * u64::from(LONG_GAIN) {
            (total / (period * u64::from(LONG_GAIN)))
                .clamp(frame_periods, u64::from(pace.max_periods).max(frame_periods))
        } else {
            short_periods
        };
        let gain = ((total / (periods * period)) as u32).clamp(GAIN_UNIT, GAIN_TOP);
        Exposure { lines: periods as u32 * FLICKER_LINES, gain: gain - gain % GAIN_STEP }
    }

    fn with_lift(total: u64, pace: Pace) -> (Exposure, u32) {
        let exposure = Exposure::from_total(total, pace);
        if exposure.gain < GAIN_TOP {
            return (exposure, LIFT_UNIT);
        }
        let lift = (total * u64::from(LIFT_UNIT) / exposure.total()).clamp(u64::from(LIFT_UNIT), u64::from(LIFT_TOP));
        (exposure, lift as u32)
    }

    /// Microseconds from one frame to the next at a pace: the driver stretches the frame for a longer exposure.
    pub const fn frame_micros(self, pace: Pace) -> u64 {
        let lines =
            if self.lines + FRAME_MARGIN > pace.frame_lines { self.lines + FRAME_MARGIN } else { pace.frame_lines };
        lines_micros(lines)
    }
}

fn apply_lift(balance: [i32; 3], lift: u32) -> [i32; 3] {
    balance.map(|gain| (i64::from(gain) * i64::from(lift) / i64::from(LIFT_UNIT)) as i32)
}

fn step_toward(current: i32, target: i32) -> i32 {
    let step = (target - current) / SLOWNESS;
    if step == 0 { target } else { current + step }
}

fn to_target(mean: u32, bright: Option<u32>) -> u32 {
    let Some(bright) = bright else {
        return TARGET;
    };
    (u64::from(mean) * u64::from(BRIGHT_AIM) / u64::from(bright.max(1)))
        .clamp(u64::from(TARGET), u64::from(TARGET_HIGH)) as u32
}

/// A photo's burst: each frame's setting at the still pace, the recipe, how many frames, and how far (in 1024ths) below the light the scene asks for they are taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Still {
    pub exposure: Exposure,
    pub pace: Pace,
    pub recipe: Recipe,
    pub frames: usize,
    pub under: i32,
}

/// Follows the frames of a stream and keeps exposure, white balance and veil right for them.
pub struct Auto {
    exposure: Exposure,
    pace: Pace,
    lift: u32,
    target_total: u64,
    // The brightest hundredth of the last settled frame's green, and the light the sensor gathered for it, lift aside.
    highlight: Option<(u32, u64)>,
    settle_frames: u32,
    balance: [i32; 3],
    white: [i32; 2],
    estimate: Option<Estimate>,
    white_age: u32,
    veil: i32,
    is_fresh: bool,
}

impl Auto {
    const fn new(exposure: Exposure, pace: Pace) -> Auto {
        let white = [colour::gain(DAYLIGHT.red), colour::gain(DAYLIGHT.blue)];
        Auto {
            exposure,
            pace,
            lift: LIFT_UNIT,
            target_total: exposure.total(),
            highlight: None,
            settle_frames: SETTLE_FRAMES,
            balance: [white[0], UNITY, white[1]],
            white,
            estimate: None,
            white_age: 0,
            veil: 0,
            is_fresh: true,
        }
    }

    /// The automatic of the viewfinders' streams, at their pace.
    pub const fn live() -> Auto {
        Auto::new(Pace::LIVE.start(), Pace::LIVE)
    }

    pub const fn exposure(&self) -> Exposure {
        self.exposure
    }

    pub const fn pace(&self) -> Pace {
        self.pace
    }

    pub(crate) const fn lift(&self) -> u32 {
        self.lift
    }

    const fn total(&self) -> u64 {
        self.exposure.total() * self.lift as u64 / LIFT_UNIT as u64
    }

    /// Tells the automatic the sensor streams anew at `pace`: its first frames do not count for the exposure, their white and veil are taken whole, and a pace that changed takes the light last aimed at.
    pub fn restart(&mut self, pace: Pace) {
        if pace != self.pace {
            (self.exposure, self.lift) = Exposure::with_lift(self.target_total, pace);
            self.pace = pace;
        }
        self.settle_frames = SETTLE_FRAMES;
        self.is_fresh = true;
    }

    pub(crate) fn to_log_line(&self) -> String {
        let white = self.estimate.map_or_else(
            || "the last frame looked at did not tell its white".into(),
            |estimate| format!("white {:?} with {} of {} zones grey", estimate.gains, estimate.grey, estimate.zones),
        );
        format!("{white}, balance {:?}, veil {}/1024, light {}", self.balance, self.recipe().veil, self.target_total)
    }

    /// The stream's recipe: the balance with the lift, the veil, and the sensor's matrix for the light.
    pub fn recipe(&self) -> Recipe {
        Recipe { gains: apply_lift(self.balance, self.lift), veil: self.veil, matrix: colour::matrix(self.balance) }
    }

    // How far below the asked light a burst is taken: as far as keeps the brightest hundredth under 85 % (all of the way where the stream saw it at the limit), and no further than merged frames make up for.
    fn under(&self, gain: u64, sensor_total: u64) -> u64 {
        let wanted = match self.highlight {
            Some((bright, _)) if bright + STEP_CODES > WHITE - BLACK => UNDER_TOP,
            Some((bright, total)) => {
                let still = u64::from(bright) * sensor_total / total.max(1);
                (still * u64::from(LIFT_UNIT) / u64::from(BRIGHT_AIM)).clamp(u64::from(LIFT_UNIT), UNDER_TOP)
            }
            None => u64::from(LIFT_UNIT),
        };
        let affordable =
            (MOST_FRAMES as u64 * u64::from(LIFT_UNIT) * u64::from(LIFT_UNIT) / gain.max(1)).max(u64::from(LIFT_UNIT));
        wanted.min(affordable)
    }

    /// The burst that gathers what the stream's last frame asked for: a frame's gain beyond the sensor's least, times how far below that light it is taken, in frames, the fewest two.
    pub fn still(&self) -> Still {
        let (normal, normal_lift) = Exposure::with_lift(self.target_total, Pace::STILL);
        let gain = u64::from(normal.gain) * u64::from(normal_lift) / u64::from(GAIN_UNIT);
        let under = self.under(gain, normal.total());
        let (exposure, lift) = Exposure::with_lift(self.target_total * u64::from(LIFT_UNIT) / under, Pace::STILL);
        let frames = (gain * under).div_ceil(u64::from(LIFT_UNIT) * u64::from(LIFT_UNIT)) as usize;
        Still {
            exposure,
            pace: Pace::STILL,
            recipe: Recipe { gains: apply_lift(self.balance, lift), ..self.recipe() },
            frames: frames.clamp(FEWEST_FRAMES, MOST_FRAMES),
            under: under as i32,
        }
    }

    fn update_balance(&mut self, measure: &Measure) {
        self.white_age += 1;
        if self.is_fresh || self.white_age >= WHITE_EVERY {
            self.white_age = 0;
            self.estimate = balance::find_white(measure, self.target_total);
            if let Some(estimate) = self.estimate {
                self.white = estimate.gains;
            }
        }
        let is_at_once = self.is_fresh && self.estimate.is_some();
        for (gain, white) in [(RED, self.white[0]), (BLUE, self.white[1])] {
            self.balance[gain] = if is_at_once { white } else { step_toward(self.balance[gain], white) };
        }
    }

    fn update_veil(&mut self, measure: &Measure) {
        let gains = apply_lift(self.balance, self.lift);
        let darkest = (0..3)
            .filter_map(|colour| Some(i64::from(measure.levels.dark(colour, DARK_ONE_IN)?) * i64::from(gains[colour])))
            .min();
        let Some(darkest) = darkest else {
            return;
        };
        let veil = ((darkest * VEIL_NUMERATOR / VEIL_DENOMINATOR / i64::from(WHITE - BLACK)) as i32).min(VEIL_MAX);
        self.veil = if self.is_fresh { veil } else { step_toward(self.veil, veil) };
    }

    /// Takes what a frame measured; the sensor's next setting where it changes (the lift may change with it or alone).
    pub fn take_measure(&mut self, measure: &Measure) -> Option<Exposure> {
        let means = measure.means()?;
        self.update_balance(measure);
        self.update_veil(measure);
        if self.settle_frames > 0 {
            self.settle_frames -= 1;
            return None;
        }
        self.is_fresh = false;
        let (bright, total) = (measure.levels.bright(BRIGHT_ONE_IN), self.total());
        self.highlight = bright.map(|bright| (bright, self.exposure.total()));
        let target = u64::from(to_target(means[GREEN], bright));
        let light = u64::from(means[GREEN]) * u64::from(self.lift) / u64::from(LIFT_UNIT);
        if light.abs_diff(target) <= target / u64::from(DEAD_BAND) {
            self.target_total = total;
            return None;
        }
        self.target_total = (total * target / light.max(1)).clamp(total / STEP_LIMIT, total * STEP_LIMIT);
        let (next, lift) = Exposure::with_lift(self.target_total, self.pace);
        if (next, lift) == (self.exposure, self.lift) {
            return None;
        }
        let sensor = (next != self.exposure).then_some(next);
        (self.exposure, self.lift, self.settle_frames) = (next, lift, SETTLE_FRAMES);
        sensor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::camera::colour::WHITES;
    use crate::hw::camera::colour::White;
    use crate::hw::camera::measure::STEP_CODES;
    use crate::hw::camera::measure::ZONES;
    use crate::hw::camera::measure::Zone;

    const START: Exposure = Pace::STILL.start();
    const DAYLIGHT_BALANCE: [i32; 3] = [colour::gain(DAYLIGHT.red), UNITY, colour::gain(DAYLIGHT.blue)];

    fn even(sums: [u32; 3]) -> Measure {
        Measure { zones: [[Zone { sums, quads: 4, clipped: 0 }; ZONES]; ZONES], ..Measure::default() }
    }

    fn scene(brightness_per_million: u64, exposure: Exposure, tint: [u32; 2]) -> Measure {
        let green = (exposure.total() * brightness_per_million / 1_000_000).min(u64::from(WHITE - BLACK)) as u32;
        even([green * tint[0] / 100, green, green * tint[1] / 100].map(|light| 4 * light))
    }

    fn greys(white: White) -> Measure {
        const GREEN: u32 = 4 * TARGET;
        even([GREEN * 512 / white.red as u32, GREEN, GREEN * 512 / white.blue as u32])
    }

    fn is_at_target(light: u32, target: u32) -> bool {
        light.abs_diff(target) <= target / DEAD_BAND + target / 16
    }

    fn with_levels(mut measure: Measure, dark: (u32, [usize; 3]), bright: (u32, u32)) -> Measure {
        measure.levels.quads = 1000;
        for (colour, code) in dark.1.into_iter().enumerate() {
            measure.levels.dark[colour][code] = dark.0;
        }
        measure.levels.green[(bright.1 / STEP_CODES) as usize] += bright.0;
        measure
    }

    fn within(gains: [i32; 3], white: White, by: i32) -> bool {
        gains[1] == UNITY && (gains[0] - 2 * white.red).abs() <= by && (gains[2] - 2 * white.blue).abs() <= by
    }

    fn run(auto: &mut Auto, brightness_per_million: u64, frames: u32) -> u32 {
        let mut measure = scene(brightness_per_million, auto.exposure(), [100, 100]);
        for _ in 0..frames {
            auto.take_measure(&measure);
            measure = scene(brightness_per_million, auto.exposure(), [100, 100]);
        }
        (u64::from(measure.means().unwrap()[1]) * u64::from(auto.lift()) / u64::from(LIFT_UNIT)) as u32
    }

    #[test]
    fn flicker_period() {
        assert_eq!(FLICKER_LINES, 758);
        assert_eq!(lines_micros(FLICKER_LINES), 10_001);
        assert_eq!(Pace::STILL.frame_periods(), 3);
        assert_eq!(START, Exposure { lines: 2274, gain: 64 });
        assert_eq!(TARGET, 172);
        assert_eq!(START.frame_micros(Pace::STILL), 33_329, "30 frames a second");
        assert_eq!(
            Exposure { lines: 4548, gain: 64 }.frame_micros(Pace::STILL),
            60_087,
            "a long exposure stretches the frame"
        );
    }

    #[test]
    fn viewfinder_frame_rate() {
        let pace = Pace::LIVE;
        assert_eq!((pace.frame_periods(), pace.start()), (1, Exposure { lines: 758, gain: 64 }));
        assert_eq!(pace.start().frame_micros(pace), 16_664);
        assert_eq!(pace.clock(), (7999, 480_000), "60.0075 frames a second in whole ticks");
        assert_eq!(Pace::STILL.clock(), (7999, 240_000));
        assert!(
            pace.overrides_driver(Mode::Preview)
                && pace.overrides_driver(Mode::SlimVideo)
                && !Pace::STILL.overrides_driver(Mode::Capture),
            "the viewfinders' frames are set past the driver, a photo's are its own"
        );
        let from_total = |lines: u64, gain: u64| Exposure::from_total(lines * gain, pace);
        assert_eq!(from_total(100, 64), Exposure { lines: 100, gain: 64 });
        assert_eq!(from_total(1516, 64), Exposure { lines: 758, gain: 128 }, "a second period would not fit the frame");
        assert_eq!(
            from_total(2274, 384),
            Exposure { lines: 758, gain: 1024 },
            "nor is the frame stretched past four times gain"
        );
        assert_eq!(from_total(4548, 4096), Exposure { lines: 758, gain: 1024 });
        let mut auto = Auto::new(pace.start(), pace);
        run(&mut auto, 0, 60);
        assert_eq!(auto.exposure(), Exposure { lines: 758, gain: 1024 });
        assert_eq!(auto.exposure().frame_micros(pace), 16_664);
        for brightness in [900u64, 5_000, 20_000, 300_000] {
            let light = run(&mut auto, brightness, 60);
            assert!(is_at_target(light, TARGET), "brightness {brightness} ends at {light} with {:?}", auto.exposure());
            assert_eq!(auto.lift(), LIFT_UNIT, "brightness {brightness} is within reach of time and gain");
        }
    }

    #[test]
    fn call_pace_keeps_light() {
        assert_eq!(
            (Pace::CALL.frame_lines, Pace::CALL.clock(), Pace::CALL.overrides_driver(Mode::HighSpeed)),
            (2526, (7999, 240_000), true),
            "a call's stream: the high speed mode's frames of 631 lines made 2526 long past the driver, 30.0037 a second"
        );
        assert_eq!(
            (Pace::CALL.frame_periods(), Pace::CALL.start().lines + FRAME_MARGIN <= Pace::CALL.frame_lines),
            (3, true),
            "three flicker periods long, and its longest exposure fits a frame with the sensor's margin"
        );
        let mut auto = Auto::live();
        run(&mut auto, 100, 60);
        let (live, lift) = (auto.exposure(), auto.lift());
        assert!(lift > LIFT_UNIT, "a dim scene the sensor cannot reach in a viewfinder's frame");
        auto.restart(Pace::CALL);
        let call = auto.exposure();
        assert_eq!(
            (auto.pace(), call.lines, auto.lift(), call.frame_micros(Pace::CALL)),
            (Pace::CALL, 3 * FLICKER_LINES, LIFT_UNIT, 33_329),
            "a call's frames gather it in three periods, with no lift, none stretched"
        );
        assert!(
            (u64::from(call.lines) * u64::from(call.gain)).abs_diff(auto.target_total)
                < u64::from(call.lines * GAIN_STEP),
            "the same light, to a step of gain"
        );
        auto.restart(Pace::LIVE);
        assert_eq!((auto.exposure(), auto.lift()), (live, lift), "and a viewfinder's take it back as they had it");
    }

    #[test]
    fn lift_makes_up_light() {
        let mut auto = Auto::live();
        assert_eq!((auto.lift(), auto.recipe().gains), (LIFT_UNIT, DAYLIGHT_BALANCE));
        let light = run(&mut auto, 100, 60);
        assert_eq!(auto.exposure(), Exposure { lines: 758, gain: 1024 }, "all the time a frame has and all the gain");
        assert!(
            (2 * LIFT_UNIT..3 * LIFT_UNIT).contains(&auto.lift()),
            "the sensor reaches 77 of 172, the lift does the rest: {}",
            auto.lift()
        );
        assert!(is_at_target(light, TARGET), "{light}");
        assert_eq!(auto.recipe().gains, apply_lift(DAYLIGHT_BALANCE, auto.lift()), "the lift lies on all three gains");
        let settled = auto.lift();
        run(&mut auto, 100, 30);
        assert_eq!(auto.lift(), settled, "and holds still");
        run(&mut auto, 20_000, 60);
        assert_eq!(auto.lift(), LIFT_UNIT, "light again, lift gone");
    }

    #[test]
    fn still_frame() {
        let mut auto = Auto::new(Exposure { lines: 758, gain: 384 }, Pace::LIVE);
        auto.take_measure(&scene(990, START, [200, 50]));
        assert_eq!(auto.target_total, 758 * 384);
        assert_eq!(
            auto.still(),
            Still {
                exposure: Exposure { lines: 2274, gain: 128 },
                pace: Pace::STILL,
                recipe: auto.recipe(),
                frames: 2,
                under: UNITY
            },
            "the same light in three times the time with a third of the gain, at twice the sensor's least gain: two frames"
        );
        let mut dark = Auto::live();
        run(&mut dark, 0, 60);
        assert_eq!(
            (dark.exposure(), dark.lift(), dark.recipe().gains),
            (Exposure { lines: 758, gain: 1024 }, LIFT_TOP, DAYLIGHT_BALANCE.map(|gain| 8 * gain))
        );
        let Still { exposure, recipe, frames, under, .. } = dark.still();
        assert_eq!(exposure, Exposure { lines: 4548, gain: 1024 }, "six times the time a frame of the stream has");
        assert_eq!((frames, under), (MOST_FRAMES, UNITY), "a dark scene takes all the frames, none below its light");
        assert_eq!(
            recipe.gains,
            apply_lift(DAYLIGHT_BALANCE, 5461),
            "the last frame asked for four times its light: less lift than the stream has, on six times the light"
        );
        assert_eq!(
            (recipe.veil, recipe.matrix),
            (dark.recipe().veil, dark.recipe().matrix),
            "veil and matrix are the stream's"
        );
    }

    #[test]
    fn burst_below_bright_light() {
        let settled = |brightness: u64, top: (u32, u32)| {
            let mut auto = Auto::new(START, Pace::STILL);
            for _ in 0..60 {
                let measure = scene(brightness, auto.exposure(), [100, 100]);
                let green = measure.means().unwrap()[1];
                auto.take_measure(&with_levels(
                    with_levels(measure, (0, [0; 3]), (1000 - top.0, green)),
                    (0, [0; 3]),
                    top,
                ));
            }
            (auto.still(), auto.target_total)
        };
        let (clipped, asked) = settled(5_000, (20, WHITE - BLACK));
        assert_eq!(
            (clipped.frames, clipped.under),
            (MOST_FRAMES, UNDER_TOP as i32),
            "highlights at the limit in daylight: eight times below the light, in eight frames"
        );
        let gathered = clipped.exposure.total() * UNDER_TOP / u64::from(LIFT_UNIT);
        assert!(
            gathered.abs_diff(asked) * 10 <= asked,
            "the frames gather an eighth of the light: {gathered} for {asked}"
        );
        let (below, _) = settled(5_000, (20, 600));
        assert!(below.under == UNITY && below.frames == FEWEST_FRAMES, "highlights well under the limit: {below:?}");
        let (dim, _) = settled(40, (20, WHITE - BLACK));
        assert_eq!(
            (dim.frames, dim.under),
            (MOST_FRAMES, UNITY),
            "where the gain is high the merge makes up for nothing more, and the highlights are given up"
        );
    }

    #[test]
    fn time_then_gain() {
        let from_total = |lines: u64, gain: u64| Exposure::from_total(lines * gain, Pace::STILL);
        let exact = [(100, 64), (757, 64), (758, 64), (1516, 64), (2274, 128), (2274, 256), (4548, 256), (4548, 512)];
        for (lines, gain) in exact {
            let expected = Exposure { lines: lines as u32, gain: gain as u32 };
            assert_eq!(
                from_total(lines, gain),
                expected,
                "up to a period by time alone, up to four times gain in a frame"
            );
        }
        assert_eq!(from_total(1, 64), Exposure { lines: 6, gain: 64 }, "the driver's shortest exposure");
        let one_period = Exposure { lines: 758, gain: 84 };
        assert_eq!(from_total(1000, 64), one_period, "one period, the rest by gain in steps of a 16th");
        assert_eq!(from_total(2274, 384), Exposure { lines: 3032, gain: 288 }, "then longer than a frame");
        let longest = Exposure { lines: 4548, gain: 1024 };
        assert_eq!(from_total(4548, 4096), longest, "the longest exposure at the highest gain");
    }

    #[test]
    fn exposure_converges() {
        for brightness in [40u64, 900, 5_000, 20_000, 300_000] {
            for start in [START, Exposure { lines: 6, gain: 64 }, Exposure { lines: 4548, gain: 1024 }] {
                let mut auto = Auto::new(start, Pace::STILL);
                let light = run(&mut auto, brightness, 60);
                assert!(
                    is_at_target(light, TARGET),
                    "brightness {brightness} from {start:?} ends at {light} with {:?}",
                    auto.exposure()
                );
                let settled = auto.exposure();
                run(&mut auto, brightness, 30);
                assert_eq!(auto.exposure(), settled, "brightness {brightness} from {start:?}");
            }
        }
    }

    #[test]
    fn exposure_limits() {
        let mut auto = Auto::new(START, Pace::STILL);
        run(&mut auto, 0, 60);
        assert_eq!((auto.exposure(), auto.lift()), (Exposure { lines: 4548, gain: 1024 }, LIFT_TOP));
        run(&mut auto, 100_000_000, 60);
        assert_eq!((auto.exposure(), auto.lift()), (Exposure { lines: 6, gain: 64 }, LIFT_UNIT));
    }

    #[test]
    fn settling_between_steps() {
        let mut auto = Auto::new(START, Pace::STILL);
        let dark = scene(1, START, [100, 100]);
        let changes: [bool; 9] = core::array::from_fn(|_| auto.take_measure(&dark).is_some());
        assert_eq!(changes, [false, false, false, true, false, false, false, true, false]);
        assert_eq!(auto.take_measure(&Measure::default()), None, "a frame that measured nothing changes nothing");
    }

    #[test]
    fn step_limit() {
        let mut auto = Auto::new(START, Pace::STILL);
        let black = scene(0, START, [100, 100]);
        let first = (0..4).find_map(|_| auto.take_measure(&black)).unwrap();
        assert_eq!(first.total(), 4 * START.total());
    }

    #[test]
    fn white_follows() {
        let mut auto = Auto::new(START, Pace::STILL);
        assert_eq!((auto.balance, auto.estimate), (DAYLIGHT_BALANCE, None), "daylight until a frame tells");
        let (lamp, shade) = (WHITES[1], WHITES[6]);
        auto.take_measure(&greys(lamp));
        assert!(within(auto.balance, lamp, 24), "{:?} for {lamp:?}", auto.balance);
        assert_eq!(auto.estimate.map(|estimate| (estimate.zones, estimate.grey)), Some((256, 256)));
        for _ in 0..SETTLE_FRAMES {
            auto.take_measure(&greys(lamp));
        }
        let settled = auto.balance;
        for _ in 0..WHITE_EVERY {
            auto.take_measure(&greys(shade));
        }
        let [red, _, blue] = auto.balance;
        assert!(
            red > settled[0] && red < 2 * shade.red - 200 && blue < settled[2] && blue > 2 * shade.blue + 200,
            "on its way: {:?}",
            auto.balance
        );
        for _ in 0..100 {
            auto.take_measure(&greys(shade));
        }
        assert!(within(auto.balance, shade, 24), "{:?} for {shade:?}", auto.balance);
        auto.restart(Pace::STILL);
        auto.take_measure(&greys(lamp));
        assert!(within(auto.balance, lamp, 24), "a stream that starts anew takes its white whole again");
    }

    #[test]
    fn log_line() {
        let mut auto = Auto::new(START, Pace::STILL);
        let line = auto.to_log_line();
        assert!(line.starts_with("the last frame looked at did not tell its white, balance "), "{line}");
        auto.take_measure(&greys(WHITES[1]));
        let line = auto.to_log_line();
        assert!(line.starts_with("white ") && line.contains(" with 256 of 256 zones grey, balance "), "{line}");
    }

    #[test]
    fn unreadable_light() {
        let mut auto = Auto::new(START, Pace::STILL);
        for _ in 0..20 {
            auto.take_measure(&greys(WHITES[1]));
        }
        let lamp = auto.balance;
        for _ in 0..20 {
            auto.take_measure(&scene(1000, START, [1000, 1]));
        }
        assert_eq!((auto.balance, auto.estimate), (lamp, None), "a frame of one strong colour");
        auto.take_measure(&even([100, 60, 40]));
        assert_eq!(auto.balance, lamp, "15 codes of green are too few");
        assert_eq!(auto.recipe().matrix, colour::matrix(lamp), "the matrix is the one for the balance");
    }

    #[test]
    fn darkest_part_as_veil() {
        let mut auto = Auto::new(START, Pace::STILL);
        assert_eq!(auto.recipe().veil, 0);
        let grey = scene(1000, START, [100, 100]);
        auto.take_measure(&with_levels(grey, (10, [5, 10, 8]), (0, 0)));
        assert_eq!(
            auto.recipe().veil,
            8,
            "green is the darkest once balanced: four fifths of 10 codes of 959, in 1024ths"
        );
        for _ in 0..SETTLE_FRAMES {
            auto.take_measure(&with_levels(grey, (10, [5, 10, 8]), (0, 0)));
        }
        auto.take_measure(&with_levels(grey, (10, [60, 60, 60]), (0, 0)));
        assert_eq!(
            auto.recipe().veil,
            8 + (VEIL_MAX - 8) / SLOWNESS,
            "a later frame moves it an eighth of the way, and toward no more than a 32nd of the range"
        );
        auto.take_measure(&grey);
        assert_eq!(auto.recipe().veil, 8 + (VEIL_MAX - 8) / SLOWNESS, "a frame without levels leaves it");
        assert_eq!(auto.still().recipe.veil, auto.recipe().veil);
    }

    #[test]
    fn dim_scene_target() {
        assert_eq!(
            (to_target(172, None), to_target(172, Some(344)), to_target(172, Some(952)), to_target(200, Some(600))),
            (TARGET, TARGET_HIGH, TARGET, 271)
        );
        assert_eq!((BRIGHT_AIM, TARGET_HIGH), (815, 344));
        let flat = |exposure: Exposure, top: u32| {
            let measure = scene(5_000, exposure, [100, 100]);
            let green = measure.means().unwrap()[1];
            with_levels(with_levels(measure, (0, [0; 3]), (1000 - top, green)), (0, [0; 3]), (top, WHITE - BLACK))
        };
        for (top, target) in [(0, TARGET_HIGH), (20, TARGET)] {
            let mut auto = Auto::new(START, Pace::STILL);
            let mut measure = flat(auto.exposure(), top);
            for _ in 0..60 {
                auto.take_measure(&measure);
                measure = flat(auto.exposure(), top);
            }
            let light = measure.means().unwrap()[1];
            assert!(is_at_target(light, target), "{top} of 1000 quads at the limit: {light} for {target}");
        }
    }
}
