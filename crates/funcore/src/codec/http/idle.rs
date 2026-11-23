use super::Target;
use alloc::vec::Vec;

/// An open connection: `D` tells its stay, `S` is the connection.
pub struct Idle<D, S> {
    pub stay: D,
    target: Target,
    stream: S,
}

impl<D, S> Idle<D, S> {
    pub const fn new(stay: D, target: Target, stream: S) -> Idle<D, S> {
        Idle { stay, target, stream }
    }
}

/// Takes the open connection of that stay to the server a target names.
pub fn take_idle<D: PartialEq, S>(idle: &mut Vec<Idle<D, S>>, target: &Target, stay: &D) -> Option<S> {
    let at = idle.iter().position(|idle| idle.stay == *stay && target.is_same_server(&idle.target))?;
    Some(idle.swap_remove(at).stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn take_by_stay_and_server() {
        let target = Target::parse("https://example.org/a").unwrap();
        let elsewhere = Target::parse("https://example.net/a").unwrap();
        let mut idle = vec![Idle::new(1, target.clone(), 'a'), Idle::new(2, target.clone(), 'b')];
        assert_eq!(take_idle(&mut idle, &elsewhere, &1), None, "another server");
        assert_eq!(take_idle(&mut idle, &target, &3), None, "another stay");
        assert_eq!(take_idle(&mut idle, &target, &2), Some('b'));
        assert_eq!(idle.len(), 1);
        assert_eq!(take_idle(&mut idle, &target, &2), None, "once");
    }
}
