const WORD_BITS: u64 = u64::BITS as u64;
// As many bits as the reference implementations keep (`COUNTER_BITS_TOTAL`).
const WORDS: usize = 8192 / WORD_BITS as usize;
const REACH: u64 = (WORDS as u64 - 1) * WORD_BITS;

// The counters a session has taken, by RFC 6479: the highest, and a bit for each of the `REACH` below it.
pub(super) struct Window {
    top: u64,
    seen: [u64; WORDS],
}

impl Default for Window {
    fn default() -> Window {
        Window { top: 0, seen: [0; WORDS] }
    }
}

impl Window {
    // Whether `take` would take a counter, taking nothing: SRTP asks before it opens a packet (RFC 3711, 3.3).
    pub(super) fn is_new(&self, counter: u64) -> bool {
        if counter.saturating_add(REACH) < self.top {
            return false;
        }
        let (place, bit) = Window::place(counter);
        counter > self.top || self.seen[place] & bit == 0
    }

    // Whether a counter was taken and lies within reach of the highest: a video frame's assembler asks it of a frame referred to.
    pub(super) fn has(&self, counter: u64) -> bool {
        let (place, bit) = Window::place(counter);
        counter <= self.top && counter.saturating_add(REACH) >= self.top && self.seen[place] & bit != 0
    }

    // Takes a counter: false for one taken before, or too far below the highest to tell.
    pub(super) fn take(&mut self, counter: u64) -> bool {
        if !self.is_new(counter) {
            return false;
        }
        if counter > self.top {
            let (word, top_word) = (counter / WORD_BITS, self.top / WORD_BITS);
            for step in 1..=(word - top_word).min(WORDS as u64) {
                self.seen[((top_word + step) % WORDS as u64) as usize] = 0;
            }
            self.top = counter;
        }
        let (place, bit) = Window::place(counter);
        self.seen[place] |= bit;
        true
    }

    // The word of `seen` a counter's bit lies in, and the bit.
    fn place(counter: u64) -> (usize, u64) {
        (((counter / WORD_BITS) % WORDS as u64) as usize, 1 << (counter % WORD_BITS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_once_and_within_reach() {
        let mut window = Window::default();
        assert!(window.take(0) && !window.take(0), "the first counter, once");
        assert!(window.take(1) && window.take(5) && window.take(3), "out of order within reach");
        assert!(!window.take(5) && !window.take(3) && window.take(2));
        assert!(window.take(REACH + 100), "a jump ahead");
        assert!(!window.take(99), "what lies too far below is not told from a replay");
        assert!(window.take(100) && !window.take(100), "the lowest still within reach");
        assert!(window.take(REACH + 99) && !window.take(REACH + 100));
        let far = 10 * WORDS as u64 * WORD_BITS + 7;
        assert!(window.take(far), "a jump past the whole window clears it");
        assert!(window.take(far - 1) && window.take(far - REACH) && !window.take(far - REACH - 1));
        assert!(window.take(u64::MAX) && !window.take(u64::MAX) && window.take(u64::MAX - REACH));
    }

    #[test]
    fn asking_takes_nothing() {
        let mut window = Window::default();
        assert!(window.is_new(0) && window.is_new(0) && window.take(0), "asked twice, still to be taken");
        assert!(!window.is_new(0), "once taken, no more");
        assert!(window.take(70) && window.is_new(69) && window.is_new(71), "below the highest and above it");
        assert!(window.is_new(REACH + 300) && window.is_new(70 + REACH), "a jump ahead is new");
        assert!(window.take(REACH + 300) && !window.is_new(299), "what lies too far below is not told from a replay");
        assert!(window.is_new(300) && !window.is_new(REACH + 300));
    }

    #[test]
    fn counters_held() {
        let mut window = Window::default();
        assert!(!window.has(0) && !window.has(5), "nothing taken");
        assert!(window.take(5) && window.has(5) && !window.has(4) && !window.has(6), "the one taken alone");
        assert!(window.take(REACH + 5) && window.has(5) && window.has(REACH + 5), "the lowest still within reach");
        assert!(window.take(REACH + 6) && !window.has(5), "out of reach is not held");
    }
}
