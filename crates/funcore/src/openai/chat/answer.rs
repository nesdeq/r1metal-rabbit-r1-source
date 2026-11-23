//! A turn's answer: the stream's events, the final answer and its cost.

use crate::base::json::items;
use crate::base::json::number_at;
use crate::base::json::text_at;
use crate::base::json::unescape;
use crate::base::json::value_at;
use crate::openai::ledger::{Entry, Kind, Ledger};
use crate::openai::output::{COMPLETED, MESSAGE, SEARCH_CALL, SEARCH_MICROS, output_text, search_count};
use crate::openai::refusal::{Refusal, parse_refusal};
use crate::openai::tokens::Usage;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const FUNCTION_CALL: &str = "function_call";
const COMPACTION: &str = "compaction";
const CITATION: &str = "url_citation";

fn to_single_line(item: &str) -> String {
    item.trim().replace(['\n', '\r'], " ")
}

fn item_type(item: &str) -> &str {
    text_at(item, &["type"]).unwrap_or_default()
}

pub(crate) fn is_compaction(item: &str) -> bool {
    item_type(item) == COMPACTION
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Source {
    pub(crate) title: String,
    pub(crate) url: String,
}

/// A function call of the model: the function's name, the call's id, and the arguments as JSON.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Call {
    pub name: String,
    pub id: String,
    pub arguments: String,
}

/// A finished answer: its id, final status, the reason it was cut short, its thread items, and its usage.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Answer {
    pub id: Option<String>,
    pub(crate) status: String,
    pub(crate) reason: Option<String>,
    pub output: Vec<String>,
    pub usage: Usage,
}

impl Answer {
    fn parse(response: &str) -> Answer {
        Answer {
            id: text_at(response, &["id"]).map(unescape),
            status: text_at(response, &["status"]).unwrap_or_default().into(),
            reason: text_at(response, &["incomplete_details", "reason"]).map(unescape),
            output: items(response, &["output"]).map(to_single_line).collect(),
            usage: Usage::parse(response),
        }
    }

    /// True if the model finished the answer.
    pub fn is_complete(&self) -> bool {
        self.status == COMPLETED
    }

    pub(crate) fn cut_reason(&self) -> Option<&str> {
        (!self.is_complete()).then(|| self.reason.as_deref().unwrap_or(&self.status))
    }

    fn items_of_type<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a String> + 'a {
        self.output.iter().filter(move |item| item_type(item) == kind)
    }

    /// The text of the answer's messages.
    pub fn text(&self) -> String {
        output_text(self.output.iter().map(String::as_str))
    }

    fn search_count(&self) -> u64 {
        search_count(self.output.iter().map(String::as_str))
    }

    pub(crate) fn search_terms(&self) -> Vec<String> {
        self.items_of_type(SEARCH_CALL)
            .filter_map(|item| {
                text_at(item, &["action", "query"]).or_else(|| text_at(item, &["action", "url"])).map(unescape)
            })
            .filter(|term| !term.trim().is_empty())
            .collect()
    }

    pub(crate) fn sources(&self) -> Vec<Source> {
        let add = |sources: &mut Vec<Source>, title: String, url: String| {
            if !url.is_empty() && !sources.iter().any(|source| source.url == url) {
                sources.push(Source { title, url });
            }
        };
        let mut sources: Vec<Source> = Vec::new();
        for part in self.items_of_type(MESSAGE).flat_map(|item| items(item, &["content"])) {
            for note in items(part, &["annotations"]).filter(|note| item_type(note) == CITATION) {
                let field = |name: &str| text_at(note, &[name]).map(unescape).unwrap_or_default();
                add(&mut sources, field("title"), field("url"));
            }
        }
        if sources.is_empty() {
            for item in self.items_of_type(SEARCH_CALL) {
                for source in items(item, &["action", "sources"]) {
                    add(&mut sources, String::new(), text_at(source, &["url"]).map(unescape).unwrap_or_default());
                }
            }
        }
        sources
    }

    fn is_counted(&self) -> bool {
        self.usage.is_counted() || self.search_count() > 0
    }

    /// Adds the answer's cost and web searches to the ledger in that month, as a turn of its own where it is the turn's first.
    pub fn add_to_ledger(&self, ledger: &mut Ledger, year: u32, month: u8, is_first: bool) {
        ledger.add(year, month, Entry::of(Kind::Chat, u64::from(is_first)), self.usage.micros());
        let searches = self.search_count();
        if searches > 0 {
            ledger.add(year, month, Entry::of(Kind::Search, searches), searches.saturating_mul(SEARCH_MICROS));
        }
    }

    /// Adds the cost of a failed answer to the ledger as far as the service counted it; true if it was added.
    pub fn add_failed_to_ledger(&self, ledger: &mut Ledger, year: u32, month: u8, is_first: bool) -> bool {
        let is_counted = self.is_counted();
        if is_counted {
            self.add_to_ledger(ledger, year, month, is_first);
        }
        is_counted
    }

    /// The log line of an answer `ms` after its turn began, its thread of `items` sent from `start` on.
    pub fn to_turn_line(&self, start: usize, items: usize, ms: u128) -> String {
        let reason = self.reason.as_deref().map_or_else(String::new, |reason| format!(" for {reason}"));
        format!(
            "openai: a turn of the conversation answered over its WebSocket, {} of {items} items sent in {ms} ms, {}{reason}, {}, {} searches",
            items - start,
            self.status,
            self.usage.to_log_line(),
            self.search_count()
        )
    }

    pub fn to_failed_line(&self) -> String {
        format!(
            "openai: a turn of the conversation failed, {}, {} searches",
            self.usage.to_log_line(),
            self.search_count()
        )
    }

    /// The answer's function calls, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.items_of_type(FUNCTION_CALL)
            .filter_map(|item| {
                Some(Call {
                    name: text_at(item, &["name"])?.into(),
                    id: text_at(item, &["call_id"])?.into(),
                    arguments: text_at(item, &["arguments"]).map(unescape).unwrap_or_default(),
                })
            })
            .collect()
    }
}

