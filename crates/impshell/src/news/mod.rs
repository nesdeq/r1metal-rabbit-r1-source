mod gather;
pub mod request;

use crate::openai::Account;
use crate::openai::write::{self, WorkPoller};
use crate::power::alarm::Alarm;
use crate::storage::Data;
use crate::sync::Board;
use crate::sys::{self, Epoll, Wake};
use crate::wifi::radio::{NetworkDown, RadioGrant};
use crate::{log, pool};
use r1_funcore::base::time::Moment;
use r1_funcore::base::time::offset::to_local;
use r1_funcore::domain::news::editor::{self, EditionResult, Outcome, Schedule, Step};
use r1_funcore::domain::news::rundowns::{self, Rundown, Rundowns, Seen};
use r1_funcore::domain::news::silent::{self, Silent};
use r1_funcore::domain::news::state::{EditionRequest, Report, State};
use r1_funcore::domain::news::stories::{TopicStories, sources_line};
use r1_funcore::domain::news::topics::{Topic, Topics};
use r1_funcore::domain::news::{self, Edition, NO_PARTITION_LINE};
use r1_funcore::domain::paper::names;
use r1_funcore::domain::paper::pages::to_pages;
use r1_funcore::domain::paper::sheet::{self, Subject};
use r1_funcore::gfx::palette::Roles;
use r1_funcore::hw::power::alarm::Reason;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::summary::{Poll, Summary, WorkStatus};
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::time::Instant;

const ALARM: u64 = 0;
const NUDGE: u64 = 1;

struct Shared {
    board: Board<State>,
    nudge: Wake,
    data: Data,
    account: Account,
}

// A rundown's name, and what asking for it came to.
type TopicOutcome = (String, Result<Outcome, Fault>);

fn add_outcomes(result: &mut EditionResult, outcomes: Vec<TopicOutcome>) {
    for (topic, outcome) in outcomes {
        if let Err(fault) = &outcome {
            log!("{}", rundowns::to_unwritten_line(&topic, fault));
        }
        result.add(&outcome);
    }
}

fn arm(alarm: &Alarm, (offset, edition): (i16, Edition), look: Option<u64>) -> Option<(Option<u64>, Option<u64>)> {
    loop {
        let (due, at) = edition.next_alarm(sys::realtime_seconds(), offset, look);
        match alarm.arm(at) {
            Ok(true) => return Some((due, at)),
            // The clocks were set meanwhile: the time is worked out anew.
            Ok(false) => {}
            Err(e) => {
                log!("news: the alarm for the next edition is not set: {e}");
                return None;
            }
        }
    }
}

impl Shared {
    fn read_pending<R>(&self, read: impl FnMut(&Rundowns) -> R) -> R {
        self.board.read_loan(|state| &state.pending, read)
    }

    fn change_pending<R>(&self, change: impl FnOnce(&mut Rundowns) -> R) -> R {
        self.board.change_loan(|state| &mut state.pending, change)
    }

    fn topics(&self) -> Topics {
        self.board.lock().topics()
    }

    fn missing(&self, now: u64, edition: Edition, is_forced: bool) -> io::Result<Vec<Topic>> {
        let topics = self.topics();
        self.change_pending(|pending| pending.to_missing(&self.data, now, (edition, is_forced), &topics))
    }

    fn resume(&self) {
        match self.data.text(news::STATE_DIR, rundowns::FILE) {
            Ok(Some(text)) => {
                let rundowns = Rundowns::parse(&text);
                if let Some(line) = rundowns.to_resumed_line() {
                    log!("{line}");
                }
                self.change_pending(|pending| *pending = rundowns);
            }
            Ok(None) => {}
            Err(e) => log!("news: the rundowns that are on their way cannot be read: {e}"),
        }
        match self.data.text(news::STATE_DIR, silent::FILE) {
            Ok(Some(text)) => {
                self.board.lock().set_silent(Silent::parse(&text));
                self.board.raise();
            }
            Ok(None) => {}
            Err(e) => log!("news: the feeds that gave no story cannot be read: {e}"),
        }
    }

