//! TCP as the kernel does it for the device's connections: what it is told once a boot.

/// What a boot writes of the kernel's TCP settings: a connection that paused sends on at the pace it had, where the kernel's own has it begin slowly again after a pause longer than its retransmission timeout.
pub const BOOT_WRITES: [(&str, &str); 1] = [("tcp_slow_start_after_idle", "0")];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pace_kept_after_pause() {
        assert_eq!(BOOT_WRITES, [("tcp_slow_start_after_idle", "0")], "the kernel's name of it, switched off");
    }
}
