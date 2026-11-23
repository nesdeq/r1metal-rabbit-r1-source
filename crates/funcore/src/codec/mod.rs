//! Layer 2: file and wire formats.

pub mod aac;
mod boolean;
mod edge;
pub mod form;
pub mod h264;
pub mod http;
pub(crate) mod ivf;
pub mod jpeg;
pub mod mp4;
pub(crate) mod opus;
pub(crate) mod picture;
pub mod qr;
pub mod vp8;
pub(crate) mod vp9;
pub(crate) mod zstd;

/// Why a file's index, a parameter set or a slice is unusable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flaw {
    Short,
    Invalid(&'static str),
    Broken,
    Unsupported(&'static str),
    Missing(&'static str),
}

impl core::fmt::Display for Flaw {
    fn fmt(&self, text: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Flaw::Short => text.write_str("Short"),
            Flaw::Invalid(what) => write!(text, "Invalid({what:?})"),
            Flaw::Broken => text.write_str("Broken"),
            Flaw::Unsupported(what) => write!(text, "Unsupported({what:?})"),
            Flaw::Missing(what) => write!(text, "Missing({what:?})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn flaw_text() {
        let flaws =
            [Flaw::Short, Flaw::Invalid("box size"), Flaw::Broken, Flaw::Unsupported("mono"), Flaw::Missing("stsd")];
        assert_eq!(
            flaws.map(|flaw| flaw.to_string()),
            ["Short", "Invalid(\"box size\")", "Broken", "Unsupported(\"mono\")", "Missing(\"stsd\")"],
            "the words an error's text and the log tell a flaw by"
        );
    }
}
