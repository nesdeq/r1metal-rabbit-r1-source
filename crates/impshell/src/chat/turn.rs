use super::{Robot, files, read};
use crate::openai::chat::Conversation;
use crate::voiced::{ChoreRunner, ReadyCheck, Shared, Worker};
use crate::wifi::radio::{Hold, NetworkDown};
use crate::{clock, log, sys, timekeeper};
use r1_funcore::base::text::has_words;
use r1_funcore::base::time::Moment;
use r1_funcore::domain::chat::calls::{self, Step};
use r1_funcore::domain::chat::instructions::{self, Instructions};
use r1_funcore::domain::chat::reply::{Reply, Update};
use r1_funcore::domain::chat::state::{Input, Milestone, Store, Turns, to_kind};
use r1_funcore::domain::chat::thread::{RobotState, Shown, Thread};
use r1_funcore::domain::chat::tools::{self, Asked, MemoryCall, UNKNOWN};
use r1_funcore::domain::chat::transcript::{Entry, EntryKind};
use r1_funcore::domain::chat::{InputKind, MET_LINE, ONBOARDING_ANSWER, message, recall};
use r1_funcore::domain::settings::{Settings, Tool};
use r1_funcore::domain::signal::robot::Reply as Answered;
use r1_funcore::domain::voice::{Phase, Query};
use r1_funcore::openai::chat::answer::{Answer, Call};
use r1_funcore::openai::chat::request::{Location, build_tools, function_output, user_message};
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::transcript::Subject;
use std::ffi::CStr;

impl Worker for Robot {
    type State = Turns;
    type Stay = (Conversation, Hold);
    const NAME: &str = "chat";
    const THREADS: [&str; 2] = ["chat", "turns"];
    const FILE: &CStr = c"chat";
    const READY_CHECK: Option<ReadyCheck<Robot>> = Some(Robot::check_ready);
    const CHORE: Option<ChoreRunner<Robot>> = Some(Robot::run_chore);
    const DOWN: fn(&(Conversation, Hold)) -> &NetworkDown = |(_, hold)| hold.down();

    fn stay(&self, shared: &Shared<Robot>, _: bool) -> Result<(Conversation, Hold), Fault> {
        let hold = shared.account.hold()?;
        let mut conversation = Conversation::new(&shared.account, hold.down());
        if shared.board.lock().should_connect_ahead() {
            conversation.prepare();
        }
        Ok((conversation, hold))
    }

    fn turn(&self, shared: &Shared<Robot>, (conversation, hold): &mut (Conversation, Hold), inputs: Vec<Input>) {
        let mut instructions = shared.board.wait(|state| state.job.instructions.take());
        shared.run_turn(&mut instructions, inputs, (conversation, hold.down()));
        shared.board.lock().job.give_instructions(instructions);
        shared.board.notify();
    }
}

impl Shared<Robot> {
    fn read_context(&self, instructions: &mut Option<Instructions>, settings: &Settings) -> (String, Thread) {
        (self.instructions(instructions, settings), self.read_store(|store| store.thread.clone()))
    }

    fn settings(&self) -> Settings {
        self.board.lock().job.settings
    }

    fn write_entry(&self, kind: EntryKind, text: &str) {
        let now = Moment::from_seconds(clock::local());
        let entry = Entry::new(kind, &now, text);
        let robot = self.read_store(|store| store.memory.robot_name().map(String::from));
        match files::write_entry(&self.job.data, &now, &entry, robot.as_deref()) {
            Ok(day) => {
                let entries = files::entries(&self.job.data, &day);
                self.show_day(day, entries);
            }
            Err(e) => log!("chat: an entry is not kept: {e}"),
        }
    }

    fn add_item(&self, item: String) -> bool {
        self.change_store(|store| {
            let change = store.thread.add(item);
            files::write_item(&self.job.data, &change);
            change.is_folded()
        })
    }

    fn add_shown(&self, kept: String, sent: String) -> Shown {
        self.change_store(|store| {
            let (change, shown) = store.thread.add_shown(kept, sent);
            files::write_item(&self.job.data, &change);
            shown
        })
    }

    // A message's words: those typed, or those said as the speech model writes them down, shown as they come; none where they could not be written down.
    fn write_message(&self, query: Query, down: &NetworkDown) -> Option<String> {
        let prompt = self.read_store(|store| instructions::to_speech_prompt(&store.memory));
        let time = Moment::from_seconds(clock::local()).to_time_text();
        let mut words = String::new();
        let mut on_words = |text: &str| {
            words.push_str(text);
            self.set_current(Some(Entry {
                kind: EntryKind::User(InputKind::Voice),
                time: time.clone(),
                text: words.clone(),
            }));
        };
        let subject = (Subject::Conversation, prompt.as_str());
        self.write_down(query, down, subject, "chat: what was said is not written down", Some(&mut on_words))
            .inspect_err(|fault| {
                self.fail(fault);
                self.set_current(None);
            })
            .ok()
    }

