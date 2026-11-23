use crate::sync::Fetcher;
use crate::{clock, log};
use r1_funcore::base::time::Moment;
use r1_funcore::domain::cost::{Figure, to_figures};
use r1_funcore::openai::ledger::Ledger;
use std::io;

fn read_figures(read_ledger: &impl Fn() -> io::Result<Ledger>) -> Vec<Figure> {
    match read_ledger() {
        Ok(ledger) => {
            let now = Moment::from_seconds(clock::local());
            to_figures(&ledger, now.year, now.month)
        }
        Err(e) => {
            log!("cost: the book of what the services were paid: {e}");
            vec![Figure::unknown()]
        }
    }
}

/// The main loop's handle of the thread that reads the cost lines at each numbered request.
pub type Cost = Fetcher<u32, (u32, Vec<Figure>)>;

impl Cost {
    /// Starts the thread, which reads the ledger through `read_ledger`.
    pub fn new(read_ledger: impl Fn() -> io::Result<Ledger> + Send + 'static) -> io::Result<Cost> {
        Fetcher::start("cost", move |number| (number, read_figures(&read_ledger)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_ledger_gives_unknown() {
        let account = crate::openai::tests::unusable();
        assert_eq!(read_figures(&|| account.ledger()), vec![Figure::unknown()]);
    }
}
