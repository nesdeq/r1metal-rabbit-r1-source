//! The key to OpenAI's services: its format, its file, the verdict of its check, and the keeper thread's state.

use super::refusal::parse_refusal_bytes;
use crate::base::handoff::{Orders, Ticket};
use crate::codec::http::{FORBIDDEN, OK, UNAUTHORIZED};
use alloc::format;
use alloc::string::String;

const PREFIX: &str = "sk-";
/// The key's directory under the data partition's private directory.
pub const DIRECTORY: &str = ".r1metal/openai";
pub const FILE: &str = "key";
/// The flasher's command that makes the code the key's window scans.
pub const SCAN_COMMAND: &str = "r1 key --qr";
pub const SCANNED_KEY: &str = "a key read off a code";
pub const STORED_KEY: &str = "the kept key";
pub const REFUSED_LINE: &str = "openai: the service refuses the kept key, which is kept no more";

pub fn to_checked_line(name: &str, status: u16, verdict: Verdict) -> String {
    format!("openai: {name} was checked: status {status}, {}", verdict.to_word())
}

/// Where a stored key came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Check,
    Flash,
    Console,
}

impl Source {
    /// The log line of a key stored.
    pub fn to_log_line(self) -> String {
        let key = match self {
            Source::Check => "the service knows the key, which is",
            Source::Flash => "the key the flash gave is",
            Source::Console => "a key the console handed over is",
        };
        format!("openai: {key} kept in {DIRECTORY}/{FILE}")
    }
}

/// The `Authorization` value of a key.
pub fn to_bearer(key: &str) -> String {
    format!("Bearer {key}")
}

/// The trimmed key of a file's text; none for a file that holds none.
pub fn parse_file(text: &str) -> Option<&str> {
    Some(text.trim()).filter(|key| !key.is_empty())
}

/// The file's text of a key: trimmed, as a line of its own.
pub fn to_file_text(key: &str) -> String {
    format!("{}\n", key.trim())
}

/// The service's verdict on a checked key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Works,
    // The service knows the key and does not let it list the models, as a key without that scope: it may work for the rest.
    Known,
    // The service does not know the key: asking again comes to the same.
    Refused,
    // The service said nothing of the key this time.
    Unanswered,
}

impl Verdict {
    // Its word in the log's line of a check.
    const fn to_word(self) -> &'static str {
        match self {
            Verdict::Works => "Works",
            Verdict::Known => "Known",
            Verdict::Refused => "Refused",
            Verdict::Unanswered => "Unanswered",
        }
    }

    /// True if a scanned key is stored: the service knows it.
    pub const fn should_store(self) -> bool {
        matches!(self, Verdict::Works | Verdict::Known)
    }

    /// True if the stored key is removed: the service does not know it.
    pub const fn should_remove(self) -> bool {
        matches!(self, Verdict::Refused)
    }

    /// The window's result of a check; `is_stored` is whether the key was stored.
    pub const fn to_check_result(self, is_stored: bool) -> CheckResult {
        match self {
            Verdict::Works | Verdict::Known if is_stored => CheckResult::Stored,
            Verdict::Works | Verdict::Known => CheckResult::NotStored,
            Verdict::Refused => CheckResult::Refused,
            Verdict::Unanswered => CheckResult::Unanswered,
        }
    }
}

/// The verdict of an answer of that status and body to the check.
pub fn parse_verdict(status: u16, body: &[u8]) -> Verdict {
    match (status, parse_refusal_bytes(body)) {
        (OK, _) => Verdict::Works,
        (status, Some(refusal)) if refusal.is_of_unknown_key(Some(status)) => Verdict::Refused,
        (UNAUTHORIZED | FORBIDDEN, Some(_)) => Verdict::Known,
        _ => Verdict::Unanswered,
    }
}

fn is_unknown_in(status: u16, body: &[u8]) -> bool {
    parse_verdict(status, body) == Verdict::Refused
}

