//! Values a thread and the main loop hand each other.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// The last value told; a new value is told only where it differs.
pub struct Latch<T>(Option<T>);

impl<T> Default for Latch<T> {
    fn default() -> Latch<T> {
        Latch(None)
    }
}

impl<T: Clone + PartialEq> Latch<T> {
    /// A latch that has told `value`.
    pub const fn with(value: T) -> Latch<T> {
        Latch(Some(value))
    }

    /// `value`, where it differs from the last one told.
    pub fn change(&mut self, value: T) -> Option<T> {
        let is_new = self.0.as_ref() != Some(&value);
        is_new.then(|| {
            self.0 = Some(value.clone());
            value
        })
    }

    /// True where `value` was told last.
    pub fn is(&self, value: &T) -> bool {
        self.0.as_ref() == Some(value)
    }

    /// Clears the value, so the next one is told.
    pub fn forget(&mut self) {
        self.0 = None;
    }
}

/// A shared value a thread takes out to work on outside the lock; others wait for its return.
pub struct Loan<T>(Option<T>);

impl<T: Default> Default for Loan<T> {
    fn default() -> Loan<T> {
        Loan(Some(T::default()))
    }
}

impl<T> Loan<T> {
    /// The value, none while it is out.
    pub const fn get(&self) -> Option<&T> {
        self.0.as_ref()
    }

    /// The value in place, none while it is out.
    pub const fn get_mut(&mut self) -> Option<&mut T> {
        self.0.as_mut()
    }

    /// Takes the value out, none while it is out.
    pub const fn take(&mut self) -> Option<T> {
        self.0.take()
    }

    /// Returns the value.
    pub fn give(&mut self, value: T) {
        self.0 = Some(value);
    }
}

// Askings of one thing, one at a time: whether one is under way, who waits for it, how many have ended, and the failure of the last where it could not be made; `W` wakes a waiter.
pub(crate) struct Turns<E, W> {
    is_asking: bool,
    waiters: Vec<W>,
    end_count: u64,
    failure: Option<E>,
}

impl<E, W> Turns<E, W> {
    pub(crate) const fn new() -> Turns<E, W> {
        Turns { is_asking: false, waiters: Vec::new(), end_count: 0, failure: None }
    }

    // Takes the turn to ask: false while another has it.
    pub(crate) const fn take(&mut self) -> bool {
        let is_free = !self.is_asking;
        self.is_asking = true;
        is_free
    }

    // The count of ended askings, which an asker notes as it first looks.
    pub(crate) const fn end_count(&self) -> u64 {
        self.end_count
    }

    // The failure that spares an asker noted at `count`: an asking ended since, and could not be made.
    pub(crate) fn failure_since(&self, count: u64) -> Option<&E> {
        self.failure.as_ref().filter(|_| self.end_count != count)
    }

    pub(crate) fn add_waiter(&mut self, waiter: W) {
        self.waiters.push(waiter);
    }

    // Ends the asking, with why it could not be made where it could not; returns the waiters to wake.
    pub(crate) fn end(&mut self, failure: Option<E>) -> Vec<W> {
        (self.is_asking, self.failure, self.end_count) = (false, failure, self.end_count + 1);
        core::mem::take(&mut self.waiters)
    }
}

/// A thread's next step over its orders.
pub enum Step<T> {
    Serve(T),
    End,
}

/// A thread's queue of orders, closed to end it.
pub struct Orders<T> {
    queue: VecDeque<T>,
    is_closed: bool,
}

impl<T> Default for Orders<T> {
    fn default() -> Orders<T> {
        Orders { queue: VecDeque::new(), is_closed: false }
    }
}

impl<T> Orders<T> {
    /// Queues an order.
    pub fn add(&mut self, order: T) {
        self.queue.push_back(order);
    }

    /// Ends the thread once the queue is empty.
    pub const fn close(&mut self) {
        self.is_closed = true;
    }