    // Keeps the feeds a gathering found silent, where that is news, and tells the screens.
    fn note_silent(&self, topics: &[Topic], stories: &[TopicStories]) {
        let found = topics.iter().zip(stories).map(|(topic, stories)| (topic.name.as_str(), &stories.silent[..]));
        let Some(text) = self.board.lock().note_silent(found) else {
            return;
        };
        self.board.raise();
        if let Err(e) = self.data.put(news::STATE_DIR, silent::FILE, text.as_bytes()) {
            log!("news: the feeds that gave no story cannot be kept: {e}");
        }
    }

    fn add_pending(&self, rundown: Rundown) {
        self.change_pending(|pending| {
            pending.add(rundown);
            if let Err(e) = pending.save(&self.data) {
                log!("news: the rundowns that are on their way cannot be kept: {e}");
            }
        });
    }

    // True where the tried file took the note of a try, which is made only then.
    fn note(&self, note: impl FnOnce(Option<&str>) -> Option<String>) -> bool {
        match self.data.text(news::STATE_DIR, editor::TRIED_FILE) {
            Ok(text) => match note(text.as_deref()) {
                Some(noted) => {
                    let result = self.data.put(news::STATE_DIR, editor::TRIED_FILE, noted.as_bytes());
                    result.inspect_err(|e| log!("news: the day's try cannot be noted, and is not made: {e}")).is_ok()
                }
                None => false,
            },
            Err(e) => {
                log!("news: whether the day's edition was tried cannot be read, and it is not: {e}");
                false
            }
        }
    }

    fn begin(&self) {
        self.board.lock().begin();
        self.board.raise();
        self.board.wait(|state| state.has_radio().then_some(()));
    }

    fn set_stage(&self, gathering_day: Option<&str>, writing_day: Option<&str>) {
        let mut state = self.board.lock();
        (state.gathering_day, state.writing_day) = (gathering_day.map(String::from), writing_day.map(String::from));
        drop(state);
        self.board.raise();
    }

    fn note_written(&self) {
        self.board.lock().report.has_changed = true;
        self.board.raise();
    }

    fn end(&self, result: &EditionResult, writing_day: Option<String>) {
        self.board.lock().end(result, writing_day);
        self.board.raise();
    }

    fn write_rundown(
        &self,
        day: &str,
        topic: &str,
        sources: &str,
        summary: &Summary,
        offset: i16,
    ) -> Result<(), Fault> {
        let subject = Subject::Rundown(day, topic, sources);
        let (place, created) = (names::edition_folder(day), Some((to_local(sys::realtime_seconds(), offset), offset)));
        let file = sheet::to_summary_file(subject, &summary.text, summary.usage.model);
        // The Markdown first, as soon as the answer is read: it is what was paid for, and what says the rundown is made.
        self.data.put(&place, &names::rundown_file(day, topic), file.as_bytes())?;
        self.note_written();
        let roles = self.board.lock().roles;
        let pages = to_pages(subject, &summary.text, created, &roles);
        let result = self.data.put(&place, &names::rundown_pages_file(day, topic), &pages);
        if let Err(e) = result {
            log!("news: the day's {topic} is kept as Markdown, its pages are not: {e}");
        }
        Ok(())
    }

    fn make_rundown(
        &self,
        topic: &Topic,
        now: u64,
        offset: i16,
        stories: &TopicStories,
        down: &NetworkDown,
    ) -> Result<Outcome, Fault> {
        let report = stories.to_report()?;
        let today = Moment::from_seconds(now);
        let (day, sources, name) = (today.to_compact_date(), sources_line(&report), topic.name.as_str());
        let start = Instant::now();
        let asked = (name, topic.task());
        let work_status = write::ask_rundown(&self.account, down, (asked.0, &asked.1), &today.to_iso_date(), &report)?;
        let summary = match work_status {
            WorkStatus::Done(summary) => summary,
            WorkStatus::Pending(work, _) => {
                // The work's name goes on the partition first: no end of the stay, or of the app, has it asked for twice.
                self.add_pending(Rundown::new(name, work, sys::realtime_seconds(), day, sources));
                log!("{}", rundowns::to_asked_line(name));
                return Ok(Outcome::Pending);
            }
        };
        write::log_rundown(name, &summary, start.elapsed());
        self.write_rundown(&day, name, sources, &summary, offset)?;
        Ok(Outcome::Kept)
    }

