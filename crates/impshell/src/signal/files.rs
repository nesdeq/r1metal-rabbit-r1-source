use crate::log;
use crate::openai::Account;
use crate::storage::settings::Uses;
use crate::storage::{Data, logs};
use r1_funcore::domain::uses::SIGNAL_USE;
use r1_funcore::openai::ledger;
use r1_funcore::signal::attachment::{ATTACHMENTS, unheld};
use r1_funcore::signal::device::{DIRECTORY, Device, FOLDERS, File, LinkState, to_removed, to_unforgotten_line};
use r1_funcore::storage::paths::{Listed, split_folder};
use std::collections::{BTreeMap, BTreeSet};
use std::io;

pub(super) struct Traces {
    pub(super) account: Account,
    pub(super) uses: Uses,
}

pub(super) fn read(data: &Data) -> io::Result<LinkState> {
    let (mut files, mut has_files) = (BTreeMap::new(), false);
    for directory in FOLDERS.into_iter().chain([DIRECTORY]) {
        for name in data.names(directory, Listed::AllFiles)? {
            has_files = true;
            if let Some(file) = File::from_path(directory, &name)
                && let Some(bytes) = data.read(directory, &name)?
            {
                files.insert(file, bytes);
            }
        }
    }
    Ok(Device::to_link_state(&files, has_files))
}

pub(super) fn write(data: &Data, device: &mut Device) -> io::Result<()> {
    let mut outcome = Ok(());
    for file in device.take_dirty() {
        let (directory, name) = file.to_path();
        let result = match device.file_bytes(&file) {
            Some(content) => data.put(directory, &name, &content),
            None => data.remove(directory, &name),
        };
        if let Err(e) = result {
            device.mark_dirty(file);
            outcome = outcome.and(Err(e));
        }
    }
    outcome
}

// Takes away the files of the attachments' directory that `held` names none of.
pub(super) fn sweep(data: &Data, held: &BTreeSet<String>) {
    match data.names(ATTACHMENTS, Listed::AllFiles) {
        Ok(listed) => {
            for name in unheld(listed, held) {
                if let Err(e) = data.remove(ATTACHMENTS, &name) {
                    log!("signal: a file of no attachment is not taken away: {e}");
                }
            }
        }
        Err(e) => log!("signal: the attachments' files are not listed: {e}"),
    }
}

fn empty_directory(data: &Data, directory: &str, keep: Option<&str>) -> Vec<io::Error> {
    match data.names(directory, Listed::AllFiles) {
        Ok(names) => {
            to_removed(&names, keep).into_iter().filter_map(|name| data.remove(directory, name).err()).collect()
        }
        Err(e) => vec![e],
    }
}