    pub(crate) const fn is_closed(&self) -> bool {
        self.is_closed
    }

    /// The thread's next step, none while it waits.
    pub fn next_step(&mut self) -> Option<Step<T>> {
        self.queue.pop_front().map(Step::Serve).or(self.is_closed.then_some(Step::End))
    }
}

/// The main loop's requests to a thread that answers in order, the current target, and the newest answer.
pub struct Fetch<R, A> {
    pub orders: Orders<R>,
    target: Latch<Option<R>>,
    answer: Option<A>,
}

impl<R, A> Default for Fetch<R, A> {
    fn default() -> Fetch<R, A> {
        Fetch { orders: Orders::default(), target: Latch::default(), answer: None }
    }
}

impl<R, A> Fetch<R, A> {
    /// Replaces an untaken answer, and returns it.
    pub const fn post(&mut self, answer: A) -> Option<A> {
        self.answer.replace(answer)
    }

    /// Takes the newest answer.
    pub const fn take_answer(&mut self) -> Option<A> {
        self.answer.take()
    }
}

impl<R: Clone + PartialEq, A> Fetch<R, A> {
    /// Queues a request that differs from the last; true where one was queued.
    pub fn want(&mut self, request: Option<R>) -> bool {
        let Some(Some(request)) = self.target.change(request) else {
            return false;
        };
        self.orders.add(request);
        true
    }
}

#[derive(Default)]
pub(crate) struct Ticket(Option<u32>);

impl Ticket {
    pub(crate) fn want(&mut self, request: Option<u32>) -> bool {
        core::mem::replace(&mut self.0, request) != request
    }

    pub(crate) fn answer(&mut self, number: u32) {
        self.0 = self.0.filter(|out| *out != number);
    }
}

/// Whether a wanted and missing part was tried since it was last wanted.
#[derive(Default)]
pub struct Attempt(bool);

impl Attempt {
    /// True where the part is to be tried now: once for every request.
    pub const fn is_due(&mut self, is_wanted: bool, is_missing: bool) -> bool {
        let is_due = is_wanted && is_missing && !self.0;
        self.0 = is_wanted && (self.0 || is_due);
        is_due
    }
}

/// A screen's request to a part's thread, and the thread's state on it.
pub struct Request<T> {
    pub(crate) target: Option<T>,
    pub(crate) current: Option<T>,
    pub(crate) clicks: u32,
    pub(crate) is_busy: bool,
}

impl<T> Default for Request<T> {
    fn default() -> Request<T> {
        Request { target: None, current: None, clicks: 0, is_busy: false }
    }
}

impl<T: Copy + PartialEq> Request<T> {
    pub(crate) fn set(&mut self, target: Option<T>) -> bool {
        let is_new = self.target != target;
        if is_new && self.target.is_none() {
            self.clicks = 0;
        }
        self.target = target;
        is_new
    }

    pub(crate) const fn is_opened_by(&self, target: Option<T>) -> bool {
        self.target.is_none() && target.is_some()
    }

    /// `set` for a thread that takes each request at once: busy from the request, so the part stays on until the take.
    pub fn claim(&mut self, target: Option<T>) -> bool {
        let is_new = self.set(target);
        self.is_busy |= is_new && target.is_some();
        is_new
    }

    pub(crate) fn serve(&mut self) -> Option<T> {
        self.is_busy = self.target.is_some();
        self.target
    }

    /// The target, unless the thread began it last; without one the thread is no longer busy.
    pub fn next_new(&mut self) -> Option<T> {
        let next = self.target.filter(|request| self.current != Some(*request));
        self.is_busy &= next.is_some();
        next
    }

    /// Notes that the thread begins `request`.
    pub fn begin(&mut self, request: T) {
        self.current = Some(request);
        self.is_busy = true;
    }

    /// Counts a click on what is served.
    pub const fn click(&mut self) {
        self.clicks += 1;
    }

