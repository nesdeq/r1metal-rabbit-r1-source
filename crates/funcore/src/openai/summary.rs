//! The language model's background work: its request, how it stands at the service, the polls that ask after it, and the jobs a stay left owed.

use super::chat::request::{Location, build_tools};
use super::ledger::{Entry, Kind, Ledger, SummaryKind};
use super::models::writer::{ADDRESS, VERBOSITY};
use super::models::{Task, Thinking};
use super::output::{COMPLETED, SEARCH_MICROS, output_text, search_count};
use super::refusal::{is_temporary, parse_document};
use super::request::{parse_target, with_modules};
use super::tokens::Usage;
use crate::base::json::escape;
use crate::base::json::items;
use crate::base::json::text_at;
use crate::base::time::NANOS_PER_SECOND;
use crate::codec::http::Target;
use crate::openai::fault::Fault;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

// The owner's choice: work that has not come in five minutes is waited for no more.
pub(crate) const PATIENCE_SECONDS: u64 = 300;
const POLL_SECONDS: u64 = 2;
const HOLD_SECONDS: u64 = 20;
pub(crate) const SLOW_POLL_SECONDS: u64 = 30;
const RECOLLECTION: &str = "a day recollected";
const AUTO_REPLY: &str = "a message of auto mode written";
const WEB_PAGE: &str = "a step of the browser's taken";
const BELL_REQUEST: &str = "a request to the bell read";
const RUNDOWNS_REQUEST: &str = "a request for the rundowns read";
/// The use whose model writes a day's rundown, and what the ledger books one as.
pub const RUNDOWN: (Task, Entry) = (Task::News, Entry::one(Kind::Rundown));
/// The error text of work a stay ended before it came.
pub const UNFINISHED: &str = "the stay ended before the model's work came, which is owed to the next asking";
pub const RESUMED_LINE: &str = "openai: work a stay ended before it came is asked after, not asked for again";

/// What a piece of background work waited for is of: by it the work is named in the log, takes its use's model, and is booked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Work {
    Summary(SummaryKind),
    Recollection,
    AutoReply,
    Page,
    BellCalls,
    RundownLines,
}

impl Work {
    /// Its name in the log, a summary's by the count of the characters of its `input`.
    pub fn to_name(self, input: &str) -> String {
        match self {
            Work::Summary(_) => format!("{} characters summarized", input.chars().count()),
            Work::Recollection => RECOLLECTION.into(),
            Work::AutoReply => AUTO_REPLY.into(),
            Work::Page => WEB_PAGE.into(),
            Work::BellCalls => BELL_REQUEST.into(),
            Work::RundownLines => RUNDOWNS_REQUEST.into(),
        }
    }

    /// The use whose model and effort write it.
    pub const fn task(self) -> Task {
        match self {
            Work::Summary(_) => Task::Summary,
            Work::Recollection => Task::Recollect,
            Work::AutoReply => Task::Auto,
            Work::Page => Task::Web,
            Work::BellCalls => Task::Bell,
            Work::RundownLines => Task::Rundowns,
        }
    }

    /// What the ledger books it as: one of its kind, or, for a recollection and a request for the rundowns, work that counts none.
    pub const fn entry(self) -> Entry {
        match self {
            Work::Summary(kind) => Entry::summary(kind),
            Work::Recollection => Entry::work(Kind::Chat),
            Work::AutoReply => Entry::one(Kind::Auto),
            Work::Page => Entry::one(Kind::Web),
            Work::BellCalls => Entry::one(Kind::Bell),
            Work::RundownLines => Entry::work(Kind::Rundown),
        }
    }
}

/// The log line of a rundown asked for from a report.
pub fn to_asked_line(topic: &str, report: &str, ms: u128, status: &WorkStatus) -> String {
    format!(
        "openai: the day's {topic} asked for from {} characters in {ms} ms, {}",
        report.chars().count(),
        status.to_log_line()
    )
}

/// A request's body: the model and its effort, the verbosity, the `task`, and the `input`; stored by the service only while it works.
pub fn request(thinking: Thinking, task: &str, input: &str) -> String {
    body(thinking, &with_modules(task), input, "")
}

/// A request's body as `request` writes it, the model free to search the web from the device's `location`.
pub fn search_request(thinking: Thinking, task: &str, input: &str, location: Option<&Location>) -> String {
    let tools = format!(",\"tools\":{}", build_tools(location, true, ""));
    body(thinking, &with_modules(task), input, &tools)
}

