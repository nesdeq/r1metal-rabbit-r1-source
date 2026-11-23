use super::{Account, Session};
use crate::log;
use crate::net::websocket::Incoming;
use crate::wifi::radio::NetworkDown;
use r1_funcore::openai::chat::answer::{Answer, End, Event, parse_event};
use r1_funcore::openai::chat::turn::{Retry, SocketThread, decide_retry};
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::models::{TIMEOUT, Task, chat};
use std::collections::VecDeque;
use std::io;
use std::time::Instant;

type TurnRequest<'a> = (&'a str, &'a [&'a str], &'a str);

/// The conversation's requests to OpenAI in one stay in the network.
pub struct Conversation {
    account: Account,
    down: NetworkDown,
    socket: Option<TurnSocket>,
}

impl Conversation {
    /// Requests with the account's key in the stay `down`; nothing connects before it is needed.
    pub fn new(account: &Account, down: &NetworkDown) -> Conversation {
        Conversation { account: account.clone(), down: down.clone(), socket: None }
    }

    /// Opens the turns' WebSocket, so that a turn waits for no handshake once a recorded message is handed over.
    pub fn prepare(&mut self) {
        if let Err(reason) = self.socket() {
            log!("openai: the conversation's WebSocket is not opened ahead: {reason}");
        }
    }

    /// Puts a turn to the language model over the WebSocket, handing `on_event` what its answer writes and searches, and books the answer as the conversation's, a turn of its own where `is_first`. A failed answer is booked as far as the service counted it.
    pub fn turn(
        &mut self,
        request: TurnRequest<'_>,
        is_first: bool,
        on_event: impl FnMut(Event),
    ) -> Result<Answer, Fault> {
        let account = self.account.clone();
        let socket = self.socket()?;
        let result = socket.turn(&account, request, is_first, on_event);
        if socket.is_spent {
            self.close_socket();
        }
        result
    }

    fn socket(&mut self) -> Result<&mut TurnSocket, Fault> {
        match &mut self.socket {
            Some(socket) => Ok(socket),
            slot => Ok(slot.insert(TurnSocket::open(&self.account, &self.down)?)),
        }
    }

    fn close_socket(&mut self) {
        if let Some(socket) = self.socket.take() {
            socket.session.close();
        }
    }
}

impl Drop for Conversation {
    fn drop(&mut self) {
        self.close_socket();
    }
}

const SOCKET_FAILED: &str = "the WebSocket could not carry the turn";

struct TurnSocket {
    session: Session<()>,
    thread: SocketThread,
    pending: VecDeque<String>,
    is_spent: bool,
}

impl TurnSocket {
    fn open(account: &Account, down: &NetworkDown) -> Result<TurnSocket, Fault> {
        Ok(TurnSocket::new(Session::reach(account, chat::ADDRESS, down, TIMEOUT, ())?))
    }

    fn new(session: Session<()>) -> TurnSocket {
        TurnSocket { session, thread: SocketThread::default(), pending: VecDeque::new(), is_spent: false }
    }

    fn is_open(&mut self) -> bool {
        loop {
            match self.session.next(false) {
                Ok(Incoming::Message(message)) => self.pending.push_back(message),
                Ok(Incoming::Nothing) => return true,
                Ok(Incoming::Closed) => return false,
                Err(e) => {
                    log!("openai: the conversation's WebSocket is not read before a turn: {e}");
                    return false;
                }
            }
        }
    }

    fn next_event(&mut self) -> io::Result<Option<Event>> {
        let message = match self.pending.pop_front() {
            Some(message) => message,
            None => match self.session.next(true)? {
                Incoming::Message(message) => message,
                Incoming::Nothing | Incoming::Closed => return Ok(None),
            },
        };
        let event = parse_event(&message);
        self.session.give_back(message);
        Ok(Some(event))
    }

    fn spend(&mut self, fault: Fault) -> Fault {
        self.is_spent = true;
        fault
    }