    // Takes the unhandled clicks.
    pub(crate) fn take_clicks(&mut self) -> u32 {
        core::mem::take(&mut self.clicks)
    }
}

// Swapped, so neither side allocates.
pub(crate) struct Handover<P, S> {
    picture: P,
    view: Option<S>,
    is_fresh: bool,
}

impl<P: Default, S> Default for Handover<P, S> {
    fn default() -> Self {
        Handover { picture: P::default(), view: None, is_fresh: false }
    }
}

impl<P, S: Copy> Handover<P, S> {
    /// Swaps in a picture of `view`; `picture` then holds the old one to draw over.
    pub fn put(&mut self, picture: &mut P, view: S) {
        core::mem::swap(&mut self.picture, picture);
        self.view = Some(view);
        self.is_fresh = true;
    }

    /// Swaps out a picture that came since the last look and `is_shown`; one passed over stays untaken.
    pub fn take(&mut self, picture: &mut P, is_shown: impl FnOnce(&S) -> bool) -> Option<S> {
        if !core::mem::take(&mut self.is_fresh) {
            return None;
        }
        let view = self.view.filter(is_shown)?;
        core::mem::swap(&mut self.picture, picture);
        Some(view)
    }

    pub(crate) const fn mark_stale(&mut self) {
        self.is_fresh = false;
    }