// A background request's body under those instructions, with `tools` as the member that follows its input, or none.
pub(crate) fn body(thinking: Thinking, instructions: &str, input: &str, tools: &str) -> String {
    format!(
        "{{\"model\":\"{}\",\"reasoning\":{{\"effort\":\"{}\"}},\"text\":{{\"verbosity\":\"{VERBOSITY}\"}},\"instructions\":\"{}\",\"input\":\"{}\"{tools},\"background\":true,\"store\":false}}",
        thinking.model().to_word(),
        thinking.effort().to_word(),
        escape(instructions),
        escape(input)
    )
}

pub(crate) fn poll_address(id: &str) -> Option<String> {
    let is_plain = !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    is_plain.then(|| format!("{ADDRESS}/{id}"))
}

/// What the work wrote, as Markdown, with its usage.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Summary {
    pub text: String,
    pub usage: Usage,
}

impl Summary {
    /// The log line of `work` written in `ms` milliseconds, after `polls` polls.
    pub fn to_written_line(&self, work: &str, ms: u128, polls: u32) -> String {
        format!(
            "openai: {work} in {} characters in {ms} ms, asked after {polls} times, {}",
            self.text.chars().count(),
            self.usage.to_log_line()
        )
    }

    /// The log line of a day's rundown of that name, `seconds` after it was asked for.
    pub fn to_rundown_line(&self, topic: &str, seconds: u64) -> String {
        format!(
            "openai: the day's {topic} written in {} characters, {seconds} s after it was asked for, {}",
            self.text.chars().count(),
            self.usage.to_log_line()
        )
    }
}

/// How the service says work it has not ended stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Standing {
    Queued,
    InProgress,
}

impl Standing {
    fn parse(state: &str) -> Option<Standing> {
        [Standing::Queued, Standing::InProgress].into_iter().find(|standing| standing.to_word() == state)
    }

    pub(crate) const fn to_word(self) -> &'static str {
        match self {
            Standing::Queued => "queued",
            Standing::InProgress => "in_progress",
        }
    }
}

/// The state of the work at the service.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorkStatus {
    // Polled by this id.
    Pending(String, Standing),
    Done(Summary),
}

impl WorkStatus {
    const fn to_log_line(&self) -> &'static str {
        match self {
            WorkStatus::Done(_) => "the answer was there at once",
            WorkStatus::Pending(..) => "the model works on it",
        }
    }
}

fn is_awaited(start_ns: u64, now_ns: u64) -> bool {
    now_ns.saturating_sub(start_ns) < PATIENCE_SECONDS * NANOS_PER_SECOND
}

/// A background job a stay ended before it came: the digest of its request, its id, and its start on a clock that runs through a suspend.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Job {
    pub digest: String,
    pub id: String,
    pub start_ns: u64,
}

/// The account's unfinished jobs while they are waited for: asked for again, they would be paid again.
#[derive(Default)]
pub struct Jobs(Vec<Job>);

impl Jobs {
    /// Adds a job in place of any for the same request; those waited for no more at `now_ns` go.
    pub fn add(&mut self, job: Job, now_ns: u64) {
        self.0.retain(|other| other.digest != job.digest && is_awaited(other.start_ns, now_ns));
        self.0.push(job);
    }

    /// Takes the job of a request, where it is waited for still at `now_ns`.
    pub fn take(&mut self, digest: &str, now_ns: u64) -> Option<Job> {
        let at = self.0.iter().position(|job| job.digest == digest)?;
        Some(self.0.swap_remove(at)).filter(|job| is_awaited(job.start_ns, now_ns))
    }
}

/// The state of the work by an answer of that status; or why there is no summary and none is to come.
pub fn parse_status(status: u16, body: &[u8]) -> Result<WorkStatus, Fault> {
    let no_text = || Fault::Temporary(format!("status {status}, nothing written in the answer"));
    let document = parse_document(body)
        .map_err(|refusal| refusal.map_or_else(no_text, |refusal| Fault::from_refusal(Some(status), &refusal)))?;
    let state = text_at(document, &["status"]).ok_or_else(no_text)?;
    if let Some(standing) = Standing::parse(state) {
        let id = text_at(document, &["id"]).filter(|id| poll_address(id).is_some());
        return id.map(|id| WorkStatus::Pending(id.into(), standing)).ok_or_else(no_text);
    }
    if state != COMPLETED {
        let reason = text_at(document, &["incomplete_details", "reason"]);
        let end = reason.map_or_else(|| state.into(), |reason| format!("{state}, {reason}"));
        return Err(Fault::Refused(format!("the model's work ended as {end}")));
    }
    let text = output_text(items(document, &["output"]));
    if text.is_empty() {
        return Err(no_text());
    }
    Ok(WorkStatus::Done(Summary { text, usage: Usage::parse(document) }))
}