    fn turn(
        &mut self,
        account: &Account,
        request: TurnRequest<'_>,
        is_first: bool,
        mut on_event: impl FnMut(Event),
    ) -> Result<Answer, Fault> {
        account.check_limit()?;
        let (start_time, items, thinking) = (Instant::now(), request.1, account.thinking(Task::Chat));
        let mut is_full_thread = false;
        loop {
            let (message, is_continued, start) = self.thread.next_turn(request, thinking, is_full_thread);
            // A socket the service closed while it lay idle is found so before the request goes, which nothing of the turn has reached.
            let is_sent = self.is_open()
                && self.session.send(&message).inspect_err(|e| log!("openai: a turn is not sent: {e}")).is_ok();
            if !is_sent {
                return Err(self.spend(Fault::Temporary(SOCKET_FAILED.into())));
            }
            // From here on the service may have taken the request and be paid for it: a socket that fails before the end fails the turn, which is not put again.
            let end = loop {
                let event = match self.next_event() {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        return Err(self.spend(Fault::Temporary("the WebSocket closed before the answer's end".into())));
                    }
                    Err(e) => return Err(self.spend(e.into())),
                };
                match event {
                    Event::End(end) => break end,
                    event => on_event(event),
                }
            };
            let answer = match end {
                End::Refused(refusal, status) => {
                    account.note_refusal(status, &refusal);
                    match decide_retry(&refusal, status, is_continued) {
                        Retry::FullThread => {
                            is_full_thread = true;
                            continue;
                        }
                        Retry::NewSocket(fault) => return Err(self.spend(fault)),
                        Retry::Fail(fault) => return Err(fault),
                    }
                }
                End::Fault(reason, answer) => {
                    book_failed(account, is_first, &answer);
                    // An answer that failed comes with no status.
                    return Err(Fault::from_refusal(None, &reason));
                }
                End::Done(answer) => answer,
            };
            book_answer(account, is_first, &answer, (start_time, start, items.len()));
            self.thread.note_answer(items, &answer);
            return Ok(answer);
        }
    }
}

fn book_failed(account: &Account, is_first: bool, answer: &Answer) {
    account.add(|ledger, year, month| answer.add_failed_to_ledger(ledger, year, month, is_first));
    log!("{}", answer.to_failed_line());
}

