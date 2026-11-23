use crate::net::names;
use crate::net::web;
use crate::wifi::radio::NetworkDown;
use crate::{log, pool};
use r1_funcore::codec::http::Target;
use r1_funcore::domain::news::document::to_outcome;
use r1_funcore::domain::news::feed::Feed;
use r1_funcore::domain::news::fetch::{self, FeedResult, FetchOutcome, Rounds};
use r1_funcore::domain::news::stories::TopicStories;
use r1_funcore::domain::news::topics::Topic;
use r1_funcore::net::fetch::{Fetched, check_head};
use r1_funcore::net::tls::Purpose;
use std::io;
use std::net::IpAddr;
use std::time::Instant;

fn fetch(
    feed: Feed<'_>,
    target: &Target,
    addresses: &[IpAddr],
    down: &NetworkDown,
    follows_pages: bool,
) -> io::Result<FetchOutcome> {
    let start = Instant::now();
    let stream = web::connect(target, addresses, Purpose::Http, start, down)?;
    match web::fetch(stream, target, &fetch::HEADERS, start, |head| check_head(head, target))? {
        Fetched::Redirect(target) => Ok(FetchOutcome::Redirect(target)),
        Fetched::Document(document) => to_outcome(&document, feed, target, follows_pages).map_err(io::Error::other),
    }
}