/// True if an answer of that status says the service does not know the stored key; `body` none where it could not be read, `is_stored` false for a key under check.
pub fn is_stored_unknown(is_stored: bool, status: u16, body: Option<&[u8]>) -> bool {
    is_stored && body.is_some_and(|body| is_unknown_in(status, body))
}

/// The outcome of a key's check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckResult {
    // The service knows the key, which replaced any stored before.
    Stored,
    // The service does not know the key.
    Refused,
    // The device is in no network.
    Offline,
    // The service said nothing of the key this time.
    Unanswered,
    // The service knows the key, which could not be stored.
    NotStored,
}

/// The report of the key's keeper.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Report {
    /// Whether a key is stored, once that is known anew.
    pub has_key: Option<bool>,
    /// The outcome of the last check, by its number.
    pub check: Option<(u32, CheckResult)>,
    /// The outcome of storing a key from the console, with the reason it failed.
    pub store: Option<Result<(), String>>,
    /// True if the service refused the stored key again, which is removed.
    pub is_refused: bool,
    /// True if the flash's key is stored: the flash's choice is taken whole, and its stamp may be stored.
    pub is_flashed: bool,
}

impl Report {
    /// The report at the keeper's start; `has_key` none where the partition cannot tell, which counts as no key so that the window asks for one.
    pub fn with_start(has_key: Option<bool>, is_flashed: bool) -> Report {
        Report { has_key: Some(has_key.unwrap_or(false)), is_flashed, ..Report::default() }
    }

    /// The report of a check: a key it stored is a key stored.
    pub fn from_check(number: u32, result: CheckResult) -> Report {
        Report {
            has_key: (result == CheckResult::Stored).then_some(true),
            check: Some((number, result)),
            ..Report::default()
        }
    }

    /// The report of storing a key from the console.
    pub fn from_handover(result: Result<(), String>) -> Report {
        Report { has_key: result.is_ok().then_some(true), store: Some(result), ..Report::default() }
    }

    /// The report of the stored key's new check: one the service refused is removed.
    pub fn from_recheck(is_refused: bool) -> Report {
        Report { has_key: is_refused.then_some(false), is_refused, ..Report::default() }
    }

    fn merge(self, newer: Report) -> Report {
        Report {
            has_key: newer.has_key.or(self.has_key),
            check: newer.check.or(self.check),
            store: newer.store.or(self.store),
            is_refused: newer.is_refused || self.is_refused,
            is_flashed: newer.is_flashed || self.is_flashed,
        }
    }
}

/// An order to the key's keeper, done in order.
pub enum Order {
    Begin(Option<String>),
    Check(u32, String),
    Keep(String),
    Recheck,
}

/// The state the key's keeper and the main loop share.
#[derive(Default)]
pub struct State {
    pub report: Report,
    pub orders: Orders<Order>,
    check: Ticket,
}

impl State {
    /// Queues the check of a scanned key, once for every check the window waits for; true if one was queued.
    pub fn ask_check(&mut self, request: Option<(u32, &str)>) -> bool {
        let Some((number, key)) = request.filter(|(number, _)| self.check.want(Some(*number))) else {
            return false;
        };
        self.orders.add(Order::Check(number, key.into()));
        true
    }

    /// Merges an order's report into the waiting one.
    pub fn add_report(&mut self, report: Report) {
        self.report = core::mem::take(&mut self.report).merge(report);
    }

    /// Takes the report; a check that ended may be asked for again.
    pub fn take_report(&mut self) -> Report {
        let report = core::mem::take(&mut self.report);
        report.check.iter().for_each(|(number, _)| self.check.answer(*number));
        report
    }
}

