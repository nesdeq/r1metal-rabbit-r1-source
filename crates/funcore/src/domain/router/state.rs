use super::Standing;
use super::forward::Machine;
use crate::net::dns::nat64::Prefix;
use crate::net::wireguard::config::Config;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, Ipv6Addr};
use core::time::Duration;

/// Who hands the router a config.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Asker {
    Console,
    Window,
}

/// An order to the router's thread, done in order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Order {
    // Keeps a config in place of any before it.
    Keep(Config, Asker),
    Forget,
}

/// What a thread beside the forwarder found for the stay of that number.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Answer {
    // The addresses of the tunnel's peer, in the order to try them.
    Endpoint(u64, Result<Vec<IpAddr>, String>),
    // A network of IPv6 alone: the address the kernel holds in it and its NAT64's prefix, none where it tells none.
    Translation(u64, Result<Option<(Ipv6Addr, Prefix)>, String>),
}

/// What the main loop hands the forwarder since it last looked.
#[derive(Default)]
pub struct Wishes {
    pub(super) is_wanted: bool,
    pub(super) uses_tunnel: bool,
    pub(super) orders: Vec<Order>,
    pub(super) answers: Vec<Answer>,
}

/// The router's report.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Report {
    /// How the router stands, where that changed.
    pub standing: Option<Standing>,
    /// Whether a config of a tunnel is kept, where that changed.
    pub has_config: Option<bool>,
    /// What keeping a config came to, with the reason it failed, and who handed it over.
    pub store: Option<(Asker, Result<(), String>)>,
}

/// The state the router's threads and the main loop share.
#[derive(Default)]
pub struct State {
    is_wanted: bool,
    uses_tunnel: bool,
    orders: Vec<Order>,
    answers: Vec<Answer>,
    report: Report,
    standing: Option<Standing>,
    has_config: Option<bool>,
    is_routing: bool,
}

impl State {
    /// Sets whether the device is a modem for a host, and whether the host's packets go through the tunnel alone; true where that is news for the forwarder.
    pub fn want(&mut self, is_wanted: bool, uses_tunnel: bool) -> bool {
        let is_news = (self.is_wanted, self.uses_tunnel) != (is_wanted, uses_tunnel);
        (self.is_wanted, self.uses_tunnel) = (is_wanted, uses_tunnel);
        is_news
    }

    /// Queues an order.
    pub fn order(&mut self, order: Order) {
        self.orders.push(order);
    }

    /// Queues what a thread beside the forwarder found.
    pub fn add_answer(&mut self, answer: Answer) {
        self.answers.push(answer);
    }

    /// Takes what the forwarder is to follow.
    pub fn take_wishes(&mut self) -> Wishes {
        Wishes {
            is_wanted: self.is_wanted,
            uses_tunnel: self.uses_tunnel,
            orders: core::mem::take(&mut self.orders),
            answers: core::mem::take(&mut self.answers),
        }
    }

    /// Notes how the forwarder stands at `now`; true where that is news for the main loop.
    pub fn note(&mut self, machine: &mut Machine, now: Duration) -> bool {
        let (standing, has_config, store) = (machine.standing(now), machine.has_config(), machine.take_stored());
        let was_routing = core::mem::replace(&mut self.is_routing, machine.is_routing());
        let mut is_news = was_routing != self.is_routing || store.is_some();
        if self.standing.replace(standing) != Some(standing) {
            (self.report.standing, is_news) = (Some(standing), true);
        }
        if self.has_config.replace(has_config) != Some(has_config) {
            (self.report.has_config, is_news) = (Some(has_config), true);
        }
        self.report.store = store.or(self.report.store.take());
        is_news
    }

    /// Notes that the thread ended: nothing is routed from then on.
    pub fn end_thread(&mut self) {
        (self.is_routing, self.standing, self.report.standing) =
            (false, Some(Standing::Absent), Some(Standing::Absent));
    }

    /// Takes the report.
    pub fn take_report(&mut self) -> Report {
        core::mem::take(&mut self.report)
    }

    /// True while a host's link has every stay made over the modem and routed.
    pub const fn is_routing(&self) -> bool {
        self.is_routing
    }

    /// True while no host is served and no order waits.
    pub fn is_off(&self) -> bool {
        !self.is_routing && self.orders.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::wireguard::config::tests::config;
    use crate::net::wireguard::tests::Toy;

    const NOW: Duration = Duration::from_secs(9);

    #[test]
    fn wishes_and_orders() {
        let mut state = State::default();
        assert!(!state.want(false, false), "as a new device has it");
        assert!(state.want(true, false) && !state.want(true, false) && state.want(true, true));
        state.order(Order::Keep(config(), Asker::Console));
        state.order(Order::Forget);
        state.add_answer(Answer::Endpoint(3, Err("no such name".into())));
        assert!(!state.is_off(), "an order waits");
        let wishes = state.take_wishes();
        assert!(wishes.is_wanted && wishes.uses_tunnel);
        assert_eq!(wishes.orders, [Order::Keep(config(), Asker::Console), Order::Forget]);
        assert_eq!(wishes.answers, [Answer::Endpoint(3, Err("no such name".into()))]);
        let again = state.take_wishes();
        assert!(again.is_wanted && again.orders.is_empty() && again.answers.is_empty(), "orders and answers once");
        assert!(state.is_off());
    }

    #[test]
    fn report_of_changes() {
        let (crypto, mut state) = (Toy::default(), State::default());
        let mut machine = Machine::new([2, 0, 0, 0, 0, 9], None);
        assert!(state.note(&mut machine, NOW), "how it stands at first is news");
        assert_eq!(state.take_report(), Report { standing: Some(Standing::Off), has_config: Some(false), store: None });
        assert!(!state.note(&mut machine, NOW) && state.take_report() == Report::default(), "told once");
        let wishes = Wishes { is_wanted: true, ..Wishes::default() };
        machine.take_wishes(&crypto, wishes, NOW);
        machine.take_link(&crypto, true, NOW);
        assert!(state.note(&mut machine, NOW) && state.is_routing() && !state.is_off());
        assert_eq!(state.take_report().standing, Some(Standing::Joining));
        machine.take_link(&crypto, false, NOW);
        assert!(state.note(&mut machine, NOW) && !state.is_routing(), "the main loop tells the radio");
        assert_eq!(state.take_report().standing, Some(Standing::Alone));
        state.end_thread();
        assert_eq!(state.take_report().standing, Some(Standing::Absent));
    }
}
