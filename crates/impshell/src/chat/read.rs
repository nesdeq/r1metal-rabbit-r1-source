use crate::openai::Account;
use crate::storage::Data;
use crate::{clock, log, sys};
use r1_funcore::base::time::Moment;
use r1_funcore::domain::chat::read;
use r1_funcore::domain::chat::recall::{self, Paper};
use r1_funcore::domain::chat::tools::{ReadTarget, Readout};
use r1_funcore::domain::cost::to_figures;
use r1_funcore::domain::device;
use r1_funcore::domain::memos;
use r1_funcore::domain::settings::Settings;
use r1_funcore::openai::ledger::SummaryKind;
use r1_funcore::storage::NEWS;
use r1_funcore::storage::paths::Listed;

pub(super) fn read_target(
    mut data: &Data,
    account: &Account,
    (target, day): (ReadTarget, Option<&str>),
    settings: &Settings,
    readout: impl FnOnce(Readout) -> String,
) -> String {
    match target {
        ReadTarget::Readout(kind) => readout(kind),
        ReadTarget::Notes => read::read_entries(&mut data, SummaryKind::Note, day),
        ReadTarget::Translations => read::read_entries(&mut data, SummaryKind::Talk, day),
        ReadTarget::News => edition(data, day),
        ReadTarget::Cost => cost(account),
        ReadTarget::About => {
            read::to_about(&device::to_lines(&device::public_facts(&sys::read_sources(&|| data.room()))))
        }
        ReadTarget::Settings => settings.to_readout(),
    }
}

// What the search reads of the notes and the translations.
pub(super) fn papers(mut data: &Data) -> Vec<Paper> {
    let read = |kind| {
        let papers = recall::read_papers(&mut data, kind);
        papers.inspect_err(|e| log!("chat: the entries cannot be searched: {e}")).unwrap_or_default()
    };
    recall::KINDS.into_iter().flat_map(read).collect()
}

fn cost(account: &Account) -> String {
    match account.ledger() {
        Ok(ledger) => {
            let now = Moment::from_seconds(clock::local());
            read::to_cost(&to_figures(&ledger, now.year, now.month))
        }
        Err(e) => read::to_unreadable(&e.to_string()),
    }
}

fn edition(data: &Data, day: Option<&str>) -> String {
    let editions =
        data.names(NEWS, Listed::Folders).inspect_err(|e| log!("chat: the editions of the news cannot be listed: {e}"));
    let day = read::edition_day(day, &editions.unwrap_or_default());
    let mut files = data;
    let rundowns = day.as_deref().map(|day| memos::read_rundowns(&mut files, day)).unwrap_or_default();
    read::to_edition(day.as_deref(), &rundowns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::domain::chat::read::NOTHING;

    #[test]
    fn unreadable_and_readout_targets() {
        let (data, radio) = (Data::unusable(), crate::wifi::radio::tests::without_networks());
        let account = Account::new(Data::unusable(), radio.online());
        let settings = Settings::default();
        let empty = |_| String::new();
        let read = |target, day, readout: &dyn Fn(Readout) -> String| {
            read_target(&data, &account, (target, day), &settings, readout)
        };
        assert!(read(ReadTarget::Notes, None, &empty).starts_with("They cannot be read: "));
        assert_eq!(read(ReadTarget::News, Some("20261016"), &empty), "No edition of 20261016.");
        assert_eq!(read(ReadTarget::News, None, &empty), NOTHING);
        assert!(read(ReadTarget::Cost, None, &empty).starts_with("They cannot be read: "), "nor the book");
        let about = read(ReadTarget::About, None, &empty);
        assert!(
            about.starts_with("- R1METAL: ") && about.contains("\n- KERNEL: LINUX ") && !about.contains("IMEI"),
            "what this computer runs, and nothing that names it: {about}"
        );
        assert_eq!(
            read(ReadTarget::Settings, None, &empty),
            settings.to_readout(),
            "the settings as the robot knows them"
        );
        let readout = |kind| format!("{kind:?}");
        assert_eq!(read(ReadTarget::Readout(Readout::Battery), None, &readout), "Battery");
        assert_eq!(read(ReadTarget::Readout(Readout::Bell), None, &readout), "Bell");
        assert!(papers(&data).is_empty(), "nothing to search on a partition that is not there");
    }
}