/// The end of a turn's answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum End {
    // Complete or cut short.
    Done(Answer),
    // The answer as far as it went is paid for as the service counted it.
    Fault(Refusal, Answer),
    // With the status the WebSocket's event gives.
    Refused(Refusal, Option<u16>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    // With the answer's id where the event gives it.
    Created(Option<String>),
    Text(String),
    // The model searches the web, or reads a page it found.
    Searching,
    End(End),
    Other,
}

pub fn parse_event(data: &str) -> Event {
    match text_at(data, &["type"]).unwrap_or_default() {
        "response.created" => Event::Created(text_at(data, &["response", "id"]).map(unescape)),
        "response.output_text.delta" => Event::Text(text_at(data, &["delta"]).map(unescape).unwrap_or_default()),
        "response.web_search_call.in_progress" | "response.web_search_call.searching" => Event::Searching,
        "response.output_item.added" if text_at(data, &["item", "type"]) == Some(SEARCH_CALL) => Event::Searching,
        "response.completed" | "response.incomplete" => Event::End(End::Done(Answer::parse(response_of(data)))),
        "response.failed" => {
            let response = response_of(data);
            let refusal = parse_refusal(response).unwrap_or_else(|| Refusal {
                code: text_at(response, &["status"]).unwrap_or_default().into(),
                message: "the answer failed".into(),
            });
            Event::End(End::Fault(refusal, Answer::parse(response)))
        }
        "error" => Event::End(End::Refused(
            parse_refusal(data).unwrap_or_else(|| Refusal {
                code: text_at(data, &["code"]).unwrap_or_default().into(),
                message: text_at(data, &["message"]).map(unescape).unwrap_or_default(),
            }),
            number_at::<u16>(data, &["status"]),
        )),
        _ => Event::Other,
    }
}

