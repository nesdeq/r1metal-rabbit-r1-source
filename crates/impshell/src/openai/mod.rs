pub mod aloud;
pub mod chat;
pub mod image;
pub mod keys;
pub mod live;
mod request;
mod session;
pub mod transcript;
pub mod write;

pub use session::Session;

use crate::net::pool::Pool;
use crate::storage::Data;
use crate::sync::wait_for;
use crate::sys::lock;
use crate::wifi::radio::{Hold, NetworkDown, Online};
use crate::{clock, crypto, log};
use r1_funcore::base::time::Moment;
use r1_funcore::codec::form::{self, BOUNDARY_BYTES};
use r1_funcore::openai::account::{State, Terms};
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::key;
use r1_funcore::openai::ledger::{self, Ledger};
use r1_funcore::openai::models::{Task, Thinking};
use r1_funcore::openai::refusal::Refusal;
use request::Key;
use std::io;
use std::sync::{Arc, Condvar, Mutex};

fn boundary() -> io::Result<String> {
    let random = crypto::random::<BOUNDARY_BYTES>();
    Ok(form::to_boundary(&random.ok_or_else(|| io::Error::other("no random bytes to part the form with"))?))
}

struct Shared {
    data: Data,
    online: Online,
    pool: Pool,
    state: Mutex<State>,
    ledger_return: Condvar,
}

/// The handle of the key, the ledger and the connections, for every thread that asks a service.
#[derive(Clone)]
pub struct Account(Arc<Shared>);

impl Account {
    /// The account whose key and ledger are on `data`, which reaches the services through `online`.
    pub fn new(data: Data, online: Online) -> Account {
        let state = Mutex::new(State::default());
        Account(Arc::new(Shared { data, online, pool: Pool::default(), state, ledger_return: Condvar::new() }))
    }

    fn read_key(&self) -> io::Result<Option<String>> {
        let text = self.0.data.text(key::DIRECTORY, key::FILE)?;
        Ok(text.as_deref().and_then(key::parse_file).map(String::from))
    }

    fn key(&self) -> Result<Key, Fault> {
        let key = self.read_key()?.ok_or(Fault::NoKey)?;
        Ok(Key { bearer: key::to_bearer(&key), account: self.clone() })
    }

    // The key for work paid for anew, which waits while the month's cost is at the owner's limit.
    fn paying(&self) -> Result<Key, Fault> {
        let key = self.key()?;
        self.check_limit()?;
        Ok(key)
    }

    fn check_limit(&self) -> Result<(), Fault> {
        let terms = lock(&self.0.state).terms;
        if !terms.is_limited() {
            return Ok(());
        }
        let now = Moment::from_seconds(clock::local());
        self.with_ledger(|ledger| Ok(terms.check_limit(ledger, now.year, now.month)))
            .map_err(|e| Fault::Temporary(format!("the book of what the services were paid: {e}")))?
    }

    /// Takes the owner's terms: each use's model and the month's limit.
    pub fn set_terms(&self, terms: Terms) {
        lock(&self.0.state).terms = terms;
    }

    fn thinking(&self, task: Task) -> Thinking {
        lock(&self.0.state).terms.thinking(task)
    }

    fn has_key(&self) -> io::Result<bool> {
        Ok(self.read_key()?.is_some())
    }

    /// Fails where no key is stored, or where the partition cannot tell.
    pub fn check_key(&self) -> Result<(), Fault> {
        match self.has_key() {
            Ok(true) => Ok(()),
            Ok(false) => Err(Fault::NoKey),
            Err(e) => Err(Fault::Temporary(format!("the key: {e}"))),
        }
    }

    /// Holds the network for a service, once a key is known to be stored.
    pub fn hold(&self) -> Result<Hold, Fault> {
        self.check_key()?;
        self.0.online.hold()
    }

    /// `hold` as `Online::hold_quietly` does, for a holder that had the time asked in an earlier stay.
    pub fn hold_quietly(&self) -> Result<Hold, Fault> {
        self.check_key()?;
        self.0.online.hold_quietly()
    }

    /// `hold` into `slot` as `Online::keep` does; returns the stay's end.
    pub fn keep_hold(&self, slot: &mut Option<Hold>) -> Result<NetworkDown, Fault> {
        self.check_key()?;
        self.0.online.keep(slot)
    }

    fn write_key(&self, key: &str) -> io::Result<()> {
        self.0.data.put(key::DIRECTORY, key::FILE, key::to_file_text(key).as_bytes())
    }

    fn remove_key(&self) -> io::Result<()> {
        self.0.data.remove(key::DIRECTORY, key::FILE)
    }

    fn note_refusal(&self, status: Option<u16>, refusal: &Refusal) {
        lock(&self.0.state).note_refused(refusal.is_of_unknown_key(status));
    }

    /// True once after a service refused the stored key.
    pub fn take_refused(&self) -> bool {
        lock(&self.0.state).take_refused()
    }

    fn read_ledger(&self) -> io::Result<Ledger> {
        let text = self.0.data.text(key::DIRECTORY, ledger::FILE)?;
        Ok(text.map_or_else(Ledger::default, |text| Ledger::parse(&text)))
    }