fn book_answer(
    account: &Account,
    is_first: bool,
    answer: &Answer,
    (start, first_item, item_count): (Instant, usize, usize),
) {
    account.add(|ledger, year, month| {
        answer.add_to_ledger(ledger, year, month, is_first);
        true
    });
    log!("{}", answer.to_turn_line(first_item, item_count, start.elapsed().as_millis()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::served::{Peer, peer};
    use r1_funcore::openai::chat::request::{function_output, user_message};
    use r1_funcore::openai::request::parse_target;
    use std::io::Write;
    use std::thread::JoinHandle;

    const TOOLS: &str = "[{\"type\":\"function\",\"name\":\"save_memory\"}]";

    fn socket_to<T: Send + 'static>(
        service: impl FnOnce(&mut Peer) -> T + Send + 'static,
    ) -> (TurnSocket, JoinHandle<T>) {
        let (socket, server) = peer(service);
        (TurnSocket::new(Session { socket, context: () }), server)
    }

    fn created(name: &str) -> String {
        format!("{{\"type\":\"response.created\",\"response\":{{\"id\":\"{name}\",\"status\":\"in_progress\"}}}}")
    }

    fn completed(name: &str, text: &str, has_calls: bool) -> String {
        let output = if has_calls {
            String::from(
                "{\"type\":\"function_call\",\"name\":\"save_memory\",\"call_id\":\"call_1\",\"arguments\":\"{}\"}",
            )
        } else {
            format!("{{\"type\":\"message\",\"content\":[{{\"type\":\"output_text\",\"text\":\"{text}\"}}]}}")
        };
        format!(
            "{{\"type\":\"response.completed\",\"response\":{{\"id\":\"{name}\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":10,\"output_tokens\":2}}}}}}"
        )
    }

    fn has_items(request: &str, items: &[&str]) -> bool {
        request.contains(&format!("\"input\":[{}]", items.join(",")))
    }

    fn previous(request: &str) -> Option<&str> {
        let (_, rest) = request.split_once("\"previous_response_id\":\"")?;
        rest.split_once('"').map(|(name, _)| name)
    }

    fn account() -> Account {
        crate::openai::tests::unusable()
    }

    #[test]
    fn new_socket_sends_full_thread() {
        let thread = [
            user_message("Hallo", &[]),
            String::from(
                "{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Servus\"}]}",
            ),
            user_message("Wie geht's?", &[]),
        ];
        let thread: Vec<&str> = thread.iter().map(String::as_str).collect();
        let (mut socket, server) = socket_to(|peer| {
            let turn = peer.read_text().unwrap();
            peer.write_text(&created("resp_1"));
            peer.write_text("{\"type\":\"response.output_text.delta\",\"delta\":\"Gut\"}");
            peer.write_text(&completed("resp_1", "", true));
            let round = peer.read_text().unwrap();
            peer.write_text(&created("resp_2"));
            peer.write_text(&completed("resp_2", "Gut.", false));
            (turn, round)
        });
        let mut events = Vec::new();
        let answer =
            socket.turn(&account(), ("You are the robot.", &thread, TOOLS), true, |event| events.push(event)).unwrap();
        assert_eq!(
            (answer.id.as_deref(), answer.calls().len(), events),
            (Some("resp_1"), 1, vec![Event::Created(Some("resp_1".into())), Event::Text("Gut".into())])
        );
        let output = function_output("call_1", "Kept.");
        let round: Vec<&str> =
            thread.iter().copied().chain(answer.output.iter().map(String::as_str)).chain([output.as_str()]).collect();
        let answer = socket.turn(&account(), ("You are the robot.", &round, TOOLS), false, |_| {}).unwrap();
        assert_eq!(answer.text(), "Gut.");
        let (turn, next_turn) = server.join().unwrap();
        assert!(
            previous(&turn).is_none() && has_items(&turn, &thread),
            "a socket that keeps no answer is sent the thread whole: {turn}"
        );
        assert!(
            previous(&next_turn) == Some("resp_1") && has_items(&next_turn, &[&output]),
            "and the round after a call of a function carries what the function answered alone: {next_turn}"
        );
        assert!(turn.contains("\"instructions\":\"You are the robot.\"") && turn.contains("\"ttl\":\"12h\""));
        socket.session.close();
    }

    #[test]
    fn lost_answer_resends_full_thread() {
        let thread = [user_message("Hallo", &[]), user_message("Und?", &[])];
        let thread: Vec<&str> = thread.iter().map(String::as_str).collect();
        let (mut socket, server) = socket_to(|peer| {
            peer.read_text().unwrap();
            peer.write_text(&created("resp_0"));
            peer.write_text(&completed("resp_0", "Ja.", false));
            let continued = peer.read_text().unwrap();
            peer.write_text("{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"code\":\"previous_response_not_found\",\"message\":\"Previous response with id 'resp_0' not found.\"}}");
            let full = peer.read_text().unwrap();
            peer.write_text(&created("resp_1"));
            peer.write_text(&completed("resp_1", "Ja.", false));
            peer.read_text().unwrap();
            peer.write_text("{\"type\":\"error\",\"status\":400,\"error\":{\"code\":\"websocket_connection_limit_reached\",\"message\":\"Responses websocket connection limit reached (60 minutes).\"}}");
            peer.read_text().unwrap();
            (continued, full)
        });
        let first = socket.turn(&account(), ("x", &thread[..1], TOOLS), true, |_| {}).unwrap();
        let next: Vec<&str> =
            thread[..1].iter().copied().chain(first.output.iter().map(String::as_str)).chain([thread[1]]).collect();
        let answer = socket.turn(&account(), ("x", &next, TOOLS), true, |_| {}).unwrap();
        assert!(answer.text() == "Ja." && !socket.is_spent);
        assert!(
            matches!(socket.turn(&account(), ("x", &next, TOOLS), true, |_| {}), Err(Fault::Temporary(text)) if text.contains("websocket_connection_limit_reached"))
                && socket.is_spent,
            "a socket that has stood as long as it may fails the turn and is spent"
        );
        assert!(
            matches!(socket.turn(&account(), ("x", &next, TOOLS), true, |_| {}), Err(Fault::Temporary(_))),
            "one that closed after the request went fails the turn, which may have been taken, and puts it nowhere again"
        );
        let (continued, full) = server.join().unwrap();
        assert!(previous(&continued) == Some("resp_0") && has_items(&continued, &[thread[1]]), "{continued}");
        assert!(previous(&full).is_none() && has_items(&full, &next), "asked again with the thread whole: {full}");
    }

    #[test]
    fn closed_socket_carries_no_turn() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let (opened_sender, opened_receiver) = std::sync::mpsc::channel::<()>();
        let (mut socket, server) = socket_to(move |peer| {
            // The service closes once the socket stands open, so that its close comes behind the opening's answer.
            opened_receiver.recv().unwrap();
            peer.0.write_all(&[0x88, 0x00]).unwrap();
            sender.send(()).unwrap();
            (peer.read_text(), peer.read_text())
        });
        opened_sender.send(()).unwrap();
        receiver.recv().unwrap();
        // The socket lies idle until the service's close has come.
        let mut pollfd = libc::pollfd { fd: socket.session.fd(), events: libc::POLLIN, revents: 0 };
        assert_eq!(unsafe { libc::poll(&mut pollfd, 1, 10_000) }, 1, "the close comes");
        let thread = [user_message("Hallo", &[])];
        let thread: Vec<&str> = thread.iter().map(String::as_str).collect();
        let result = socket.turn(&account(), ("x", &thread, TOOLS), true, |_| {});
        assert!(
            matches!(&result, Err(Fault::Temporary(text)) if text == SOCKET_FAILED) && socket.is_spent,
            "the turn fails, and the socket is spent: {result:?}"
        );
        socket.session.close();
        assert_eq!(server.join().unwrap(), (None, None), "and the service got the closes and no request");
    }

    #[test]
    fn refusal_reaches_account() {
        let account = account();
        let (mut socket, server) = socket_to(|peer| {
            peer.read_text().unwrap();
            peer.write_text("{\"type\":\"error\",\"status\":401,\"error\":{\"code\":\"invalid_api_key\",\"message\":\"Incorrect API key provided: sk-...abcd\"}}");
            peer.read_text().unwrap();
            peer.write_text(&created("resp_1"));
        });
        let thread = [user_message("Hallo", &[])];
        let thread: Vec<&str> = thread.iter().map(String::as_str).collect();
        let result = socket.turn(&account, ("x", &thread, TOOLS), true, |_| {});
        assert!(
            matches!(&result, Err(Fault::Refused(text)) if text == "status 401, the service does not know the key (invalid_api_key)")
                && !socket.is_spent,
            "{result:?}"
        );
        assert!(account.take_refused(), "the account has the key checked anew");
        let result = socket.turn(&account, ("x", &thread, TOOLS), true, |_| {});
        assert!(
            matches!(result, Err(Fault::Temporary(_))) && socket.is_spent,
            "a socket that closes in the middle of an answer is spent"
        );
        server.join().unwrap();
    }

    #[test]
    #[ignore]
    fn service_caches_turns_12_hours() {
        use crate::openai::Key;
        use r1_funcore::domain::chat::memory::Memory;
        use r1_funcore::domain::chat::tools::functions;
        use r1_funcore::domain::settings::Tools;
        use r1_funcore::openai::chat::request::build_tools;
        use r1_funcore::openai::key;
        let secret = std::env::var("OPENAI_API_KEY").expect("a key in OPENAI_API_KEY");
        let (account, down) = (account(), crate::wifi::radio::tests::up());
        let open = || {
            let key = Key { bearer: key::to_bearer(&secret), account: account.clone() };
            let socket = key.open(&parse_target(chat::ADDRESS).unwrap(), TIMEOUT, &down).unwrap();
            TurnSocket::new(Session { socket, context: () })
        };
        let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let instructions = format!(
            "Begun at {start}.\n\n{}",
            r1_funcore::domain::chat::instructions::to_instructions(&Memory::default(), None, &[], Tools::default())
        );
        let tools = build_tools(None, true, &functions(Tools::default()));
        let mut thread = vec![user_message("[Sunday 25 October 2026, 18:00, typed] Say only: ok.", &[])];
        let ask = |socket: &mut TurnSocket, thread: &[String], is_first: bool| {
            let items: Vec<&str> = thread.iter().map(String::as_str).collect();
            let mut text = String::new();
            let answer = socket
                .turn(&account, (&instructions, &items, &tools), is_first, |event| {
                    if let Event::Text(piece) = event {
                        text.push_str(&piece);
                    }
                })
                .unwrap();
            println!("{} items: {:?}, {}", items.len(), text, answer.usage.to_log_line());
            assert!(answer.is_complete() && answer.text() == text && !text.is_empty(), "{answer:?}");
            assert!(
                answer.usage.cache_writes > 0 && answer.usage.cache_writes_12h == answer.usage.cache_writes,
                "every token written into the cache is kept there 12 hours: {:?}",
                answer.usage
            );
            answer
        };
        let mut socket = open();
        let first = ask(&mut socket, &thread, true);
        assert!(
            first.usage.cache_writes + first.usage.cache_reads + 16 >= first.usage.input,
            "the first turn writes what no cache holds: {:?}",
            first.usage
        );
        thread.extend(first.output.iter().cloned());
        thread.push(user_message("[Sunday 25 October 2026, 18:01, typed] Again, only: ok.", &[]));
        let second = ask(&mut socket, &thread, true);
        assert!(
            second.usage.cache_reads >= first.usage.cache_writes,
            "the next turn on the socket reads what the first wrote: {:?}",
            second.usage
        );
        thread.extend(second.output.iter().cloned());
        thread.push(user_message("[Sunday 25 October 2026, 18:02, typed] Once more, only: ok.", &[]));
        let third = ask(&mut open(), &thread, true);
        assert!(
            third.usage.cache_reads >= second.usage.cache_reads,
            "a new socket, sent the thread whole, reads what the old one's turns wrote: {:?}",
            third.usage
        );
        socket.session.close();
    }

    #[test]
    fn no_key_no_request() {
        let mut conversation = Conversation::new(&account(), &crate::wifi::radio::tests::up());
        assert!(matches!(conversation.turn(("x", &[], TOOLS), true, |_| {}), Err(Fault::Temporary(_))));
        assert!(conversation.socket.is_none(), "nor a socket opened");
    }
}