    fn collect_pending(&self, offset: i16) {
        let now = sys::realtime_seconds();
        if !self.read_pending(|pending| pending.is_look_due(now)) {
            return;
        }
        self.begin();
        let (start, mut result, mut remaining) = (Instant::now(), EditionResult::default(), Rundowns::default());
        let mut overdue = Vec::new();
        // The key is looked for before the network is held for the looks.
        let poller = WorkPoller::new(&self.account).and_then(|poller| Ok((poller, self.account.hold()?)));
        let entries: Vec<Rundown> = self.read_pending(|pending| pending.entries.clone());
        for entry in entries {
            let name = entry.topic.as_str();
            let poll = match &poller {
                Ok((poller, hold)) => poller.poll(&entry.name, hold.down()),
                Err(fault) => Poll::from_fault(fault),
            };
            let now = sys::realtime_seconds();
            match remaining.take_look(entry.clone(), poll, now) {
                Seen::Written(summary) => {
                    write::log_rundown(name, &summary, entry.elapsed(now));
                    match self.write_rundown(&entry.day, name, &entry.sources, &summary, offset) {
                        Ok(()) => result.add(&Ok(Outcome::Kept)),
                        Err(fault) => {
                            log!("news: the day's {name} is not kept: {fault}");
                            result.add_fault(&fault);
                        }
                    }
                }
                Seen::Waiting(line) => log!("{line}"),
                Seen::Ended(fault) => {
                    log!("{}", rundowns::to_unwritten_line(name, &fault));
                    result.add_fault(&fault);
                }
                Seen::Overdue(fault) => overdue.push((entry, fault)),
            }
        }
        if let Err(e) = remaining.save(&self.data) {
            log!("news: the rundowns that are on their way cannot be kept: {e}");
        }
        self.change_pending(|pending| *pending = remaining);
        // An overdue rundown's work is cancelled, so that it is not paid for beside the one asked for anew in its place: one the owner keeps and has made still.
        let now = to_local(sys::realtime_seconds(), offset);
        let (day, mut anew, topics) = (Moment::from_seconds(now).to_compact_date(), Vec::new(), self.topics());
        for (entry, fault) in overdue {
            let name = entry.topic.as_str();
            if let Ok((poller, hold)) = &poller
                && let Poll::Retry(e) = poller.cancel(&entry.name, hold.down())
            {
                log!("news: the day's {name} is not cancelled at the service: {e}");
            }
            let topic = rundowns::to_anew(&topics, &entry, poller.is_ok());
            match topic.filter(|_| self.note(|text| editor::note_anew(text, &day, name))) {
                Some(topic) => {
                    log!("{}", rundowns::to_anew_line(name, &fault));
                    anew.push(topic.clone());
                }
                None => {
                    log!("{}", rundowns::to_unwritten_line(name, &fault));
                    result.add_fault(&fault);
                }
            }
        }
        if let (Ok((_, hold)), false) = (&poller, anew.is_empty()) {
            add_outcomes(&mut result, self.ask(&anew, (now, offset), hold.down()));
        }
        drop(poller);
        let (count, writing_day) =
            self.read_pending(|pending| (pending.entries.len(), pending.first_day().map(String::from)));
        log!("{}", result.to_looked_line(count, start.elapsed().as_millis()));
        self.end(&result, writing_day);
    }

    fn ask(&self, topics: &[Topic], (now, offset): (u64, i16), down: &NetworkDown) -> Vec<TopicOutcome> {
        let day = Moment::from_seconds(now).to_compact_date();
        self.set_stage(Some(&day), None);
        let stories = gather::gather(topics, down);
        self.note_silent(topics, &stories);
        self.set_stage(None, Some(&day));
        let jobs = topics
            .iter()
            .zip(&stories)
            .map(|(topic, stories)| move || self.make_rundown(topic, now, offset, stories, down));
        let lost_thread = || Err(Fault::Temporary("the thread that had it written died".into()));
        topics
            .iter()
            .zip(pool::run_all("rundown", jobs))
            .map(|(topic, result)| {
                let outcome = result.map_err(Fault::from).and_then(|outcome| outcome.unwrap_or_else(lost_thread));
                (topic.name.clone(), outcome)
            })
            .collect()
    }