    fn with_ledger<T>(&self, change: impl FnOnce(&mut Ledger) -> io::Result<T>) -> io::Result<T> {
        let loan = wait_for(&self.0.ledger_return, &self.0.state, |state| state.ledger.take());
        let (ledger, result) = match loan.map_or_else(|| self.read_ledger(), Ok) {
            Ok(mut ledger) => {
                let result = change(&mut ledger);
                (Some(ledger), result)
            }
            Err(e) => (None, Err(e)),
        };
        lock(&self.0.state).ledger.give(ledger);
        self.0.ledger_return.notify_all();
        result
    }

    fn change_ledger(&self, change: impl FnOnce(&mut Ledger) -> bool) -> io::Result<()> {
        self.with_ledger(|ledger| {
            if !change(ledger) {
                return Ok(());
            }
            self.0.data.put(key::DIRECTORY, ledger::FILE, ledger.to_text().as_bytes())
        })
    }

    // Books what `add` enters in the month the clock tells; the ledger is written where `add` says it entered anything.
    fn add(&self, add: impl FnOnce(&mut Ledger, u32, u8) -> bool) {
        let now = Moment::from_seconds(clock::local());
        if let Err(e) = self.change_ledger(|ledger| add(ledger, now.year, now.month)) {
            log!("openai: the book of what the services were paid: {e}");
        }
    }

    pub fn ledger(&self) -> io::Result<Ledger> {
        self.with_ledger(|ledger| Ok(ledger.clone()))
    }

    /// Removes `kinds` from the ledger, which is stored where it held any of them.
    pub fn forget(&self, kinds: &[ledger::Kind]) -> io::Result<()> {
        self.change_ledger(|ledger| ledger.forget(kinds))
    }

    pub fn drop_ended_connections(&self) {
        self.0.pool.drop_ended();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use r1_funcore::openai::ledger::{Entry, Kind};

    pub(crate) fn unusable() -> Account {
        Account::new(Data::unusable(), crate::wifi::radio::tests::without_networks().online())
    }

    #[test]
    fn no_partition_no_key() {
        let radio = crate::wifi::radio::tests::without_networks();
        let account = Account::new(Data::unusable(), radio.online());
        assert!(
            account.read_key().is_err() && account.ledger().is_err(),
            "without a partition there is no key and no book"
        );
        assert!(matches!(account.key(), Err(Fault::Temporary(_))) && account.has_key().is_err(), "nor one to ask with");
        assert!(account.write_key("sk-proj-Fake0Key_-9").is_err(), "and none is kept");
        assert!(account.remove_key().is_err(), "nor taken away");
        let refusal = |code: &str| Refusal { code: code.into(), message: "x".into() };
        account.note_refusal(Some(401), &refusal("missing_scope"));
        assert!(!account.take_refused(), "only a service that does not know the key refuses it");
        account.clone().note_refusal(Some(401), &refusal("invalid_api_key"));
        assert!(account.take_refused() && !account.take_refused(), "told once, to every clone of the account");
        assert!(matches!(account.check_key(), Err(Fault::Temporary(text)) if text.starts_with("the key: ")));
        assert!(
            matches!(account.hold(), Err(Fault::Temporary(_)))
                && matches!(account.hold_quietly(), Err(Fault::Temporary(_)))
                && matches!(account.keep_hold(&mut None), Err(Fault::Temporary(_)))
                && radio.is_off(),
            "where no key can be read the network is not held for nothing"
        );
    }

    #[test]
    fn limit_holds_back_paid_work() {
        let root = crate::storage::scratch("limit");
        let account = Account::new(Data::with_root(&root), crate::wifi::radio::tests::without_networks().online());
        account.write_key("sk-proj-Fake0Key_-9").unwrap();
        let now = Moment::from_seconds(clock::local());
        let mut ledger = Ledger::default();
        ledger.add(now.year, now.month, Entry::one(Kind::Rundown), 2_000_000);
        account.0.data.put(key::DIRECTORY, ledger::FILE, ledger.to_text().as_bytes()).unwrap();
        assert!(account.paying().is_ok(), "no limit, no hold");
        account.set_terms(Terms { limit: Some(2), ..Terms::default() });
        assert!(matches!(account.paying(), Err(Fault::Limit)), "two dollars spent of two");
        assert!(account.key().is_ok(), "what was asked for already is asked after all the same");
        account.set_terms(Terms { limit: Some(5), ..Terms::default() });
        assert!(account.paying().is_ok(), "a higher limit lets work through");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn forget_keeps_month_cost() {
        let root = crate::storage::scratch("forget");
        let account = Account::new(Data::with_root(&root), crate::wifi::radio::tests::without_networks().online());
        let mut ledger = Ledger::default();
        ledger.add(2026, 10, Entry::one(Kind::Auto), 2_000);
        ledger.add(2026, 10, Entry::one(Kind::Rundown), 15_583);
        account.0.data.put(key::DIRECTORY, ledger::FILE, ledger.to_text().as_bytes()).unwrap();
        account.forget(&ledger::SIGNAL).unwrap();
        ledger.forget(&ledger::SIGNAL);
        let text = std::fs::read_to_string(root.join(key::DIRECTORY).join(ledger::FILE)).unwrap();
        assert_eq!((Ledger::parse(&text), account.ledger().unwrap()), (ledger.clone(), ledger), "kept and held");
        account.forget(&ledger::SIGNAL).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(unusable().forget(&ledger::SIGNAL).is_err(), "no book without a partition");
    }
}
