//! What the clock's alarm may wake a sleeping device for: every exception to a full sleep stands in this list.

/// A use the clock's alarm may be set for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    // The daily news, and the model's rundowns fetched in the looks that follow.
    DailyNews,
    SignalMessages,
    // Every alarm, reminder and timer the bell holds.
    Bell,
    // The end of the attach the modem keeps after a stay.
    KeptAttach,
}

impl Reason {
    #[cfg(test)]
    const ALL: [Reason; 4] = [Reason::DailyNews, Reason::SignalMessages, Reason::Bell, Reason::KeptAttach];

    /// The use as the log and the page of sleep name it.
    pub const fn name(self) -> &'static str {
        match self {
            Reason::DailyNews => "the daily news",
            Reason::SignalMessages => "Signal's messages",
            Reason::Bell => "the bell's rings",
            Reason::KeptAttach => "the modem's kept attach",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    const PAGE: &str = include_str!("../../../../../docs/firmware/sleep.md");

    #[test]
    fn reasons_on_page() {
        assert_eq!(
            Reason::ALL.map(Reason::name),
            ["the daily news", "Signal's messages", "the bell's rings", "the modem's kept attach"],
            "a use more is an exception more to a sleep that shuts everything down"
        );
        let page = PAGE.to_ascii_lowercase();
        for reason in Reason::ALL {
            let name = reason.name().to_ascii_lowercase();
            assert!(page.contains(&format!("| {name} |")), "the page of sleep lists {name}");
        }
        // The first table under the list's heading is the list.
        let section = page.split("## the alarm's list").nth(1).unwrap_or_default();
        let table = section.split("\n\n").find(|block| block.starts_with('|')).unwrap_or_default();
        assert_eq!(
            table.lines().filter(|line| line.starts_with('|')).count(),
            Reason::ALL.len() + 2,
            "and no use that is not in the list: {table}"
        );
    }
}