    fn make_rundowns(&self, request: EditionRequest) -> Result<Vec<TopicOutcome>, Fault> {
        let offset = request.offset;
        // The network is held while the feeds are read and the model is asked, and let go of then.
        let hold = self.account.hold()?;
        // The day is read off the clock once the network is held: in the first stay since the start the clocks are set before whoever holds it goes on.
        let now = to_local(sys::realtime_seconds(), offset);
        let missing = self.missing(now, request.edition, request.is_forced)?;
        if missing.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.ask(&missing, (now, offset), hold.down()))
    }

    fn run_edition(&self, request: EditionRequest) {
        let now = to_local(sys::realtime_seconds(), request.offset);
        // An edition that is whole, or whose hour has not come, has nothing asked of the radio; nor has one whose lacking rundowns the model is at work on.
        if self.missing(now, request.edition, request.is_forced).is_ok_and(|missing| missing.is_empty()) {
            return;
        }
        let day = Moment::from_seconds(now).to_compact_date();
        if request.is_noted() && !self.note(|text| editor::note_try(text, &day)) {
            return;
        }
        self.begin();
        let (start, mut result) = (Instant::now(), EditionResult::default());
        match self.make_rundowns(request) {
            Ok(outcomes) => add_outcomes(&mut result, outcomes),
            Err(fault) => {
                log!("news: no edition is made: {fault}");
                result.add_fault(&fault);
            }
        }
        log!("{}", result.to_made_line(start.elapsed().as_millis()));
        let writing_day = self.read_pending(|pending| pending.first_day().map(String::from));
        self.end(&result, writing_day);
    }
}

fn work(shared: &Shared, alarm: &Alarm, epoll: &mut Epoll) {
    // The partition is waited for however late it appears; without one there is nowhere to keep an edition, and none is made.
    let has_partition = shared.data.wait_decided();
    if has_partition {
        // Rundowns the model was at work on when the app last ran are looked at as the clock says, and asked for no more.
        shared.resume();
    } else {
        log!("{NO_PARTITION_LINE}");
    }
    // An edition that was due while the device was off is made when it starts.
    let mut schedule = Schedule::default();
    loop {
        let request = shared.board.lock().take_request();
        schedule.begin_pass(request.filter(|_| has_partition), sys::realtime_seconds());
        loop {
            match schedule.next_step() {
                Step::Try(request) => shared.run_edition(request),
                Step::Look(request) => shared.collect_pending(request.offset),
                Step::Arm(request) => {
                    let look = shared.read_pending(Rundowns::next_look_s);
                    let next = arm(alarm, (request.offset, request.edition), look).and_then(|(due, _)| due);
                    if let Some(due) = schedule.set_due(next) {
                        log!("{}", news::to_due_line(due, request.offset, alarm.can_wake));
                    }
                }
                Step::Wait => break,
            }
        }
        if let Err(e) = epoll.wait(None) {
            return log!("news: waiting: {e}");
        }
        alarm.acknowledge();
        shared.nudge.clear();
    }
}

/// The main loop's handle of the thread that makes each day's edition.
pub struct Editor {
    shared: Arc<Shared>,
}

impl Editor {
    /// Starts the thread and its alarm, with editions on `data` and the model services through `account`.
    pub fn start(data: Data, account: Account) -> io::Result<Editor> {
        let alarm = Alarm::new(Reason::DailyNews)?;
        let shared = Arc::new(Shared { board: Board::new(State::default())?, nudge: Wake::new()?, data, account });
        let mut epoll = Epoll::new()?;
        epoll.add_wakeup(alarm.fd(), ALARM)?;
        epoll.add(shared.nudge.fd(), NUDGE)?;
        let worker = Arc::clone(&shared);
        sys::spawn_background("news", move || {
            work(&worker, &alarm, &mut epoll);
            worker.board.lock().end_thread();
            worker.board.raise();
        })?;
        Ok(Editor { shared })
    }

    /// Readable when the making of an edition begins, moves on or ends.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Sets the screen clock's lead on UTC in minutes and the edition the owner set, which set an edition's hour; none is made before they are known.
    pub fn set_schedule(&self, schedule: Option<(i16, Edition)>) {
        if self.shared.board.lock().set_schedule(schedule) {
            self.shared.nudge.raise();
        }
    }