// The feeds in turns of as many as funcore has read at once, so that no list, a model's among them, sets how many threads and connections stand together.
fn fetch_all(feeds: &[Feed<'_>], down: &NetworkDown, follows_pages: bool) -> Vec<FeedResult<io::Error>> {
    feeds.chunks(fetch::most_at_once()).flat_map(|feeds| fetch_turn(feeds, down, follows_pages)).collect()
}

fn fetch_turn(feeds: &[Feed<'_>], down: &NetworkDown, follows_pages: bool) -> Vec<FeedResult<io::Error>> {
    let mut rounds = Rounds::new(feeds);
    while let Some(round) = rounds.next_round() {
        let hosts: Vec<&str> = round.iter().map(|(_, target)| target.host.as_str()).collect();
        let resolved = names::resolve_all(&hosts, down);
        let fetches = round.iter().zip(resolved).map(|((index, target), addresses)| {
            let feed = feeds[*index];
            move || fetch(feed, target, &addresses?, down, follows_pages)
        });
        let lost_thread = || Err(io::Error::other("the thread that asked it died"));
        let outcomes: Vec<io::Result<FetchOutcome>> = pool::run_all("feed", fetches)
            .into_iter()
            .map(|result| result.and_then(|outcome| outcome.unwrap_or_else(lost_thread)))
            .collect();
        for ((index, _), outcome) in round.into_iter().zip(outcomes) {
            rounds.add_outcome(index, outcome);
        }
    }
    rounds.finish(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))
}

// The stories of each rundown's feeds, in the rundowns' order.
pub(super) fn gather(topics: &[Topic], down: &NetworkDown) -> Vec<TopicStories> {
    let start = Instant::now();
    let pairs = fetch::to_feeds(topics);
    let feeds: Vec<Feed<'_>> = pairs.iter().map(|(_, feed)| *feed).collect();
    let results = fetch_all(&feeds, down, false);
    let (stories, lines) = fetch::to_topic_stories(topics, &pairs, results, start.elapsed().as_millis());
    lines.iter().for_each(|line| log!("{line}"));
    stories
}

// Reads each feed line a request asks to add, a page that names a feed followed to it: what `fetch::to_found` tells of each.
pub(super) fn check_feeds(lines: &[String], down: &NetworkDown) -> Vec<Option<String>> {
    let feeds: Vec<Feed<'_>> = lines.iter().map(|line| Feed::parse(line)).collect();
    let (found, left_out) = fetch::to_found(lines, fetch_all(&feeds, down, true));
    left_out.iter().for_each(|line| log!("{line}"));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::served::{Request, serve_answers};
    use crate::sys::klog;
    use crate::wifi::radio::tests::up;
    use r1_funcore::codec::http::{READ_BYTES, USER_AGENT};
    use r1_funcore::domain::news::fetch::FeedError;
    use r1_funcore::domain::news::topics::Topics;
    use r1_funcore::net::fetch::{Fault, MAX_BYTES, MAX_HOPS};
    use std::error::Error;
    use std::sync::mpsc::Receiver;

    const FEED: &str = "<rss><item><title>One</title></item></rss>";
    const MOVED: &str = "HTTP/1.1 301 Moved Permanently\r\nLocation: /feed/\r\nContent-Length: 0\r\n\r\n";
    const GONE: &str = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";

    fn server(answers: Vec<String>) -> (String, Receiver<Vec<Request>>) {
        let (target, receiver) = serve_answers("", answers.into_iter().map(String::into_bytes).collect());
        (format!("https://{}:{}", target.host, target.port), receiver)
    }

    fn ok_answer(document: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{document}", document.len())
    }

    fn fetch_one(address: &str, down: &NetworkDown) -> FeedResult<io::Error> {
        fetch_all(&[Feed::parse(address)], down, false).pop().unwrap()
    }

    #[test]
    fn feed_found_on_its_page() {
        let page = r#"<html><head><link rel="alternate" type="application/rss+xml" href="/feed.xml"></head></html>"#;
        let (site, receiver) = server(vec![ok_answer(page), ok_answer(FEED)]);
        let (direct, _feed) = server(vec![ok_answer(FEED)]);
        let (bare, _page) = server(vec![ok_answer("<html><body>Words</body></html>")]);
        let lines = [format!("{site}/f1"), format!("motor {direct}/rss"), format!("{bare}/"), "no address".to_string()];
        assert_eq!(
            check_feeds(&lines, &up()),
            [Some(format!("{site}/feed.xml")), Some(lines[1].clone()), None, None],
            "the feed a page names in the page's place, a feed that answers as its line has it, and none for a page that names no feed or a line that is no address"
        );
        let heads: Vec<String> = receiver.recv().unwrap().into_iter().map(|(_, head, _)| head).collect();
        assert!(heads[0].starts_with("GET /f1 ") && heads[1].starts_with("GET /feed.xml "), "{heads:?}");
        let (same, _page) = server(vec![ok_answer(page)]);
        assert!(
            fetch_one(&format!("{same}/feed.xml"), &up()).stories.is_err(),
            "the day's gathering reads a feed's own address, and follows no page"
        );
    }

    #[test]
    fn feed_follows_redirects() {
        let (address, receiver) = server(vec![MOVED.into(), ok_answer(FEED)]);
        let result = fetch_one(&format!("{address}/rss"), &up());
        let titles = result.stories.map(|stories| stories.into_iter().map(|story| story.title).collect::<Vec<_>>());
        assert_eq!((titles.ok(), result.bytes, result.hops), (Some(vec!["One".to_string()]), FEED.len(), 1));
        let heads: Vec<String> = receiver.recv().unwrap().into_iter().map(|(_, head, _)| head).collect();
        assert!(
            heads[0].starts_with("GET /rss HTTP/1.1\r\n") && heads[1].starts_with("GET /feed/ HTTP/1.1\r\n"),
            "{heads:?}"
        );
        assert!(
            heads[1].contains(&format!("\r\nUser-Agent: {USER_AGENT}\r\n"))
                && heads[1].contains("\r\nAccept-Encoding: identity\r\n")
                && heads[1].contains("\r\nAccept: application/rss+xml, "),
            "under the firmware's own name, for a feed, not packed: {heads:?}"
        );
        let (address, receiver) = server(vec![MOVED.into(); MAX_HOPS + 1]);
        let result = fetch_one(&address, &up());
        assert_eq!(
            (result.stories.map_err(|e| e.kind()).err(), result.hops),
            (Some(io::ErrorKind::InvalidData), MAX_HOPS + 1),
            "a feed that sends the asker round and round is given up"
        );
        assert_eq!(receiver.recv().unwrap().len(), MAX_HOPS + 1);
    }

    #[test]
    fn feeds_read_at_once_in_order() {
        let (direct, first) = server(vec![ok_answer(FEED)]);
        let (redirected, second) = server(vec![MOVED.into(), ok_answer(&FEED.replace("One", "Two"))]);
        let (missing, third) = server(vec![GONE.into()]);
        let feeds = [missing.as_str(), "no address at all", redirected.as_str(), direct.as_str()].map(Feed::parse);
        let results = fetch_all(&feeds, &up(), false);
        let rows: Vec<_> = results
            .iter()
            .map(|result| (result.stories.as_ref().ok().map(|stories| stories[0].title.as_str()), result.hops))
            .collect();
        assert_eq!(
            rows,
            [(None, 0), (None, 0), (Some("Two"), 1), (Some("One"), 0)],
            "one that was sent elsewhere is asked there in the round after, while the others have done"
        );
        assert_eq!((first.recv().unwrap().len(), second.recv().unwrap().len(), third.recv().unwrap().len()), (1, 2, 1));
        let (late, asked) = server(vec![ok_answer(FEED)]);
        let mut many = vec![Feed::parse("no address at all"); fetch::most_at_once()];
        many.push(Feed::parse(&late));
        let results = fetch_all(&many, &up(), false);
        assert_eq!(
            (
                results.len(),
                results.iter().filter(|result| result.stories.is_ok()).count(),
                asked.recv().unwrap().len()
            ),
            (many.len(), 1, 1),
            "more feeds than are read at once are read in turns, each result in its feed's place"
        );
        assert!(results[many.len() - 1].stories.is_ok(), "the last of them in the turn after");
    }

    #[test]
    fn non_feed_documents_refused() {
        let gzip = "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n\r\n\x1f\x08".to_string();
        let bad_redirect =
            "HTTP/1.1 302 Found\r\nLocation: mailto:nobody@example.org\r\nContent-Length: 0\r\n\r\n".to_string();
        let too_large = format!("HTTP/1.1 200 OK\r\n\r\n{}", "x".repeat(MAX_BYTES + READ_BYTES));
        let page = "<html><body>Just a moment...</body></html>";
        fn refusal<E: Error + 'static>(answer: String) -> Option<E> {
            let (address, receiver) = server(vec![answer]);
            let error = fetch_one(&address, &up()).stories.err().and_then(io::Error::into_inner);
            receiver.recv().unwrap();
            error.and_then(|error| error.downcast::<E>().ok()).map(|refusal| *refusal)
        }
        for (answer, fault) in [
            (GONE.to_string(), Fault::Status(404)),
            (gzip, Fault::Encoded("gzip".into())),
            (bad_redirect, Fault::BadRedirect),
            (too_large, Fault::TooLarge),
        ] {
            assert_eq!(refusal(answer), Some(fault), "a fetch that gave no document");
        }
        for (answer, error) in
            [(ok_answer(""), FeedError::NoStories(0)), (ok_answer(page), FeedError::NoStories(page.len()))]
        {
            assert_eq!(refusal(answer), Some(error), "a document that is no feed");
        }
        let no_address = fetch_one("feed.example.org/rss", &up()).stories.err();
        assert_eq!(no_address.as_ref().map(io::Error::kind), Some(io::ErrorKind::InvalidData));
        assert_eq!(
            no_address
                .and_then(io::Error::into_inner)
                .and_then(|error| error.downcast::<FeedError>().ok())
                .map(|fault| *fault),
            Some(FeedError::BadAddress),
            "an address that is no URL is asked nowhere"
        );
        let down = up();
        down.raise();
        assert!(fetch_one("https://192.0.2.1/rss", &down).stories.is_err(), "without the network nothing is asked");
        assert!(fetch_all(&[], &down, false).is_empty() && gather(&[], &down).is_empty(), "no feed, no reading");
    }

    #[test]
    #[ignore]
    fn live_feeds_answer() {
        let (down, start, kept) = (up(), Instant::now(), Topics::default());
        let pairs = fetch::to_feeds(kept.all());
        let feeds: Vec<Feed<'_>> = pairs.iter().map(|(_, feed)| *feed).collect();
        let results = fetch_all(&feeds, &down, false);
        println!("all feeds read {} ms after the start", start.elapsed().as_millis());
        let mut failures = Vec::new();
        for (place, topic) in kept.all().iter().enumerate() {
            let mut stories = TopicStories::default();
            for ((_, feed), result) in pairs.iter().zip(&results).filter(|((index, _), _)| *index == place) {
                match &result.stories {
                    Ok(feed_stories) => println!(
                        "{:8} {:22} {:7} bytes {:3} stories, sent elsewhere {} times",
                        topic.name,
                        feed.source(),
                        result.bytes,
                        feed_stories.len(),
                        result.hops
                    ),
                    Err(e) => {
                        println!("{:8} {:22} {e}", topic.name, feed.source());
                        failures.push(feed.source());
                    }
                }
                stories.add(feed, result.stories.as_ref().ok().cloned());
            }
            let report = stories.to_report().unwrap_or_default();
            println!(
                "{}: {} of {} feeds, {} stories, a report of {} bytes",
                topic.name,
                stories.answered_count,
                stories.feed_count,
                stories.stories.len(),
                report.len()
            );
        }
        klog::drain().iter().filter(|line| line.starts_with("names: dns")).for_each(|line| println!("{line}"));
        assert!(failures.is_empty(), "feeds that gave no document: {failures:?}");
    }
}