    fn run_turn(
        &self,
        instructions: &mut Option<Instructions>,
        inputs: Vec<Input>,
        (conversation, down): (&mut Conversation, &NetworkDown),
    ) {
        let place = timekeeper::read_location(&self.job.data);
        let city = place.as_ref().map(|place| place.city.as_str());
        // Each item as the thread keeps it, and one that goes with a photo as the turn's requests send it.
        let mut items = Vec::new();
        for input in inputs {
            let (kind, words, cut_reply, photo) = match input {
                Input::Wake => {
                    self.set_phase(Phase::Waking);
                    let wake = message::to_wake_message(&message::to_stamp(clock::local(), city, None));
                    items.push((user_message(&wake, &[]), None));
                    continue;
                }
                Input::Message { query, cut_reply, photo } => {
                    let kind = to_kind(&query);
                    let Some(words) = self.write_message(query, down).filter(|words| has_words(words)) else {
                        // A message that comes to no words goes nowhere: the reply cut short and the photo wait for the next.
                        let stale = self.board.lock().job.give_back(cut_reply, photo);
                        drop(stale);
                        self.set_current(None);
                        continue;
                    };
                    (kind, words, cut_reply, photo)
                }
            };
            self.write_entry(EntryKind::User(kind), &words);
            self.set_current(None);
            let stamp = message::to_stamp(clock::local(), city, Some(kind));
            let part = cut_reply.and_then(|round| self.board.lock().job.take_heard_part(round));
            let message = message::to_message(&stamp, part.as_deref(), &words);
            let Some(photo) = photo else {
                items.push((user_message(&message, &[]), None));
                continue;
            };
            self.write_entry(EntryKind::Shown, "");
            let (with_photo, kept) = message::to_shown_messages(&message);
            let sent = user_message(&with_photo, &[photo.jpeg.as_slice()]);
            items.push((user_message(&kept, &[]), Some(sent)));
        }
        if items.is_empty() {
            self.set_idle();
            return;
        }
        let mut shown = Vec::new();
        for (item, sent) in items {
            match sent {
                Some(sent) => shown.push(self.add_shown(item, sent)),
                None => {
                    self.add_item(item);
                }
            }
        }
        self.answer(instructions, conversation, (place.as_ref(), &shown));
    }

    fn instructions(&self, instructions: &mut Option<Instructions>, settings: &Settings) -> String {
        let tools = settings.tools;
        if let Some(text) = instructions::warm_text(instructions, sys::monotonic(), tools) {
            return text.into();
        }
        let (memory, plan) =
            self.read_store(|store| (store.memory.clone(), store.robot.as_ref().and_then(RobotState::plan)));
        let today = Moment::from_seconds(clock::local()).to_compact_date();
        let days = files::read_recall_days(&self.job.data, &today);
        let text = instructions::to_instructions(&memory, plan.as_ref(), &days, tools);
        instructions.insert(Instructions::new(text, tools, sys::monotonic())).text.clone()
    }

    fn begin_voice(&self) -> bool {
        if self.read_store(Store::is_muted) {
            return false;
        }
        let task = self.read_store(|store| instructions::to_voice_instructions(&store.memory));
        self.board.lock().job.begin_voice(task);
        self.board.raise();
        true
    }

    fn add_passages(&self, passages: Vec<String>) {
        for passage in passages {
            self.board.lock().job.add_passage(passage);
            self.board.raise();
        }
    }

    fn write_answer(&self, answer: &Answer) {
        for (kind, text) in calls::to_entries(answer) {
            self.write_entry(kind, &text);
        }
        self.set_current(None);
        if let Some(line) = calls::to_cut_line(answer) {
            log!("{line}");
        }
    }