    /// Sets the rundowns the owner keeps, which an edition makes; none is made before they are known.
    pub fn set_topics(&self, topics: Topics) {
        if self.shared.board.lock().set_topics(topics) {
            self.shared.nudge.raise();
        }
    }

    /// Sets the colours of an edition's pages.
    pub fn set_roles(&self, roles: Roles) {
        self.shared.board.lock().roles = roles;
    }

    /// Sets whether a news screen shows; one that opens retries an edition that is due and lacks something.
    pub fn want(&self, is_wanted: bool) {
        if self.shared.board.lock().set_wanted(is_wanted) {
            self.shared.nudge.raise();
        }
    }

    /// Has the day's edition made anew, whatever the hour and the files: the console's order.
    pub fn make_edition(&self) {
        if self.shared.board.lock().force() {
            self.shared.nudge.raise();
        }
    }

    /// True while there is no edition to make, so neither a sleep nor the radio's rest waits for one.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }

    /// Hands the thread the radio's grant for the edition it has to make.
    pub fn grant_radio(&self, _: &RadioGrant) {
        if self.shared.board.lock().grant_radio() {
            self.shared.board.notify();
        }
    }

    /// Takes the report for the main loop.
    pub fn latest(&self) -> Report {
        self.shared.board.take(State::take_report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::radio::Radio;
    use crate::wifi::radio::tests::without_networks;
    use r1_funcore::base::time::SECONDS_PER_DAY;
    use r1_funcore::domain::cause::Cause;
    use r1_funcore::domain::news::rundowns::FIRST_LOOK_S;
    use std::thread;
    use std::time::Duration;

    fn editor() -> (Editor, Radio, Epoll) {
        let radio = without_networks();
        let editor = Editor::start(Data::unusable(), Account::new(Data::unusable(), radio.online())).unwrap();
        let epoll = Epoll::with_fd(editor.fd());
        (editor, radio, epoll)
    }

    fn next_report(editor: &Editor, epoll: &mut Epoll, grant: &RadioGrant) -> Report {
        assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7], "the thread tells of what it does");
        let report = editor.latest();
        editor.grant_radio(grant);
        report
    }

