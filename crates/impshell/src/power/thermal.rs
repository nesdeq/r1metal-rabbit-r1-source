use crate::log;
use r1_funcore::hw::power::thermal::{self, Files, POLICY};
use std::fs;

struct Kernel;

impl Files for Kernel {
    fn write(&mut self, file: &str, text: &str) -> bool {
        fs::write(file, text).inspect_err(|e| log!("thermal: {file}: {e}")).is_ok()
    }
}

/// Writes `POLICY` and logs every setting the kernel lacks or refuses.
pub fn apply() {
    log!("{}", thermal::apply(POLICY, &mut Kernel));
}