/// The trimmed key of a text, if it looks like one of OpenAI's: the prefix, then letters, digits, hyphens and underscores alone.
pub fn parse_key(text: &str) -> Option<&str> {
    let key = text.trim();
    let rest = key.strip_prefix(PREFIX)?;
    let is_valid = rest.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    (!rest.is_empty() && is_valid).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format() {
        assert_eq!(parse_key(" sk-proj-Ab3_-x9\n"), Some("sk-proj-Ab3_-x9"), "the blanks about it go");
        assert_eq!(parse_key("sk-Zx81"), Some("sk-Zx81"));
        for invalid in
            ["", "sk-", "pk-live-abc", "sk-ab cd", "sk-ab\ncd", "sk-abc+", "https://example.org/sk-a", "sk-\u{E4}"]
        {
            assert_eq!(parse_key(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn verdicts() {
        let refusal = |code: &str| {
            alloc::format!(
                "{{\"error\": {{\"message\": \"x\", \"type\": \"invalid_request_error\", \"code\": {code}}}}}"
            )
        };
        let (unknown_key, scope, no_code) =
            (refusal("\"invalid_api_key\""), refusal("\"missing_scope\""), refusal("null"));
        assert_eq!(parse_verdict(200, b"{\"data\": []}"), Verdict::Works);
        assert_eq!(parse_verdict(401, unknown_key.as_bytes()), Verdict::Refused, "a key mistyped, deleted or revoked");
        let no_permission =
            br#"{"error":"You have insufficient permissions for this operation. Missing scopes: api.model.read."}"#;
        assert_eq!(
            [
                parse_verdict(401, scope.as_bytes()),
                parse_verdict(401, no_code.as_bytes()),
                parse_verdict(403, unknown_key.as_bytes()),
                parse_verdict(403, no_permission)
            ],
            [Verdict::Known; 4],
            "a key without the scope to list the models, and a 403, as one without the permission to gets, come from a key the service knows"
        );
        assert_eq!(
            [
                parse_verdict(401, b""),
                parse_verdict(401, b"<html>401</html>"),
                parse_verdict(429, scope.as_bytes()),
                parse_verdict(500, b"")
            ],
            [Verdict::Unanswered; 4],
            "a refusal that is none of the service's, and any other status, tell nothing of the key"
        );
        assert!(crate::openai::models::check::ADDRESS.starts_with("https://api.openai.com/"));
        assert!(
            is_unknown_in(401, unknown_key.as_bytes())
                && !is_unknown_in(401, scope.as_bytes())
                && !is_unknown_in(200, b"")
        );
    }

    #[test]
    fn log_lines() {
        assert_eq!(
            [Source::Check, Source::Flash, Source::Console].map(Source::to_log_line),
            [
                "openai: the service knows the key, which is kept in .r1metal/openai/key",
                "openai: the key the flash gave is kept in .r1metal/openai/key",
                "openai: a key the console handed over is kept in .r1metal/openai/key",
            ]
        );
        assert_eq!(
            to_checked_line(STORED_KEY, 401, Verdict::Refused),
            "openai: the kept key was checked: status 401, Refused"
        );
        assert_eq!(
            [Verdict::Works, Verdict::Known, Verdict::Refused, Verdict::Unanswered].map(Verdict::to_word),
            ["Works", "Known", "Refused", "Unanswered"],
            "the words the log has had for each verdict"
        );
        let unknown = br#"{"error":{"message":"x","code":"invalid_api_key"}}"#;
        assert!(is_stored_unknown(true, 401, Some(unknown)));
        assert!(!is_stored_unknown(false, 401, Some(unknown)) && !is_stored_unknown(true, 401, None));
    }

    #[test]
    fn file_text_and_bearer() {
        assert_eq!(
            (to_file_text(" sk-a1 \n"), parse_file("sk-a1\n"), parse_file(" \n"), parse_file("")),
            ("sk-a1\n".into(), Some("sk-a1"), None, None)
        );
        assert_eq!(to_bearer("sk-a1"), "Bearer sk-a1");
    }

    #[test]
    fn verdict_actions() {
        assert_eq!(
            [Verdict::Works, Verdict::Known, Verdict::Refused, Verdict::Unanswered].map(Verdict::should_store),
            [true, true, false, false]
        );
        assert_eq!(
            [Verdict::Works, Verdict::Known, Verdict::Refused, Verdict::Unanswered].map(Verdict::should_remove),
            [false, false, true, false],
            "only a key the service does not know is taken away"
        );
        assert_eq!(
            [
                Verdict::Works.to_check_result(true),
                Verdict::Known.to_check_result(false),
                Verdict::Refused.to_check_result(false)
            ],
            [CheckResult::Stored, CheckResult::NotStored, CheckResult::Refused]
        );
        assert_eq!(Verdict::Unanswered.to_check_result(false), CheckResult::Unanswered);
    }

    #[test]
    fn reports() {
        assert_eq!(
            Report::from_handover(Ok(())),
            Report { has_key: Some(true), store: Some(Ok(())), ..Report::default() },
            "a key the console handed over and kept is a key kept"
        );
        assert_eq!(
            Report::from_handover(Err("full".into())),
            Report { store: Some(Err("full".into())), ..Report::default() },
            "one that could not be kept leaves the word on keys as it was"
        );
        assert_eq!(Report::from_recheck(true), Report { has_key: Some(false), is_refused: true, ..Report::default() });
        assert_eq!(Report::from_recheck(false), Report::default(), "a key the service did not refuse again stays");
        assert_eq!(
            [Report::from_check(3, CheckResult::Stored), Report::from_check(4, CheckResult::NotStored)],
            [
                Report { has_key: Some(true), check: Some((3, CheckResult::Stored)), ..Report::default() },
                Report { check: Some((4, CheckResult::NotStored)), ..Report::default() }
            ],
            "a key a check kept is a key kept, and one it could not keep leaves the word on keys as it was"
        );
        assert_eq!(
            [
                Report::with_start(Some(true), true),
                Report::with_start(Some(false), false),
                Report::with_start(None, false)
            ],
            [
                Report { has_key: Some(true), is_flashed: true, ..Report::default() },
                Report { has_key: Some(false), ..Report::default() },
                Report { has_key: Some(false), ..Report::default() }
            ],
            "as it begins, whether a key is kept, a partition that cannot tell counting as none, and whether the flash's was"
        );
    }

    #[test]
    fn check_asked_once() {
        use crate::base::handoff::Step;
        let mut state = State::default();
        assert!(!state.ask_check(None), "no check waited for, none queued");
        assert!(
            state.ask_check(Some((4, "sk-a"))) && !state.ask_check(Some((4, "sk-a"))),
            "the check the window waits for is queued once"
        );
        assert!(
            matches!(state.orders.next_step(), Some(Step::Serve(Order::Check(4, key))) if key == "sk-a")
                && state.orders.next_step().is_none(),
            "by its number, with its key"
        );
        state.add_report(Report::from_check(4, CheckResult::Unanswered));
        assert!(!state.ask_check(Some((4, "sk-a"))), "nor again while its report waits to be taken");
        assert_eq!(state.take_report().check, Some((4, CheckResult::Unanswered)));
        assert!(state.ask_check(Some((4, "sk-a"))), "its report taken, the same check may be asked for again");
    }

    #[test]
    fn merged_reports() {
        let earlier = Report {
            has_key: Some(false),
            check: Some((1, CheckResult::Refused)),
            store: None,
            is_refused: true,
            is_flashed: true,
        };
        let later =
            Report { has_key: Some(true), check: None, store: Some(Ok(())), is_refused: false, is_flashed: false };
        assert_eq!(
            earlier.merge(later),
            Report {
                has_key: Some(true),
                check: Some((1, CheckResult::Refused)),
                store: Some(Ok(())),
                is_refused: true,
                is_flashed: true
            },
            "a refusal and a flash's key kept that were not taken yet are kept too"
        );
    }
}