/// The outcome of a poll of the work.
#[derive(PartialEq, Eq, Debug)]
pub enum Poll {
    Done(Summary),
    Pending(Standing),
    // It never will be done, or the service has none of that id: asking again would come to the same.
    Failed(Fault),
    // The poll did not get through, or the service was in no state to answer.
    Retry(Fault),
}

impl Poll {
    /// The poll that could not be asked for that fault: without a key it fails, else it is made again later.
    pub fn from_fault(fault: &Fault) -> Poll {
        match fault {
            Fault::NoKey => Poll::Failed(Fault::NoKey),
            fault => Poll::Retry(Fault::Temporary(fault.to_string())),
        }
    }

    /// What the poll's answer, of that `body`, costs: work found done, or found failed, which is paid for though no summary books it; none for a look at work still to come.
    pub fn cost(&self, body: &[u8]) -> Option<Cost> {
        match self {
            Poll::Done(summary) => Some(Cost::done(summary, body)),
            Poll::Failed(_) => Some(Cost::failed(body)),
            Poll::Pending(_) | Poll::Retry(_) => None,
        }
    }
}

/// What an answer of background work cost.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cost {
    // Work that is done: its usage and its web searches, in millionths of a dollar.
    Done(u64),
    // Work that came to nothing: its usage, and its web searches in millionths of a dollar, as far as the service counted them.
    Failed(Usage, u64),
}

impl Cost {
    fn done(summary: &Summary, body: &[u8]) -> Cost {
        Cost::Done(summary.usage.micros().saturating_add(search_micros(body)))
    }

    fn failed(body: &[u8]) -> Cost {
        let (usage, search_micros) = parse_cost(body);
        Cost::Failed(usage, search_micros)
    }

    /// Adds it to the ledger in that month as `entry` tells; true if anything was added, which failed work that counted nothing is not.
    pub fn book(self, ledger: &mut Ledger, year: u32, month: u8, entry: Entry) -> bool {
        match self {
            Cost::Done(micros) => {
                ledger.add(year, month, entry, micros);
                true
            }
            Cost::Failed(usage, search_micros) => ledger.add_failed_work(year, month, entry, (usage, search_micros)),
        }
    }
}

/// What the answer to a request for work, of that `body`, costs by what it was read as; none for work still to come.
pub fn status_cost(status: &Result<WorkStatus, Fault>, body: &[u8]) -> Option<Cost> {
    match status {
        Ok(WorkStatus::Done(summary)) => Some(Cost::done(summary, body)),
        Ok(WorkStatus::Pending(..)) => None,
        Err(_) => Some(Cost::failed(body)),
    }
}

/// The watch over background work: its start and last poll on a clock that runs through a suspend, and the poll count.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Watch {
    start_ns: u64,
    poll_ns: u64,
    polls: u32,
}

impl Watch {
    /// The watch over work asked for at `start_ns`.
    pub const fn new(start_ns: u64) -> Watch {
        Watch { start_ns, poll_ns: start_ns, polls: 0 }
    }

    /// When the work is polled next, and whether the network is held until then: `POLL_SECONDS` after the last poll where that falls within `HOLD_SECONDS` of the request, else `SLOW_POLL_SECONDS` after it.
    pub const fn next(&self) -> (u64, bool) {
        let fast_ns = self.poll_ns + POLL_SECONDS * NANOS_PER_SECOND;
        if fast_ns <= self.start_ns + HOLD_SECONDS * NANOS_PER_SECOND {
            (fast_ns, true)
        } else {
            (self.poll_ns + SLOW_POLL_SECONDS * NANOS_PER_SECOND, false)
        }
    }

    /// Notes a poll at `now_ns` and returns the watch's end: the summary and the poll count, or the fault; none where it is polled again, the work waited for still then.
    pub fn note_poll(&mut self, poll: Poll, now_ns: u64) -> Option<Result<(Summary, u32), Fault>> {
        (self.polls, self.poll_ns) = (self.polls.saturating_add(1), now_ns);
        match poll {
            Poll::Done(summary) => Some(Ok((summary, self.polls))),
            Poll::Failed(fault) => Some(Err(fault)),
            Poll::Pending(_) | Poll::Retry(_) if is_awaited(self.start_ns, self.next().0) => None,
            Poll::Pending(_) | Poll::Retry(_) => {
                Some(Err(Fault::Temporary(format!("the model's work did not come in {PATIENCE_SECONDS} s"))))
            }
        }
    }
}