    fn answer(
        &self,
        instructions: &mut Option<Instructions>,
        conversation: &mut Conversation,
        (place, shown): (Option<&Location>, &[Shown]),
    ) {
        self.board.lock().job.begin_answer();
        let has_voice = self.begin_voice();
        let mut reply = Reply::new(has_voice);
        let ending = loop {
            // The tools of each round are those on as it begins, so a switch counts from the next request.
            let settings = self.settings();
            let tools = build_tools(place, settings.tools.is_on(Tool::Web), &tools::functions(settings.tools));
            let (text, thread) = self.read_context(instructions, &settings);
            self.set_thinking();
            reply.begin_round(Moment::from_seconds(clock::local()).to_time_text());
            let items = thread.to_sent(shown);
            let is_first = reply.is_first();
            let result = conversation.turn((&text, &items, &tools), is_first, |event| match reply.take_event(event) {
                Update::Text(entry) => {
                    self.set_phase(Phase::Thinking);
                    self.set_current(Some(entry));
                    self.add_passages(reply.take_passages(false));
                }
                Update::Search => self.set_phase(Phase::Searching),
                Update::Nothing => {}
            });
            let now = sys::monotonic();
            instructions.iter_mut().for_each(|instructions| instructions.mark_used(now));
            let answer = match result {
                Ok(answer) => answer,
                Err(fault) => break Err(fault),
            };
            for item in &answer.output {
                reply.note_item(self.add_item(item.clone()));
            }
            if reply.take_folded() {
                instructions::mark_stale(instructions);
            }
            self.write_answer(&answer);
            let tool_calls = answer.calls();
            for call in &tool_calls {
                let output = self.run_call(call, &mut reply, instructions, &settings);
                self.add_item(function_output(&call.id, &output));
            }
            if !reply.next_round(&answer, &tool_calls) {
                break Ok(());
            }
        };
        if has_voice {
            self.add_passages(reply.take_passages(true));
            self.board.lock().job.close_voice();
            self.board.raise();
        }
        self.set_current(None);
        match ending {
            Ok(()) => self.set_phase(Phase::Idle),
            Err(fault) => {
                log!("chat: the turn came to nothing: {fault}");
                self.fail(&fault);
            }
        }
        self.add_acts(reply.into_acts());
    }

    fn run_call(
        &self,
        call: &Call,
        reply: &mut Reply,
        instructions: &mut Option<Instructions>,
        settings: &Settings,
    ) -> String {
        let Some(tool_call) = tools::parse_tool_call(&call.name, &call.arguments, settings.tools) else {
            log!("{}", calls::to_unknown_call_line(&call.name));
            reply.note_call(None);
            return UNKNOWN.into();
        };
        log!("{}", calls::to_called_line(&call.name));
        let (output, act) = match calls::to_step(&tool_call, settings) {
            Step::Search { query, has_papers } => {
                let memory = self.read_store(|store| store.memory.clone());
                let papers = if has_papers { read::papers(&self.job.data) } else { Vec::new() };
                (recall::search(query, &memory, &files::every_day(&self.job.data), &papers), None)
            }
            Step::Read { target, day } => {
                let readout = |kind| self.readout(Asked::Device(kind)).told;
                (read::read_target(&self.job.data, &self.account, (target, day), settings, readout), None)
            }
            Step::Ask(asked) => calls::to_asked_answer(&tool_call, self.readout(asked)),
            Step::EndOnboarding => {
                self.mark_onboarded();
                instructions::mark_stale(instructions);
                (ONBOARDING_ANSWER.into(), None)
            }
            Step::Forget(which) => {
                let memory = self.read_store(|store| store.memory.clone());
                calls::to_forget_answer(which, &memory, &files::talk_days(&self.job.data))
            }
            Step::Remember(call) => (self.remember(call, instructions), None),
            Step::Answer(answer, act) => (answer.into(), Some(act)),
        };
        if let Some(act) = act {
            reply.add_act(act);
        }
        reply.note_call(Some(&tool_call));
        output
    }

    fn remember(&self, call: &MemoryCall, instructions: &mut Option<Instructions>) -> String {
        let today = Moment::from_seconds(clock::local());
        let (output, previous_name, name, knows_enough) = self.change_store(|store| {
            let memory = &mut store.memory;
            let previous_name = memory.robot_name().map(String::from);
            let output = calls::apply_to_memory(call, memory, &today);
            files::write_memory(&self.job.data, memory);
            (output, previous_name, memory.robot_name().map(String::from), calls::knows_enough(memory))
        });
        let is_meeting = self.read_store(Store::is_meeting);
        let effects = calls::effects_of(call, previous_name.as_deref(), name.as_deref(), knows_enough, is_meeting);
        if effects.is_renamed {
            self.post_identity();
        }
        if effects.is_named {
            self.add_milestone(Milestone::Name);
        }
        if effects.is_onboarded {
            self.mark_onboarded();
            self.add_milestone(Milestone::Enough);
        }
        if effects.is_stale {
            instructions::mark_stale(instructions);
        }
        output
    }

    fn mark_onboarded(&self) {
        self.change_store(|store| {
            if let Some(state) = store.robot.as_mut() {
                state.is_onboarded = true;
                files::write_state(&self.job.data, state);
            }
        });
        log!("{MET_LINE}");
    }

    fn readout(&self, asked: Asked) -> Answered {
        self.board.lock().job.ask_readout(asked);
        self.board.raise();
        self.board.wait(|state| state.job.readout_answer.take())
    }
}
