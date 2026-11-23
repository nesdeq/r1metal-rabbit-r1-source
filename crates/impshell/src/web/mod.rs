use crate::log;
use crate::net::web;
use crate::openai::{Account, write};
use crate::storage::Data;
use crate::timekeeper::read_location;
use crate::voiced::{Part, Shared, Worker};
use crate::wifi::radio::{Hold, NetworkDown};
use r1_funcore::codec::http::Target;
use r1_funcore::domain::voice::{Phase, Query};
use r1_funcore::domain::web::state::{Browser, PROMPT};
use r1_funcore::domain::web::visit::{Outcome, Step, Visit};
use r1_funcore::domain::web::{HEADERS, check_head};
use r1_funcore::net::fetch::Fetched;
use r1_funcore::net::tls::Purpose;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::transcript::Subject;
use std::ffi::CStr;
use std::io;
use std::time::Instant;

/// The browser job's worker, with the partition that tells where the device is.
pub struct Visitor {
    data: Data,
}

/// The main loop's handle of the browser job.
pub type Web = Part<Visitor>;

impl Web {
    /// Starts the threads, idle until the screen shows; `data` holds the place the model's search starts from.
    pub fn start(account: Account, data: Data) -> io::Result<Web> {
        Part::spawn(Visitor { data }, false, account)
    }
}

// Asks for the page at `target` on a connection of its own, and reads the document or where the asker is sent on.
fn fetch(target: &Target, down: &NetworkDown) -> io::Result<Fetched> {
    let start = Instant::now();
    let stream = web::reach(target, Purpose::Http, down)?;
    web::fetch(stream, target, &HEADERS, start, |head| check_head(head, target))
}

impl Worker for Visitor {
    type State = Browser;
    type Stay = Hold;
    const NAME: &str = "web";
    const THREADS: [&str; 2] = ["web", "visit"];
    const FILE: &CStr = c"web";
    const DOWN: fn(&Hold) -> &NetworkDown = Hold::down;

    fn stay(&self, shared: &Shared<Self>, _: bool) -> Result<Hold, Fault> {
        shared.account.hold()
    }

    fn turn(&self, shared: &Shared<Self>, hold: &mut Hold, queries: Vec<Query>) {
        for query in queries {
            let outcome = self.visit(shared, query, hold.down());
            shared.tell(|state| state.take_visit(outcome));
        }
    }
}

impl Visitor {
    fn visit(&self, shared: &Shared<Self>, query: Query, down: &NetworkDown) -> Outcome {
        let subject = (Subject::WebRequest, PROMPT);
        let said = match shared.write_down(query, down, subject, "web: the request said is not written down", None) {
            Ok(said) => said,
            Err(fault) => return Outcome::Failed(fault),
        };
        let page = shared.board.lock().job.page().cloned();
        let (mut visit, mut slot) = (Visit::new(&said, page.as_ref()), None);
        loop {
            match visit.step() {
                Step::Fetch(target) => {
                    shared.set_phase(Phase::Searching);
                    let (start, fetched) = (Instant::now(), fetch(target, down).map_err(|e| e.to_string()));
                    if let Some(line) = visit.take_fetched(fetched, start.elapsed().as_millis()) {
                        log!("{line}");
                    }
                }
                Step::Ask(input) => {
                    shared.set_phase(Phase::Thinking);
                    let location = read_location(&self.data);
                    let reply = write::write_page(&shared.account, &mut slot, input, location.as_ref());
                    visit.take_reply(reply.inspect_err(|fault| log!("web: the model's step: {fault}")));
                }
                Step::Done(outcome) => return outcome,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::served::serve_answers;
    use crate::sys::Epoll;
    use crate::wifi::radio::tests::{up, without_networks};
    use r1_funcore::domain::cause::Cause;
    use r1_funcore::domain::voice::Bid;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn typed_request_no_key_fails() {
        let radio = without_networks();
        let account = Account::new(Data::unusable(), radio.online());
        let web = Web::start(account, Data::unusable()).unwrap();
        let mut epoll = Epoll::with_fd(web.fd());
        assert!(web.is_off());
        web.want(true);
        web.ask(vec![Bid::Text("example.org".into())]);
        let mut phase = Phase::Idle;
        while phase == Phase::Idle {
            assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7], "the screen is told");
            phase = web.latest().phase;
        }
        while !web.is_off() {
            thread::yield_now();
        }
        assert_eq!(
            web.latest().phase,
            Phase::Failed(Cause::NoAnswer),
            "the key cannot be read: the network is not held, and nothing is fetched"
        );
        web.want(false);
        assert!(web.is_off());
    }

    #[test]
    fn page_fetched_or_asker_sent_on() {
        let page = "<main><p>Words</p></main>";
        let answers = [
            "HTTP/1.1 301 Moved Permanently\r\nLocation: /there\r\nContent-Length: 0\r\n\r\n".to_string(),
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{page}", page.len()),
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 1\r\n\r\nx".to_string(),
        ];
        let (target, receiver) = serve_answers("/here", answers.into_iter().map(String::into_bytes).collect());
        let down = up();
        let Fetched::Redirect(next) = fetch(&target, &down).unwrap() else { panic!("sent on") };
        assert_eq!(next.path, "/there");
        assert_eq!(fetch(&next, &down).unwrap(), Fetched::Document(page.as_bytes().to_vec()));
        assert_eq!(
            fetch(&next, &down).map_err(|e| e.to_string()),
            Err("a document of the kind image/png".into()),
            "what is no page is not read"
        );
        let heads: Vec<String> = receiver.recv().unwrap().into_iter().map(|(_, head, _)| head).collect();
        assert!(
            heads[1].starts_with("GET /there HTTP/1.1\r\n") && heads[1].contains("\r\nAccept: text/html, "),
            "asked for as a page: {heads:?}"
        );
    }

    #[test]
    #[ignore]
    fn live_pages_read() {
        let down = up();
        for (address, heading) in
            [("en.wikipedia.org/wiki/Rabbit", "# Rabbit\n\n"), ("news.ycombinator.com", "# Hacker News\n\n")]
        {
            let mut visit = Visit::new(address, None);
            let page = loop {
                match visit.step() {
                    Step::Fetch(target) => {
                        let fetched = fetch(target, &down).map_err(|e| e.to_string());
                        visit.take_fetched(fetched, 0).iter().for_each(|line| println!("{line}"));
                    }
                    Step::Done(Outcome::Page(page)) => break page,
                    step => panic!("{address} is not read by the device: {step:?}"),
                }
            };
            println!("{}\n{}\n{} links, the first {:?}", page.address, page.text, page.links.len(), page.links.first());
            assert!(
                page.text.starts_with(heading) && page.text.len() > 1000 && page.links.len() > 20,
                "{address}: a page of words and links under its heading"
            );
        }
    }
}