/// The outcome of an answer of that status to a poll.
pub fn parse_poll(status: u16, body: &[u8]) -> Poll {
    if is_temporary(status) {
        return Poll::Retry(Fault::Temporary(format!("status {status}")));
    }
    match parse_status(status, body) {
        Ok(WorkStatus::Done(summary)) => Poll::Done(summary),
        Ok(WorkStatus::Pending(_, standing)) => Poll::Pending(standing),
        Err(fault) => Poll::Failed(fault),
    }
}

pub fn poll_target(id: &str) -> Result<Target, Fault> {
    let address =
        poll_address(id).ok_or_else(|| Fault::Temporary("work of the model without a name to ask by".into()))?;
    parse_target(&address)
}

/// The target a POST cancels the work of that id at; its answer reads as a poll's.
pub fn cancel_target(id: &str) -> Result<Target, Fault> {
    let mut target = poll_target(id)?;
    target.path += "/cancel";
    Ok(target)
}

// The usage of an answer, with or without a summary; nothing for an answer that counts none.
fn parse_usage(body: &[u8]) -> Usage {
    core::str::from_utf8(body).map_or_else(|_| Usage::default(), Usage::parse)
}

// What an answer without a summary cost: its usage, and its web searches in millionths of a dollar.
fn parse_cost(body: &[u8]) -> (Usage, u64) {
    (parse_usage(body), search_micros(body))
}