fn response_of(data: &str) -> &str {
    value_at(data, &["response"]).unwrap_or(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::chat::turn::PREVIOUS_NOT_FOUND;
    use alloc::vec;

    #[test]
    fn single_line_item() {
        assert_eq!(
            to_single_line("{\"a\": 1,\n  \"b\": \"x\\ny\"\r\n}"),
            "{\"a\": 1,   \"b\": \"x\\ny\"  }",
            "an item is one line in the thread, its escaped breaks as they are"
        );
    }

    #[test]
    fn stream_events() {
        assert_eq!(
            parse_event("{\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}"),
            Event::Created(Some("resp_1".into()))
        );
        assert_eq!(parse_event("{\"type\":\"response.created\"}"), Event::Created(None));
        assert_eq!(
            parse_event(
                "{\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"delta\":\"Regen, \\\"ab\\\" Mittag.\"}"
            ),
            Event::Text("Regen, \"ab\" Mittag.".into())
        );
        assert_eq!(
            parse_event("{\"type\":\"response.web_search_call.searching\",\"item_id\":\"ws_1\"}"),
            Event::Searching
        );
        assert_eq!(
            parse_event(
                "{\"type\":\"response.output_item.added\",\"item\":{\"type\":\"web_search_call\",\"id\":\"ws_1\"}}"
            ),
            Event::Searching
        );
        assert_eq!(
            parse_event("{\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"id\":\"msg_1\"}}"),
            Event::Other
        );
        assert_eq!(
            parse_event("{\"type\":\"error\",\"code\":\"rate_limit_exceeded\",\"message\":\"Slow down.\"}"),
            Event::End(End::Refused(
                Refusal { code: "rate_limit_exceeded".into(), message: "Slow down.".into() },
                None
            ))
        );
        assert_eq!(
            parse_event(
                "{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"code\":\"previous_response_not_found\",\"message\":\"Previous response with id 'resp_1' not found.\",\"param\":\"previous_response_id\"}}"
            ),
            Event::End(End::Refused(
                Refusal {
                    code: PREVIOUS_NOT_FOUND.into(),
                    message: "Previous response with id 'resp_1' not found.".into()
                },
                Some(400)
            )),
            "the WebSocket's refusal comes with the status of an answer"
        );
        let Event::End(End::Fault(refusal, answer)) = parse_event(
            "{\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"The model failed.\"},\"output\":[{\"type\":\"web_search_call\",\"action\":{\"type\":\"search\",\"query\":\"x\"}}],\"usage\":{\"input_tokens\":9000,\"input_tokens_details\":{\"cached_tokens\":8000,\"cache_write_tokens\":0},\"output_tokens\":300}}}",
        ) else {
            panic!("an answer that failed");
        };
        assert_eq!(refusal, Refusal { code: "server_error".into(), message: "The model failed.".into() });
        assert_eq!(
            (answer.is_complete(), answer.search_count(), answer.usage),
            (false, 1, Usage { input: 9000, cache_reads: 8000, output: 300, ..Usage::default() }),
            "an answer that failed tells what it searched and what the service counted for it, which is paid for"
        );
        assert_eq!((parse_event("{}"), parse_event("not json")), (Event::Other, Event::Other));
    }

    const DONE: &str = "{\"type\":\"response.completed\",\"sequence_number\":40,\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"incomplete_details\":null,\"instructions\":\"You are the robot.\",\"output\":[{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"gAAA\",\"summary\":[]},{\"id\":\"ws_1\",\"type\":\"web_search_call\",\"status\":\"completed\",\"action\":{\"type\":\"search\",\"query\":\"Wetter Wien morgen\",\"sources\":[{\"type\":\"url\",\"url\":\"https://wetter.at/wien\"}]}},{\"id\":\"ws_2\",\"type\":\"web_search_call\",\"status\":\"completed\",\"action\":{\"type\":\"open_page\",\"url\":\"https://wetter.at/wien\"}},{\"id\":\"ws_3\",\"type\":\"web_search_call\",\"status\":\"completed\",\"action\":{\"type\":\"search\",\"query\":\"ZAMG Wien\",\"sources\":[]}},{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"Regen ab Mittag, **12 Grad** ([wetter.at](https://wetter.at/wien)).\",\"annotations\":[{\"type\":\"url_citation\",\"start_index\":30,\"end_index\":61,\"url\":\"https://wetter.at/wien\",\"title\":\"Wetter Wien\"},{\"type\":\"url_citation\",\"url\":\"https://wetter.at/wien\",\"title\":\"again\"}]}]},{\"id\":\"fc_1\",\"type\":\"function_call\",\"status\":\"completed\",\"name\":\"save_memory\",\"call_id\":\"call_9\",\"arguments\":\"{\\\"content\\\":\\\"Lives in Vienna\\\",\\\"core\\\":true,\\\"replaces\\\":null}\"}],\"usage\":{\"input_tokens\":51234,\"input_tokens_details\":{\"cached_tokens\":50000,\"cache_write_tokens\":1000},\"output_tokens\":2100,\"output_tokens_details\":{\"reasoning_tokens\":1800},\"total_tokens\":53334}}}";

    #[test]
    fn finished_answer() {
        let Event::End(End::Done(answer)) = parse_event(DONE) else {
            panic!("the end of an answer");
        };
        assert!(answer.is_complete() && answer.reason.is_none());
        assert_eq!(answer.id.as_deref(), Some("resp_1"), "the answer's name, not that of one of its items");
        assert_eq!(
            answer.output.iter().map(|item| item_type(item)).collect::<Vec<&str>>(),
            ["reasoning", "web_search_call", "web_search_call", "web_search_call", "message", "function_call"],
            "every item, the encrypted thought among them, to go back with the next turn"
        );
        assert!(answer.output[0].contains("\"encrypted_content\":\"gAAA\""));
        assert_eq!(answer.text(), "Regen ab Mittag, **12 Grad** ([wetter.at](https://wetter.at/wien)).");
        assert_eq!(answer.search_count(), 2, "two searches are paid for; a page that was opened is not");
        assert_eq!(answer.search_terms(), ["Wetter Wien morgen", "https://wetter.at/wien", "ZAMG Wien"]);
        assert_eq!(
            answer.sources(),
            vec![Source { title: "Wetter Wien".into(), url: "https://wetter.at/wien".into() }],
            "a page cited twice is one source"
        );
        assert_eq!(
            answer.calls(),
            vec![Call {
                name: "save_memory".into(),
                id: "call_9".into(),
                arguments: "{\"content\":\"Lives in Vienna\",\"core\":true,\"replaces\":null}".into()
            }]
        );
        assert_eq!(
            answer.usage,
            Usage { input: 51_234, cache_reads: 50_000, cache_writes: 1_000, output: 2_100, ..Usage::default() }
        );
        assert_eq!(
            answer.usage.micros(),
            1_698,
            "234 in at the plain rate, 50,000 read from the cache, 1,000 written into it and 2,100 out: a sixth of a cent"
        );
        let incomplete = "{\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"output\":[{\"type\":\"web_search_call\",\"action\":{\"type\":\"search\",\"query\":\"x\",\"sources\":[{\"type\":\"url\",\"url\":\"https://a.org\"}]}}],\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}";
        let Event::End(End::Done(incomplete)) = parse_event(incomplete) else {
            panic!("the end of an answer cut short");
        };
        assert_eq!(
            (incomplete.is_complete(), incomplete.reason.as_deref(), incomplete.text()),
            (false, Some("max_output_tokens"), String::new())
        );
        assert_eq!(
            (
                incomplete.cut_reason(),
                answer.cut_reason(),
                Answer { status: "failed".into(), ..Answer::default() }.cut_reason()
            ),
            (Some("max_output_tokens"), None, Some("failed")),
            "why it was cut short, else how it stood"
        );
        assert_eq!(
            incomplete.sources(),
            vec![Source { title: String::new(), url: "https://a.org".into() }],
            "where nothing is cited, the pages the searches named"
        );
        assert!(
            is_compaction("{\"type\":\"compaction\",\"encrypted_content\":\"x\"}") && !is_compaction(&answer.output[0])
        );
        let (usage, cut_usage) = (answer.usage.to_log_line(), incomplete.usage.to_log_line());
        assert_eq!(
            [answer.to_turn_line(2, 5, 1200), incomplete.to_turn_line(0, 1, 9), answer.to_failed_line()],
            [
                format!(
                    "openai: a turn of the conversation answered over its WebSocket, 3 of 5 items sent in 1200 ms, completed, {usage}, 2 searches"
                ),
                format!(
                    "openai: a turn of the conversation answered over its WebSocket, 1 of 1 items sent in 9 ms, incomplete for max_output_tokens, {cut_usage}, 1 searches"
                ),
                format!("openai: a turn of the conversation failed, {usage}, 2 searches")
            ],
            "the items new to the socket of those the thread holds, how the answer stood and why it was cut, what it cost and searched"
        );
    }

    #[test]
    fn ledger_entries() {
        use crate::openai::ledger::Paid;
        let Event::End(End::Done(answer)) = parse_event(DONE) else {
            panic!("the end of an answer");
        };
        let mut ledger = Ledger::default();
        answer.add_to_ledger(&mut ledger, 2026, 10, true);
        answer.add_to_ledger(&mut ledger, 2026, 10, false);
        let october = ledger.months[0];
        assert_eq!(
            (october.paid[Kind::Chat as usize], october.paid[Kind::Search as usize]),
            (Paid { count: 1, micros: 2 * 1_698 }, Paid { count: 4, micros: 4 * SEARCH_MICROS }),
            "the turn's first answer counts the turn, every answer its cost and its searches"
        );
        assert!(answer.is_counted() && !Answer::default().is_counted());
        let search = Answer {
            output: vec!["{\"type\":\"web_search_call\",\"action\":{\"type\":\"search\",\"query\":\"x\"}}".into()],
            ..Answer::default()
        };
        assert!(search.is_counted(), "a search is paid for where no token was counted");
        let mut ledger = Ledger::default();
        assert!(
            !Answer::default().add_failed_to_ledger(&mut ledger, 2026, 10, true),
            "a failed answer that counted nothing"
        );
        assert_eq!(ledger, Ledger::default(), "is no entry");
        assert!(search.add_failed_to_ledger(&mut ledger, 2026, 10, true));
        assert_eq!(ledger.months[0].paid[Kind::Search as usize], Paid { count: 1, micros: SEARCH_MICROS });
    }
}