    #[test]
    fn edition_waits_for_radio_grant() {
        let (editor, radio, mut epoll) = editor();
        let grant = radio.set_resting(false).expect("awake the radio does not rest");
        assert!(editor.is_off() && editor.latest() == Report::default());
        editor.grant_radio(&grant);
        assert!(!editor.shared.board.lock().has_radio(), "leave nobody asked for is none");
        let shared = Arc::clone(&editor.shared);
        let thread = thread::spawn(move || {
            shared.begin();
            shared.set_stage(Some("20261013"), None);
            shared.set_stage(None, Some("20261013"));
            shared.note_written();
            shared.end(&EditionResult { kept_count: 1, ..EditionResult::default() }, None);
        });
        assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![7], "it tells before it does anything");
        assert!(!editor.is_off(), "from then on the device does not sleep, and the radio does not rest");
        assert_eq!(editor.latest(), Report::default(), "nothing is gathered before the leave is given");
        assert!(!thread.is_finished());
        editor.grant_radio(&grant);
        thread.join().unwrap();
        assert_eq!(
            (editor.is_off(), editor.latest(), editor.latest()),
            (true, Report { has_changed: true, kept_count: 1, ..Report::default() }, Report::default()),
            "made: a file was written and a rundown kept, told once, and the device may sleep again"
        );
        assert!(!editor.shared.board.lock().has_radio(), "the leave was for that one making");
    }

    fn request(is_forced: bool, is_asked: bool) -> EditionRequest {
        EditionRequest { offset: 120, edition: Edition::default(), is_forced, is_asked }
    }

    fn first_edition() -> u64 {
        Edition::default().next_after(0).unwrap()
    }

    #[test]
    fn no_partition_no_edition() {
        let (editor, radio, mut epoll) = editor();
        let grant = radio.set_resting(false).expect("awake the radio does not rest");
        editor.set_schedule(Some((120, Edition::default())));
        editor.set_schedule(None);
        editor.want(true);
        editor.want(true);
        editor.make_edition();
        assert_eq!(epoll.wait(Some(Duration::from_millis(50))).unwrap(), Vec::<u64>::new(), "its thread makes nothing");
        assert!(editor.is_off());
        let (hour, edition) = (first_edition(), Edition::default());
        assert!(
            editor.shared.missing(hour, edition, false).is_err(),
            "what a day lacks is not known without the partition"
        );
        assert_eq!(
            editor.shared.missing(hour, edition, true).ok(),
            Some(Topics::default().all().to_vec()),
            "an edition made anew lacks every rundown, the two a new device has while none are told"
        );
        editor.set_topics(Topics::parse("rundown news off\nrundown tech on\nfeed tech https://a.example/rss\n"));
        assert_eq!(
            editor.shared.missing(hour, edition, true).ok().map(|missing| missing[0].name.clone()),
            Some("tech".into()),
            "and of those the owner keeps the ones that are made"
        );
        assert!(!editor.shared.note(|text| editor::note_try(text, "20261013")), "nor whether the day was tried");
        editor.shared.run_edition(request(true, false));
        assert!(
            editor.is_off() && epoll.wait(Some(Duration::ZERO)).unwrap().is_empty(),
            "by itself the thread begins no try that it cannot note: nothing is asked, and nobody is told"
        );
        let shared = Arc::clone(&editor.shared);
        let thread = thread::spawn(move || shared.run_edition(request(true, true)));
        let mut cause = None;
        while cause.is_none() {
            cause = next_report(&editor, &mut epoll, &grant).cause;
        }
        thread.join().unwrap();
        assert_eq!(
            cause,
            Some(Cause::NotWritten),
            "the key cannot be read: nothing is asked of the network or of a model"
        );
        assert!(editor.is_off() && editor.latest() == Report::default());
    }

    #[test]
    fn alarm_set_to_edition_hour() {
        let alarm = Alarm::new(Reason::DailyNews).unwrap();
        let evening = Edition { hour: Some(19) };
        for offset in [0, 120, -210, 840, -660] {
            let start = sys::realtime_seconds();
            let (armed, end) = (arm(&alarm, (offset, evening), None).unwrap(), sys::realtime_seconds());
            let (Some(due), at) = armed else { panic!("an edition is due") };
            let moment = Moment::from_seconds(to_local(due, offset));
            assert_eq!((moment.hour, moment.minute, moment.second), (19, 0, 0), "at {offset} minutes from UTC");
            assert!(due > start && due <= end + 24 * 3600, "the next one, within a day: {due} at {start}");
            assert_eq!(at, Some(due), "with no rundown on its way the alarm is set to the edition's hour");
        }
        let off = Edition { hour: None };
        assert_eq!(arm(&alarm, (0, off), None), Some((None, None)), "with no daily edition the alarm is cleared");
    }

    #[test]
    fn alarm_set_to_earlier_look() {
        let alarm = Alarm::new(Reason::DailyNews).unwrap();
        let schedule = (0, Edition::default());
        let (Some(due), _) = arm(&alarm, schedule, None).unwrap() else { panic!("an edition is due") };
        let look = (sys::realtime_seconds() + FIRST_LOOK_S).min(due - 1);
        assert_eq!(
            arm(&alarm, schedule, Some(look)),
            Some((Some(due), Some(look))),
            "a look in two minutes, or just before the hour where that comes sooner: the edition's hour as it was"
        );
        assert_eq!(
            arm(&alarm, schedule, Some(due + 60)),
            Some((Some(due), Some(due))),
            "a look after the hour waits for the next stay: the hour is first"
        );
        assert_eq!(arm(&alarm, schedule, Some(0)), Some((Some(due), Some(0))), "a look that is overdue is due at once");
    }

    #[test]
    fn pending_rundowns_not_asked_twice() {
        let (editor, _radio, mut epoll) = editor();
        let (now, edition) = (to_local(sys::realtime_seconds(), 120), Edition::default());
        // The edition's hour of today by the clock on screen, by which what the partition holds is looked at.
        let hour = edition.next_after(now - now % SECONDS_PER_DAY).unwrap();
        let day = Moment::from_seconds(hour).to_compact_date();
        let rundown = |topic: &str, name: &str| {
            let sources = "Sources (21/21 feeds, 681 stories): a.com";
            Rundown::new(topic, name.into(), sys::realtime_seconds(), day.clone(), sources)
        };
        let shared = &editor.shared;
        let names = |missing: io::Result<Vec<Topic>>| {
            missing.ok().map(|missing| missing.into_iter().map(|topic| topic.name).collect())
        };
        assert_eq!(
            names(shared.missing(hour, edition, true)),
            Some(vec!["news".to_string(), "finance".to_string()]),
            "with nothing on its way an edition made anew lacks both"
        );
        shared.change_pending(|pending| pending.add(rundown("news", "resp_1")));
        assert_eq!(
            names(shared.missing(hour, edition, true)),
            Some(vec!["finance".to_string()]),
            "the news are on their way: they are waited for, whatever asks, and not paid for twice"
        );
        assert!(shared.missing(hour, edition, false).is_err(), "what the other lacks is still read off the partition");
        assert!(
            shared.missing(first_edition(), edition, true).is_ok_and(|missing| missing.len() == 2),
            "another day's rundown is another"
        );
        // What the partition gave back after a restart.
        let text = Rundowns { entries: vec![rundown("news", "resp_1"), rundown("finance", "resp_2")] }.to_text();
        shared.change_pending(|pending| *pending = Rundowns::parse(&text));
        shared.read_pending(|pending| assert_eq!(pending.first_day(), Some(day.as_str())));
        assert_eq!(shared.missing(hour, edition, true).ok(), Some(Vec::new()), "both are on their way");
        assert_eq!(
            shared.missing(hour, edition, false).ok(),
            Some(Vec::new()),
            "and none is looked for on the partition"
        );
        shared.run_edition(request(true, true));
        assert!(
            editor.is_off() && epoll.wait(Some(Duration::ZERO)).unwrap().is_empty(),
            "asked for by hand, an edition whose rundowns are all on their way begins nothing: the radio stays off and nobody is told"
        );
    }

    #[test]
    fn look_made_only_when_due() {
        let (editor, _radio, mut epoll) = editor();
        let shared = &editor.shared;
        shared.collect_pending(120);
        assert!(
            editor.is_off() && epoll.wait(Some(Duration::ZERO)).unwrap().is_empty(),
            "nothing on its way, nothing to look at"
        );
        let now = sys::realtime_seconds();
        let day = Moment::from_seconds(to_local(now, 120)).to_compact_date();
        shared.add_pending(Rundown::new("news", "resp_1".into(), now, day, "Sources"));
        assert_eq!(
            shared.read_pending(|pending| pending.entries.len()),
            1,
            "without a partition it is on its way in memory"
        );
        shared.collect_pending(120);
        assert!(
            editor.is_off() && epoll.wait(Some(Duration::ZERO)).unwrap().is_empty(),
            "a rundown asked for just now is looked at in two minutes, not before"
        );
        shared.read_pending(|pending| {
            assert_eq!(pending.next_look_s(), Some(pending.entries[0].at_s + FIRST_LOOK_S));
        });
    }

    #[test]
    fn overdue_rundown_ends_without_key() {
        let (editor, radio, mut epoll) = editor();
        let grant = radio.set_resting(false).expect("awake the radio does not rest");
        let asked = sys::realtime_seconds() - 3600;
        let day = Moment::from_seconds(to_local(asked, 120)).to_compact_date();
        editor.shared.add_pending(Rundown::new("news", "resp_1".into(), asked, day, "Sources"));
        let shared = Arc::clone(&editor.shared);
        let thread = thread::spawn(move || shared.collect_pending(120));
        let mut cause = None;
        while cause.is_none() {
            cause = next_report(&editor, &mut epoll, &grant).cause;
        }
        thread.join().unwrap();
        assert_eq!(
            cause,
            Some(Cause::NotWritten),
            "the key cannot be read: its work is not cancelled, and nothing is asked for anew"
        );
        assert!(
            editor.is_off() && editor.shared.read_pending(|pending| pending.entries.is_empty()),
            "and it is on its way no more"
        );
    }
}