pub(super) fn forget(data: &Data, traces: &Traces) {
    let (account_directory, account_file) = File::Account.to_path();
    let mut errors: Vec<io::Error> = data.put(account_directory, &account_file, &[]).err().into_iter().collect();
    for directory in FOLDERS {
        errors.extend(empty_directory(data, directory, None));
        if let Some((parent, folder)) = split_folder(directory) {
            errors.extend(data.discard(parent, folder).err());
        }
    }
    errors.extend(empty_directory(data, account_directory, Some(&account_file)));
    errors.extend(traces.uses.forget(SIGNAL_USE).err());
    errors.extend(traces.account.forget(&ledger::SIGNAL).err());
    errors.extend(logs::scrub().err());
    if let Err(e) = data.trim() {
        log!("signal: the free blocks are not handed back to the medium: {e}");
    }
    if let Some(first) = errors.first() {
        return log!("{}", to_unforgotten_line(errors.len(), first));
    }
    if let Err(e) = data.remove(account_directory, &account_file) {
        log!("signal: the account's file is not taken away: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::scratch;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::domain::uses;
    use r1_funcore::openai::key;
    use r1_funcore::openai::ledger::{Entry, Kind, Ledger};
    use r1_funcore::signal::device::{CONVERSATIONS, SENDER_KEYS, SESSIONS};
    use std::fs;
    use std::path::Path;

    fn data_with_files(root: &Path) -> Data {
        let data = Data::with_root(root);
        let files = [
            (DIRECTORY, "account"),
            (DIRECTORY, "keys-aci"),
            (DIRECTORY, "seen.new"),
            (SESSIONS, "0a0b.1"),
            (SESSIONS, "0a0b.1.new"),
            (SENDER_KEYS, "0a0b.1.0c0d"),
            (CONVERSATIONS, "0a0b"),
            (ATTACHMENTS, "5-0.jpg"),
        ];
        for (directory, name) in files {
            data.put(directory, name, b"kept").unwrap();
        }
        data.append(uses::DIRECTORY, uses::FILE, b"1767225600 signal\n1767225700 photo\n").unwrap();
        let mut ledger = Ledger::default();
        ledger.add(2026, 10, Entry::one(Kind::Auto), 2_000);
        data.put(key::DIRECTORY, ledger::FILE, ledger.to_text().as_bytes()).unwrap();
        data
    }

    fn traces(data: &Data, uses: Uses) -> Traces {
        Traces { account: Account::new(data.clone(), without_networks().online()), uses }
    }

    fn file_names(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(root.join(DIRECTORY))
            .map(|entries| entries.flatten().map(|entry| entry.file_name().into_string().unwrap()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn unlink_removes_every_trace() {
        let root = scratch("unlink");
        let data = data_with_files(&root);
        let keeper = crate::storage::settings::Keeper::start(data.clone(), None).unwrap();
        forget(&data, &traces(&data, keeper.uses()));
        assert_eq!(file_names(&root), Vec::<String>::new(), "no file, the half written too, and no folder of ids");
        assert!(!root.join(ATTACHMENTS).exists(), "nor the plaintexts of the attachments");
        assert!(matches!(read(&data).unwrap(), LinkState::Unlinked));
        assert_eq!(fs::read_to_string(root.join(uses::DIRECTORY).join(uses::FILE)).unwrap(), "1767225700 photo\n");
        let ledger = Ledger::parse(&fs::read_to_string(root.join(key::DIRECTORY).join(ledger::FILE)).unwrap());
        let mut expected = Ledger::default();
        expected.add(2026, 10, Entry::one(Kind::Auto), 2_000);
        expected.forget(&ledger::SIGNAL);
        assert_eq!(ledger, expected, "the ledger's month keeps its cost, told by no kind");
        forget(&data, &traces(&data, keeper.uses()));
        assert!(matches!(read(&data).unwrap(), LinkState::Unlinked), "an unlink of nothing leaves nothing");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_unlink_finished_at_start() {
        let root = scratch("unlink-cut");
        let data = data_with_files(&root);
        forget(&data, &traces(&data, Uses::closed()));
        assert_eq!(file_names(&root), ["account"], "the uses could not be forgotten: the rest went all the same");
        assert_eq!(fs::read(root.join(DIRECTORY).join("account")).unwrap(), b"", "an account file without an account");
        let ledger = Ledger::parse(&fs::read_to_string(root.join(key::DIRECTORY).join(ledger::FILE)).unwrap());
        assert!(!ledger.clone().forget(&ledger::SIGNAL), "the ledger was forgotten after the failure");
        assert!(matches!(read(&data).unwrap(), LinkState::Leftovers), "a start finds what is left");
        let keeper = crate::storage::settings::Keeper::start(data.clone(), None).unwrap();
        forget(&data, &traces(&data, keeper.uses()));
        assert_eq!(
            (file_names(&root), matches!(read(&data).unwrap(), LinkState::Unlinked)),
            (Vec::<String>::new(), true)
        );
        assert_eq!(fs::read_to_string(root.join(uses::DIRECTORY).join(uses::FILE)).unwrap(), "1767225700 photo\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sweep_removes_unheld_files() {
        let root = scratch("sweep");
        let data = Data::with_root(&root);
        assert!(data.names(ATTACHMENTS, Listed::AllFiles).unwrap().is_empty());
        sweep(&data, &BTreeSet::new());
        for name in ["held.jpg", "claimed.m4a", "stray.jpg", "held.jpg.new", "stray.m4a.new"] {
            data.put(ATTACHMENTS, name, b"x").unwrap();
        }
        let held = BTreeSet::from([format!("{ATTACHMENTS}/held.jpg"), format!("{ATTACHMENTS}/claimed.m4a")]);
        sweep(&data, &held);
        assert_eq!(
            data.names(ATTACHMENTS, Listed::AllFiles).unwrap(),
            ["claimed.m4a", "held.jpg", "held.jpg.new"],
            "a message's file, a transfer's, and the half of one that takes a held file's place stay"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn files_without_account_are_leftovers() {
        let root = scratch("unlink-left");
        let data = Data::with_root(&root);
        assert!(matches!(read(&data).unwrap(), LinkState::Unlinked), "a partition that holds nothing of Signal's");
        data.put(SESSIONS, "0a0b.1.new", b"cut").unwrap();
        assert!(matches!(read(&data).unwrap(), LinkState::Leftovers), "a file half written without an account");
        let data = data_with_files(&root);
        assert!(matches!(read(&data).unwrap(), LinkState::Leftovers), "an account file that holds no account");
        data.put(DIRECTORY, &File::Account.to_path().1, b"").unwrap();
        assert!(
            matches!(read(&data).unwrap(), LinkState::Leftovers),
            "and one that holds nothing, which an unlink leaves"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
