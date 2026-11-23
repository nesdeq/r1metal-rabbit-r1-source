use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::IpAddr;

// Each name's addresses, and the second since the device started until which they may be kept.
#[derive(Default)]
pub(super) struct Cache(BTreeMap<String, (Vec<IpAddr>, u64)>);

impl Cache {
    pub(super) const fn new() -> Cache {
        Cache(BTreeMap::new())
    }

    /// A name's addresses and the seconds they may be kept for from `now`, if they may still be.
    pub fn get(&self, name: &str, now: u64) -> Option<(&[IpAddr], u64)> {
        let (addresses, until) = self.0.get(name).filter(|(_, until)| *until > now)?;
        Some((addresses, until - now))
    }

    /// Keeps a name's addresses from `now` for the seconds its answer allows, and lets go of every name whose time is over; what it allows no time is not kept.
    pub fn insert(&mut self, name: &str, addresses: &[IpAddr], seconds: u32, now: u64) {
        self.0.retain(|_, (_, until)| *until > now);
        if seconds > 0 {
            self.0.insert(name.into(), (addresses.to_vec(), now + u64::from(seconds)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_lifetime() {
        let (mut cache, addresses) = (Cache::new(), [IpAddr::from([192, 0, 2, 1])]);
        cache.insert("a.example", &addresses, 60, 100);
        cache.insert("b.example", &addresses, 0, 100);
        assert_eq!(cache.get("a.example", 100), Some((&addresses[..], 60)));
        assert_eq!(cache.get("a.example", 159), Some((&addresses[..], 1)));
        assert_eq!(cache.get("a.example", 160), None, "not past the time its answer allows");
        assert_eq!(cache.get("b.example", 100), None, "what an answer lets nobody keep is not kept");
        assert_eq!(cache.get("c.example", 0), None);
        cache.insert("c.example", &addresses, 60, 160);
        assert_eq!(cache.0.len(), 1, "a name whose time is over is let go of as another is kept");
    }
}
