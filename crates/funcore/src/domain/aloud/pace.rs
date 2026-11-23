use core::time::Duration;

const RECONNECT: Duration = Duration::from_millis(2424);
const FIRST_VOICE: Duration = Duration::from_millis(1400);
pub(super) const LEAD_FACTOR: u32 = 2;
const WINDOW: usize = 5;

fn push_sample(samples: &mut [Duration; WINDOW], sample: Duration) {
    samples.copy_within(1.., 0);
    samples[WINDOW - 1] = sample;
}

fn longest(samples: &[Duration; WINDOW]) -> Duration {
    samples.iter().copied().max().unwrap_or(Duration::ZERO)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Pace {
    reconnects: [Duration; WINDOW],
    first_voices: [Duration; WINDOW],
}

impl Default for Pace {
    fn default() -> Pace {
        Pace { reconnects: [RECONNECT; WINDOW], first_voices: [FIRST_VOICE; WINDOW] }
    }
}

impl Pace {
    pub(super) fn add_reconnect(&mut self, duration: Duration) {
        push_sample(&mut self.reconnects, duration);
    }

    pub(super) fn add_first_voice(&mut self, duration: Duration) {
        push_sample(&mut self.first_voices, duration);
    }

    pub(super) fn reconnect(&self) -> Duration {
        longest(&self.reconnects)
    }

    fn lead(&self, has_link: bool) -> Duration {
        let margin = longest(&self.first_voices) * LEAD_FACTOR;
        if has_link { margin } else { self.reconnect() + margin }
    }

    // Whether the session is kept for the next passage, and how long until that passage is asked for.
    pub(super) fn plan_wait(
        &self,
        has_link: bool,
        voice_left: Duration,
        has_more: bool,
        is_held: bool,
    ) -> (bool, Duration) {
        let should_keep_link = has_more && !is_held && voice_left <= self.lead(false);
        (should_keep_link, voice_left.saturating_sub(self.lead(has_link && should_keep_link)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(count: u64) -> Duration {
        Duration::from_millis(count)
    }

    #[test]
    fn lead_times() {
        let pace = Pace::default();
        assert_eq!((pace.lead(true), pace.lead(false)), (ms(2800), ms(5224)));
    }

    #[test]
    fn longest_of_window() {
        let mut pace = Pace::default();
        pace.add_first_voice(ms(3000));
        assert_eq!(pace.lead(true), ms(6000));
        for _ in 0..4 {
            pace.add_first_voice(ms(500));
        }
        assert_eq!(pace.lead(true), ms(6000));
        pace.add_first_voice(ms(500));
        assert_eq!(pace.lead(true), ms(1000));
    }

    #[test]
    fn link_kept_on_short_voice() {
        let pace = Pace::default();
        let table = [
            ((true, ms(5224), true, false), (true, ms(2424))),
            ((true, ms(5225), true, false), (false, ms(1))),
            ((false, ms(4000), true, false), (true, ms(0))),
            ((true, ms(4000), false, false), (false, ms(0))),
            ((true, ms(4000), true, true), (false, ms(0))),
        ];
        for ((has_link, voice_left, has_more, is_held), (should_keep_link, ask_in)) in table {
            assert_eq!(pace.plan_wait(has_link, voice_left, has_more, is_held), (should_keep_link, ask_in));
        }
    }
}
