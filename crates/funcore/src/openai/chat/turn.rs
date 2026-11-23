//! A turn over the WebSocket: the thread state the socket holds, and its retry.

use super::answer::Answer;
use super::request::response_create;
use crate::openai::fault::Fault;
use crate::openai::models::Thinking;
use crate::openai::refusal::Refusal;
use alloc::string::String;

pub(super) const PREVIOUS_NOT_FOUND: &str = "previous_response_not_found";
const CONNECTION_LIMIT: &str = "websocket_connection_limit_reached";

#[derive(Clone, PartialEq, Eq, Debug)]
struct ThreadMark {
    count: usize,
    last: Option<String>,
}

impl ThreadMark {
    fn new(items: &[&str], output: &[String]) -> ThreadMark {
        let last = output.last().map(String::as_str).or(items.last().copied()).map(String::from);
        ThreadMark { count: items.len() + output.len(), last }
    }

    fn is_prefix_of(&self, items: &[&str]) -> bool {
        self.count <= items.len() && self.count.checked_sub(1).map(|at| items[at]) == self.last.as_deref()
    }
}

/// What a turn over the WebSocket does after the service refused its request.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Retry {
    // The service no longer holds the answer the request went on from: the thread goes again in full.
    FullThread,
    // The socket has stood as long as it may: the turn fails this time, and the next goes over a new one.
    NewSocket(Fault),
    Fail(Fault),
}

/// The retry of a refused request; `is_continued` where it went on from an answer the socket holds.
pub fn decide_retry(refusal: &Refusal, status: Option<u16>, is_continued: bool) -> Retry {
    if refusal.code == PREVIOUS_NOT_FOUND && is_continued {
        Retry::FullThread
    } else if refusal.code == CONNECTION_LIMIT {
        Retry::NewSocket(Fault::Temporary(refusal.to_log_line(status)))
    } else {
        Retry::Fail(Fault::from_refusal(status, refusal))
    }
}

/// The thread state of the conversation's WebSocket between turns: the id of the answer the service holds, with the items it holds with it.
#[derive(Default)]
pub struct SocketThread {
    previous: Option<(String, ThreadMark)>,
}

impl SocketThread {
    /// The request of a turn of the thread `items` to `thinking`, whether it goes on from the answer the socket holds, and the item it starts at: those new to that answer where the thread goes on from it and `is_full` is false, else all; that answer is let go of, since a failed request has the service let go of it.
    pub fn next_turn(
        &mut self,
        (instructions, items, tools): (&str, &[&str], &str),
        thinking: Thinking,
        is_full: bool,
    ) -> (String, bool, usize) {
        let previous = self.previous.take().filter(|(_, mark)| !is_full && mark.is_prefix_of(items));
        let (id, start) = previous.map_or((None, 0), |(id, mark)| (Some(id), mark.count));
        let request = response_create(thinking, instructions, &items[start..], tools, id.as_deref());
        (request, id.is_some(), start)
    }

    /// Takes the end of a turn's answer to the thread `items`: the socket holds it by its id, with the items and those it added.
    pub fn note_answer(&mut self, items: &[&str], answer: &Answer) {
        self.previous = answer.id.clone().map(|id| (id, ThreadMark::new(items, &answer.output)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::output::COMPLETED;
    use alloc::vec;

    #[test]
    fn thread_mark_prefix() {
        let (a, b, c) = ("{\"n\":1}", "{\"n\":2}", "{\"n\":3}");
        let mark = ThreadMark::new(&[a], &[b.into()]);
        assert_eq!(mark, ThreadMark { count: 2, last: Some(b.into()) });
        assert!(mark.is_prefix_of(&[a, b]) && mark.is_prefix_of(&[a, b, c]), "the thread as it was kept, and grown");
        assert!(!mark.is_prefix_of(&[a]) && !mark.is_prefix_of(&[a, c, b]), "a shorter thread, or another one");
        assert!(!mark.is_prefix_of(&["{\"type\":\"compaction\"}", c]), "nor one that was folded");
        let empty = ThreadMark::new(&[], &[]);
        assert!(empty.is_prefix_of(&[]) && empty.is_prefix_of(&[a]), "what went ahead of a thread with nothing in it");
    }

    fn done(id: &str) -> Answer {
        Answer { id: Some(id.into()), status: COMPLETED.into(), ..Answer::default() }
    }

    #[test]
    fn new_items_only() {
        let (a, b, c) = ("{\"n\":1}", "{\"n\":2}", "{\"n\":3}");
        let mut thread = SocketThread::default();
        let chat = Thinking::STANDARD;
        let next_turn =
            |thread: &mut SocketThread, items: &[&str], is_full| thread.next_turn(("x", items, "[]"), chat, is_full);
        let (request, is_continued, start) = next_turn(&mut thread, &[a], false);
        assert_eq!((is_continued, start), (false, 0), "a socket that keeps nothing is sent the thread whole");
        assert!(request.contains("\"input\":[{\"n\":1}]") && !request.contains("previous_response_id"));
        let answer = Answer { output: vec![b.into()], ..done("resp_1") };
        thread.note_answer(&[a], &answer);
        let (request, is_continued, start) = next_turn(&mut thread, &[a, b, c], false);
        assert_eq!((is_continued, start), (true, 2), "the next carries what is new to the answer and its items");
        assert!(request.contains("\"previous_response_id\":\"resp_1\"") && request.contains("\"input\":[{\"n\":3}]"));
        assert!(!next_turn(&mut thread, &[a, b, c], false).1, "the answer a request went on from is kept no more");
        thread.note_answer(&[a], &answer);
        assert!(!next_turn(&mut thread, &[a, b, c], true).1, "a thread that goes whole");
        thread.note_answer(&[a], &answer);
        assert!(!next_turn(&mut thread, &[a], false).1, "and one that does not begin with what is kept");
    }

    #[test]
    fn retries() {
        let with_code = |code: &str| Refusal { code: code.into(), message: "x".into() };
        assert_eq!(
            [
                decide_retry(&with_code(PREVIOUS_NOT_FOUND), Some(400), true),
                decide_retry(&with_code(PREVIOUS_NOT_FOUND), Some(400), false),
                decide_retry(&with_code(CONNECTION_LIMIT), Some(400), false),
                decide_retry(&with_code("x"), Some(503), true)
            ],
            [
                Retry::FullThread,
                Retry::Fail(Fault::Refused("status 400, previous_response_not_found: x".into())),
                Retry::NewSocket(Fault::Temporary("status 400, websocket_connection_limit_reached: x".into())),
                Retry::Fail(Fault::Temporary("status 503, x: x".into()))
            ],
            "an answer the service holds no more has the thread go whole, a socket that has stood as long as it may is spent this time, and any other refusal fails the turn as its status says"
        );
        assert_eq!(
            Fault::from_refusal(None, &with_code("server_error")),
            Fault::Refused("server_error: x".into()),
            "an answer that failed comes with no status, and is refused"
        );
    }
}