// What the web searches of an answer cost in millionths of a dollar, which its usage does not count; nothing for work that searched nothing.
fn search_micros(body: &[u8]) -> u64 {
    let searches = core::str::from_utf8(body).map_or(0, |document| search_count(items(document, &["output"])));
    searches.saturating_mul(SEARCH_MICROS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::json::unescape;
    use crate::openai::models::{Effort, Model};

    #[test]
    fn request_body() {
        let task = "Sum it up.";
        let body = request(Thinking::STANDARD, task, "He said \"yes\".\nThen: no\\maybe.");
        assert!(body.starts_with(
            "{\"model\":\"gpt-6.1-terra\",\"reasoning\":{\"effort\":\"high\"},\"text\":{\"verbosity\":\"low\"},\"instructions\":\"Sum it up."
        ));
        assert!(
            request(Thinking::new(Model::Sol, Effort::None), task, "x")
                .starts_with("{\"model\":\"gpt-6.1-sol\",\"reasoning\":{\"effort\":\"low\"}"),
            "the use's model, at an effort it takes"
        );
        assert!(
            body.ends_with(
                ",\"input\":\"He said \\\"yes\\\".\\nThen: no\\\\maybe.\",\"background\":true,\"store\":false}"
            ),
            "{body}"
        );
        assert_eq!(text_at(&body, &["input"]).map(unescape).as_deref(), Some("He said \"yes\".\nThen: no\\maybe."));
        assert_eq!(
            text_at(&body, &["instructions"]).map(unescape),
            Some(with_modules(task)),
            "the task, with the reasoning modules"
        );
        assert!(!body.contains('\n'), "nothing in the body breaks out of its line");
        let searching = search_request(Thinking::new(Model::Luna, Effort::Low), task, "the guardian", None);
        assert!(
            searching.starts_with("{\"model\":\"gpt-6-luna\",\"reasoning\":{\"effort\":\"low\"}")
                && searching.ends_with(
                    ",\"input\":\"the guardian\",\"tools\":[{\"type\":\"web_search\",\"search_context_size\":\"high\",\"return_token_budget\":\"unlimited\",\"external_web_access\":true}],\"background\":true,\"store\":false}"
                ),
            "the same body with the web search among its tools: {searching}"
        );
    }

    fn status_of(body: &[u8]) -> Result<WorkStatus, String> {
        parse_status(200, body).map_err(|fault| fault.to_string())
    }

    const NO_TEXT: &str = "status 200, nothing written in the answer";

    #[test]
    fn pending_work() {
        let queued =
            br#"{"id": "resp_0a1B-2c_3", "object": "response", "status": "queued", "error": null, "output": []}"#;
        assert_eq!(status_of(queued), Ok(WorkStatus::Pending("resp_0a1B-2c_3".into(), Standing::Queued)));
        let in_progress = br#"{"id": "resp_9", "status": "in_progress", "error": null}"#;
        assert_eq!(status_of(in_progress), Ok(WorkStatus::Pending("resp_9".into(), Standing::InProgress)));
        assert_eq!(poll_address("resp_9").as_deref(), Some("https://api.openai.com/v1/responses/resp_9"));
        for invalid in ["", "resp 9", "resp/../9", "resp?x=1", "r\u{E4}sp"] {
            assert_eq!(poll_address(invalid), None, "{invalid:?}");
        }
        let odd = br#"{"id": "a/b", "status": "queued"}"#;
        assert_eq!(status_of(odd), Err(NO_TEXT.into()), "a name that could lead elsewhere is asked after nowhere");
    }

    #[test]
    fn finished_work() {
        let done = br###"{"id": "resp_9", "status": "completed", "error": null, "incomplete_details": null,
            "output": [
                {"id": "rs_1", "type": "reasoning", "summary": [{"type": "summary_text", "text": "thinking"}]},
                {"id": "msg_1", "type": "message", "role": "assistant", "status": "completed", "content": [
                    {"type": "output_text", "text": "A plan for **Friday**.\n\n## Decided\n\n- \"Go\" ahead\r\n- Kse", "annotations": []}
                ]}
            ],
            "usage": {"input_tokens": 1450, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 2439, "output_tokens_details": {"reasoning_tokens": 2300}, "total_tokens": 3889}}"###;
        assert_eq!(
            status_of(done),
            Ok(WorkStatus::Done(Summary {
                text: "A plan for **Friday**.\n\n## Decided\n\n- \"Go\" ahead\n- Kse".into(),
                usage: Usage { input: 1450, output: 2439, ..Usage::default() }
            })),
            "the text of the message, not the model's thought before it, with lines that end as ours do, and what was counted for it"
        );
        assert_eq!(parse_usage(done), Usage { input: 1450, output: 2439, ..Usage::default() });
        let searched = br#"{"status": "completed", "output": [
            {"type": "web_search_call", "action": {"type": "search", "query": "x"}},
            {"type": "web_search_call", "action": {"type": "open_page", "url": "https://a.example/"}},
            {"type": "web_search_call", "action": {"type": "search", "query": "y"}},
            {"type": "message", "content": [{"type": "output_text", "text": "A page."}]}]}"#;
        assert_eq!(
            (search_micros(searched), search_micros(done), search_micros(&[0xFF])),
            (20_000, 0, 0),
            "a cent for each search of the browser's step, nothing for a page it opened or for work that searched nothing"
        );
        let incomplete = br#"{"id": "resp_9", "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}, "output": [], "usage": {"input_tokens": 1450, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 1408}, "output_tokens": 16000}}"#;
        assert_eq!(
            status_of(incomplete),
            Err("refused: the model's work ended as incomplete, max_output_tokens".into())
        );
        assert_eq!(
            parse_usage(incomplete),
            Usage { input: 1450, cache_writes: 1408, output: 16000, ..Usage::default() },
            "work that ended without a summary is paid for as far as it went"
        );
        assert_eq!(
            (parse_usage(br#"{"id": "resp_9", "status": "queued", "usage": null}"#), parse_usage(&[0xFF, 0xFE])),
            (Usage::default(), Usage::default()),
            "work still to come has counted nothing, and neither has what is no text"
        );
        let busy = br#"{"error": {"message": "Slow down.", "code": "rate_limit_exceeded"}}"#;
        assert_eq!(
            [429, 503, 400].map(|status| parse_status(status, busy).map_err(|fault| fault.is_permanent())),
            [Err(false), Err(false), Err(true)],
            "a request refused for too many askings or by a service in no state to answer fails this time, as the notes, the news, recollections and auto mode take any failure this time"
        );
        let failed = br#"{"id": "resp_9", "status": "failed", "error": {"code": "server_error", "message": "The model failed."}}"#;
        assert_eq!(status_of(failed), Err("refused: status 200, server_error: The model failed.".into()));
        assert_eq!(
            status_of(br#"{"status": "cancelled"}"#),
            Err("refused: the model's work ended as cancelled".into())
        );
        let refusal = br#"{"status": "completed", "output": [{"type": "message", "content": [{"type": "refusal", "refusal": "No."}]}]}"#;
        for body in [&b""[..], b"{}", b"<html>502</html>", br#"{"status": "completed", "output": []}"#, refusal] {
            assert_eq!(status_of(body), Err(NO_TEXT.into()), "{:?}", core::str::from_utf8(body));
        }
    }

    #[test]
    fn polls() {
        let done = br#"{"id": "resp_9", "status": "completed", "output": [{"type": "message", "content": [{"type": "output_text", "text": "A rundown."}]}], "usage": {"input_tokens": 103746, "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 103680}, "output_tokens": 17314}}"#;
        assert!(matches!(
            parse_poll(200, done),
            Poll::Done(Summary {
                usage: Usage {
                    input: 103_746,
                    cache_reads: 0,
                    cache_writes: 103_680,
                    cache_writes_12h: 0,
                    output: 17_314,
                    model: Model::Luna
                },
                ..
            })
        ));
        assert_eq!(
            ["in_progress", "queued"]
                .map(|state| parse_poll(200, format!("{{\"id\": \"resp_9\", \"status\": \"{state}\"}}").as_bytes())),
            [Poll::Pending(Standing::InProgress), Poll::Pending(Standing::Queued)],
            "work still to come, as the service says it stands"
        );
        let gone = br#"{"error": {"message": "Response with id 'resp_9' not found.", "type": "invalid_request_error", "code": null}}"#;
        assert!(
            matches!(parse_poll(404, gone), Poll::Failed(Fault::Refused(_))),
            "work the service has let go of is not asked after again"
        );
        assert!(
            matches!(
                parse_poll(200, br#"{"id": "resp_9", "status": "completed", "output": []}"#),
                Poll::Failed(Fault::Temporary(_))
            ),
            "work that is done and holds no text is done all the same"
        );
        for status in [429, 500, 502, 503] {
            assert!(
                matches!(parse_poll(status, b"<html>busy</html>"), Poll::Retry(Fault::Temporary(_))),
                "status {status} is asked again at the next look"
            );
        }
        assert!(matches!(
            parse_poll(401, br#"{"error": {"message": "bad key", "code": "invalid_api_key"}}"#),
            Poll::Failed(Fault::Refused(reason)) if reason == "status 401, the service does not know the key (invalid_api_key)"
        ));
        assert_eq!(poll_target("resp_9").map(|target| target.path).ok().as_deref(), Some("/v1/responses/resp_9"));
        assert!(
            matches!(poll_target("a/b"), Err(Fault::Temporary(_))),
            "a name that could lead elsewhere leads nowhere"
        );
        let done = WorkStatus::Done(Summary { text: "x".into(), usage: Usage::default() });
        assert_eq!(
            (done.to_log_line(), WorkStatus::Pending("resp_9".into(), Standing::Queued).to_log_line()),
            ("the answer was there at once", "the model works on it")
        );
        assert!(matches!(Poll::from_fault(&Fault::NoKey), Poll::Failed(Fault::NoKey)), "without a key, never");
        assert!(matches!(
            Poll::from_fault(&Fault::Offline("asleep".into())),
            Poll::Retry(Fault::Temporary(reason)) if reason == "not in a network: asleep"
        ));
        let summary = Summary { text: "x".into(), usage: Usage::default() };
        let incomplete =
            br#"{"id": "resp_9", "status": "incomplete", "usage": {"input_tokens": 1450, "output_tokens": 16000}}"#;
        let usage = Usage { input: 1450, output: 16000, ..Usage::default() };
        assert_eq!(
            parse_poll(200, incomplete).cost(incomplete),
            Some(Cost::Failed(usage, 0)),
            "work that ended without a summary is paid for"
        );
        let searched = br#"{"id": "resp_9", "status": "incomplete", "output": [{"type": "web_search_call", "action": {"type": "search", "query": "x"}}], "usage": {"input_tokens": 1450, "output_tokens": 16000}}"#;
        assert_eq!(
            parse_poll(200, searched).cost(searched),
            Some(Cost::Failed(usage, 10_000)),
            "and so is a search it made before it ended"
        );
        assert_eq!(
            [Poll::Pending(Standing::Queued), Poll::Retry(Fault::Temporary("x".into()))]
                .map(|poll| poll.cost(searched)),
            [None; 2],
            "a look at work still to come books nothing"
        );
        assert_eq!(
            Poll::Done(summary).cost(searched),
            Some(Cost::Done(10_000)),
            "work found done is booked at its usage and its searches"
        );
    }

    #[test]
    fn works() {
        let works = [
            Work::Summary(SummaryKind::Note),
            Work::Summary(SummaryKind::Talk),
            Work::Recollection,
            Work::AutoReply,
            Work::Page,
            Work::BellCalls,
            Work::RundownLines,
        ];
        assert_eq!(
            works.map(|work| work.to_name("Gr\u{FC}\u{DF}e")),
            [
                "5 characters summarized",
                "5 characters summarized",
                "a day recollected",
                "a message of auto mode written",
                "a step of the browser's taken",
                "a request to the bell read",
                "a request for the rundowns read"
            ],
            "each by its name in the log, a summary by its input's characters, not bytes"
        );
        assert_eq!(
            works.map(Work::task),
            [Task::Summary, Task::Summary, Task::Recollect, Task::Auto, Task::Web, Task::Bell, Task::Rundowns],
            "each written by the model of its own use"
        );
        assert_eq!(
            works.map(Work::entry),
            [
                Entry::one(Kind::NoteSummary),
                Entry::one(Kind::TalkSummary),
                Entry::work(Kind::Chat),
                Entry::one(Kind::Auto),
                Entry::one(Kind::Web),
                Entry::one(Kind::Bell),
                Entry::work(Kind::Rundown)
            ],
            "a recollection adds to the conversation's cost and a request to the rundowns', and each counts none; any other is one of its kind"
        );
        assert_eq!(RUNDOWN, (Task::News, Entry::one(Kind::Rundown)), "a day's rundown, by the news' model");
    }

    #[test]
    fn answer_booked_once() {
        let done = br#"{"id": "resp_9", "status": "completed", "output": [{"type": "message", "content": [{"type": "output_text", "text": "A rundown."}]}], "usage": {"input_tokens": 1000, "output_tokens": 2000}}"#;
        let cut = br#"{"id": "resp_9", "status": "incomplete", "output": [], "usage": {"input_tokens": 1000, "output_tokens": 2000}}"#;
        let pending = br#"{"id": "resp_9", "status": "in_progress"}"#;
        let refused = br#"{"error": {"message": "Slow down.", "code": "rate_limit_exceeded"}}"#;
        let (usage, entry) = (Usage { input: 1000, output: 2000, ..Usage::default() }, Entry::one(Kind::Rundown));
        let asked = |body: &[u8]| status_cost(&parse_status(200, body), body);
        let looked = |body: &[u8]| parse_poll(200, body).cost(body);
        assert_eq!(
            (asked(done), looked(done)),
            (Some(Cost::Done(usage.micros())), Some(Cost::Done(usage.micros()))),
            "an answer that is there at once and a look that finds the work done cost the same"
        );
        assert_eq!((asked(pending), looked(pending)), (None, None), "work still to come costs nothing yet");
        assert_eq!(
            (asked(cut), looked(cut)),
            (Some(Cost::Failed(usage, 0)), Some(Cost::Failed(usage, 0))),
            "work that came to nothing costs what the service counted"
        );
        assert_eq!(
            status_cost(&parse_status(429, refused), refused),
            Some(Cost::Failed(Usage::default(), 0)),
            "a request refused counted nothing"
        );
        let mut ledger = Ledger::default();
        assert!(!Cost::Failed(Usage::default(), 0).book(&mut ledger, 2026, 10, entry));
        assert_eq!(ledger, Ledger::default(), "and is no entry");
        assert!(Cost::Done(usage.micros()).book(&mut ledger, 2026, 10, entry));
        assert!(Cost::Failed(usage, 0).book(&mut ledger, 2026, 10, entry));
        assert!(Cost::Done(0).book(&mut ledger, 2026, 10, entry), "work done is one of its kind whatever it cost");
        let mut expected = Ledger::default();
        (0..2).for_each(|_| expected.add(2026, 10, entry, usage.micros()));
        expected.add(2026, 10, entry, 0);
        assert_eq!(ledger, expected, "each answer is booked as its kind is");
    }

    #[test]
    fn log_lines() {
        let written = Summary { text: "Gr\u{FC}\u{DF}e".into(), usage: Usage::default() };
        let usage = Usage::default().to_log_line();
        assert_eq!(
            written.to_written_line(&Work::Recollection.to_name(""), 900, 3),
            format!("openai: a day recollected in 5 characters in 900 ms, asked after 3 times, {usage}")
        );
        assert_eq!(
            written.to_rundown_line("news", 42),
            format!("openai: the day's news written in 5 characters, 42 s after it was asked for, {usage}")
        );
        assert_eq!(
            [WorkStatus::Pending("resp_9".into(), Standing::Queued), WorkStatus::Done(written)]
                .map(|status| to_asked_line("news", "abc", 12, &status)),
            [
                "openai: the day's news asked for from 3 characters in 12 ms, the model works on it",
                "openai: the day's news asked for from 3 characters in 12 ms, the answer was there at once"
            ]
        );
    }

    #[test]
    fn cancelled_work() {
        assert_eq!(
            cancel_target("resp_9").map(|target| target.path).ok().as_deref(),
            Some("/v1/responses/resp_9/cancel")
        );
        assert!(matches!(cancel_target("a/b"), Err(Fault::Temporary(_))), "a name that could lead elsewhere");
        let cancelled = br#"{"id": "resp_9", "status": "cancelled", "error": null, "usage": null}"#;
        assert!(
            matches!(parse_poll(200, cancelled), Poll::Failed(Fault::Refused(reason)) if reason.ends_with("cancelled")),
            "the answer to a cancelling reads as a poll's: the work ended"
        );
        assert_eq!(parse_usage(cancelled), Usage::default(), "work cancelled before it counted anything costs nothing");
    }

    #[test]
    fn poll_schedule() {
        let summary = Summary { text: "A rundown.".into(), usage: Usage::default() };
        let mut watch = Watch::new(1000);
        assert!(watch.note_poll(Poll::Retry(Fault::Temporary("broken".into())), 1000).is_none(), "asked again");
        assert!(watch.note_poll(Poll::Retry(Fault::Temporary("status 429".into())), 2000).is_none());
        assert!(watch.note_poll(Poll::Pending(Standing::Queued), 3000).is_none());
        assert!(
            matches!(watch.note_poll(Poll::Done(summary.clone()), 4000), Some(Ok((actual, 4))) if actual == summary),
            "asked after again whatever kept it from coming, and how often is told"
        );
        let failed = Watch::new(1000).note_poll(Poll::Failed(Fault::Refused("gone".into())), 1000);
        assert!(matches!(failed, Some(Err(Fault::Refused(_)))));
        assert!(
            matches!(
                Watch::new(0).note_poll(Poll::Done(summary), 2 * PATIENCE_SECONDS * NANOS_PER_SECOND),
                Some(Ok((_, 1)))
            ),
            "work that came is had, however late"
        );
        let (mut watch, mut looks) = (Watch::new(1000), Vec::new());
        let end = loop {
            let (at, is_held) = watch.next();
            looks.push(((at - 1000) / NANOS_PER_SECOND, is_held));
            if let Some(end) = watch.note_poll(Poll::Pending(Standing::InProgress), at) {
                break end;
            }
        };
        let fast: Vec<_> = (1..=10).map(|look| (2 * look, true)).collect();
        let slow: Vec<_> = (50..PATIENCE_SECONDS).step_by(30).map(|at| (at, false)).collect();
        assert_eq!(looks, [fast, slow].concat());
        assert!(
            matches!(end, Err(Fault::Temporary(reason)) if reason == "the model's work did not come in 300 s"),
            "the last at 290 s: no look is made past the time the work is waited for"
        );
        assert_eq!(
            Watch::new(0).next(),
            (POLL_SECONDS * NANOS_PER_SECOND, true),
            "work owed from a stay long before is asked after at once, over the network its asking took"
        );
    }

    #[test]
    fn jobs_by_digest() {
        const PATIENCE_NS: u64 = PATIENCE_SECONDS * NANOS_PER_SECOND;
        let now = 2 * PATIENCE_NS;
        assert!(is_awaited(now, now) && is_awaited(now - PATIENCE_NS + 1, now) && !is_awaited(now - PATIENCE_NS, now));
        let job = |digest: &str, id: &str, start_ns: u64| Job { digest: digest.into(), id: id.into(), start_ns };
        let mut jobs = Jobs::default();
        jobs.add(job("a", "resp_1", now), now);
        jobs.add(job("b", "resp_2", now), now);
        jobs.add(job("a", "resp_3", now), now);
        let mut take = |digest: &str, at: u64| jobs.take(digest, at).map(|job| job.id);
        assert_eq!(take("c", now), None, "another request is owed nothing");
        assert_eq!(take("a", now).as_deref(), Some("resp_3"), "the work asked for last");
        assert_eq!(take("a", now), None, "once");
        assert_eq!(take("b", now + PATIENCE_NS), None, "work waited for no more is owed nothing");
        let mut jobs = Jobs::default();
        jobs.add(job("d", "resp_4", 0), 0);
        jobs.add(job("e", "resp_5", now), now);
        assert_eq!(jobs.0.len(), 1, "what is waited for no more goes as other work is owed");
    }
}