    // True while a picture put waits to be taken.
    pub(crate) const fn is_fresh(&self) -> bool {
        self.is_fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn latch_tells_changes() {
        let mut latch = Latch::default();
        assert_eq!((latch.change(3), latch.change(3), latch.change(4)), (Some(3), None, Some(4)));
        latch.forget();
        assert_eq!(latch.change(4), Some(4));
        let mut first = Latch::with(false);
        assert_eq!((first.change(false), first.change(true)), (None, Some(true)));
        let mut words = Latch::default();
        assert_eq!((words.change(String::from("a")), words.change(String::from("a"))), (Some(String::from("a")), None));
        assert_eq!(words.change(String::from("b")), Some(String::from("b")));
    }

    #[test]
    fn turns_one_at_a_time() {
        let mut turns: Turns<&str, u8> = Turns::new();
        let count = turns.end_count();
        assert!(turns.take() && !turns.take(), "one asks, the next waits");
        turns.add_waiter(1);
        turns.add_waiter(2);
        assert_eq!(turns.failure_since(count), None, "nobody has failed");
        assert_eq!(turns.end(Some("silent")), [1, 2], "the asking's end wakes whoever waits for it");
        assert_eq!(
            turns.failure_since(count),
            Some(&"silent"),
            "an asking ended while it waited, and could not be made"
        );
        let later = turns.end_count();
        assert_eq!(turns.failure_since(later), None, "whoever comes after it asks for itself");
        assert!(turns.take(), "the turn is free again");
        assert!(turns.end(None).is_empty());
        assert_eq!(turns.failure_since(later), None, "and an asking that was made spares nobody theirs");
    }

    #[test]
    fn orders_end_after_queue() {
        let mut orders = Orders::default();
        orders.add(1);
        assert!(matches!(orders.next_step(), Some(Step::Serve(1))) && orders.next_step().is_none());
        orders.add(2);
        orders.close();
        assert!(matches!(orders.next_step(), Some(Step::Serve(2))), "what was queued goes first");
        assert!(matches!(orders.next_step(), Some(Step::End)), "then the thread takes its end");
    }

    #[test]
    fn fetch_queues_new_requests() {
        let mut fetch = Fetch::<u8, &str>::default();
        assert!(fetch.want(Some(1)) && !fetch.want(Some(1)), "a request is queued once");
        assert!(!fetch.want(None), "wanting nothing queues nothing");
        assert!(fetch.want(Some(1)), "but the same request after it is new");
        assert!(matches!(fetch.orders.next_step(), Some(Step::Serve(1))));
        assert!(matches!(fetch.orders.next_step(), Some(Step::Serve(1))) && fetch.orders.next_step().is_none());
        assert_eq!((fetch.post("a"), fetch.post("b")), (None, Some("a")), "an answer not taken is replaced");
        assert_eq!((fetch.take_answer(), fetch.take_answer()), (Some("b"), None), "and the newest taken once");
    }

    #[test]
    fn ticket_wants_each_request_once() {
        let mut ticket = Ticket::default();
        assert!(!ticket.want(None) && ticket.want(Some(3)) && !ticket.want(Some(3)));
        assert!(ticket.want(Some(4)) && ticket.want(Some(3)));
        ticket.answer(2);
        assert!(!ticket.want(Some(3)));
        ticket.answer(3);
        assert!(ticket.want(Some(3)));
        assert!(ticket.want(None));
    }

    #[test]
    fn request_clicks_and_busy() {
        let mut request = Request::default();
        request.clicks += 1;
        assert!(request.is_opened_by(Some(1)) && !request.is_opened_by(None));
        assert!(request.set(Some(1)) && !request.set(Some(1)));
        assert!(!request.is_opened_by(Some(2)));
        assert_eq!(request.take_clicks(), 0);
        request.clicks += 1;
        assert!(request.set(Some(2)));
        assert!(request.clicks > 0);
        assert_eq!((request.take_clicks(), request.take_clicks()), (1, 0));
        assert_eq!((request.is_busy, request.serve(), request.is_busy), (false, Some(2), true));
        request.is_busy = false;
        assert!(request.set(None) && !request.is_busy && request.serve().is_none() && !request.is_busy);
        assert_eq!(request.target, None);
    }

    #[test]
    fn claimed_request_begins_once() {
        let mut request = Request::default();
        assert!(request.claim(Some(4)) && !request.claim(Some(4)));
        assert!(request.is_busy && request.current.is_none());
        assert_eq!((request.next_new(), request.is_busy), (Some(4), true));
        request.begin(4);
        assert_eq!((request.current, request.next_new(), request.is_busy), (Some(4), None, false));
        request.begin(4);
        assert!(request.claim(None) && request.is_busy);
        assert!(request.next_new().is_none() && !request.is_busy);
        assert!(request.claim(Some(4)) && request.is_busy);
        assert!(request.next_new().is_none() && !request.is_busy);
        assert!(request.claim(Some(5)) && request.next_new() == Some(5) && request.is_busy);
    }

    #[test]
    fn attempt_due_once_per_request() {
        let mut attempt = Attempt::default();
        assert!(!attempt.is_due(false, true));
        assert!(attempt.is_due(true, true) && !attempt.is_due(true, true));
        assert!(!attempt.is_due(true, false) && !attempt.is_due(true, true));
        assert!(!attempt.is_due(false, true) && attempt.is_due(true, true));
        let mut present = Attempt::default();
        assert!(!present.is_due(true, false) && present.is_due(true, true));
    }

    #[test]
    fn handover_swaps_once_for_its_view() {
        let mut handover = Handover::<Vec<u32>, u8>::default();
        let (mut drawn, mut shown) = (vec![1], vec![9]);
        assert_eq!(handover.take(&mut shown, |_| true), None);
        handover.put(&mut drawn, 3);
        assert!(drawn.is_empty());
        assert_eq!(handover.take(&mut shown, |view| *view == 4), None);
        assert_eq!(handover.take(&mut shown, |view| *view == 3), None);
        drawn = vec![2];
        handover.put(&mut drawn, 3);
        assert_eq!(drawn, [1]);
        assert_eq!((handover.take(&mut shown, |view| *view == 3), &shown[..]), (Some(3), &[2u32][..]));
        assert_eq!(handover.take(&mut shown, |_| true), None);
        handover.put(&mut drawn, 3);
        assert!(handover.is_fresh(), "a picture put waits");
        handover.mark_stale();
        assert!(!handover.is_fresh());
        assert_eq!(handover.take(&mut shown, |_| true), None);
    }
}
