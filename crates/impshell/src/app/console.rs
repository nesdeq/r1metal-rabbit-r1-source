use super::{Source, watch};
use crate::sys::{self, Epoll};
use crate::usb::Console;
use crate::{clock, log};
use r1_funcore::cell::sim::Pin;
use r1_funcore::domain::console::answer::{key_answer, pong_answer, to_record_answer};
use r1_funcore::domain::console::command::{Capabilities, Command, Refusal, read_line};
use r1_funcore::domain::console::queue::Queue;
use r1_funcore::domain::console::wire::{Line, Reply, TOO_LONG};
use r1_funcore::domain::settings::Settings;
use r1_funcore::hw::power::Power;
use r1_funcore::input::Event;
use r1_funcore::input::pose::Pose;
use r1_funcore::net::wireguard::config::Config;
use r1_funcore::ui::Ui;

#[derive(Default)]
pub(super) struct Requests {
    pub(super) events: Vec<Event>,
    pub(super) pose: Option<Pose>,
    pub(super) redraw: bool,
    pub(super) time: bool,
    pub(super) edition: bool,
    pub(super) chat: Vec<String>,
    pub(super) record: Option<(bool, u64)>,
    pub(super) key: Option<String>,
    pub(super) tunnel: Option<Config>,
    pub(super) pin: Option<Pin>,
    pub(super) attach: Option<bool>,
    pub(super) at: Vec<String>,
}

#[derive(Default)]
pub(super) struct Dispatch {
    #[cfg(feature = "debug-shell")]
    shell: Option<super::shell::Shell>,
    queue: Queue,
}

impl Dispatch {
    #[cfg(feature = "debug-shell")]
    pub(super) fn with_shell(shell: Option<super::shell::Shell>) -> Dispatch {
        Dispatch { shell, ..Dispatch::default() }
    }

    fn note_hang_up(&mut self) {
        self.queue.note_hang_up();
        #[cfg(feature = "debug-shell")]
        self.shell.iter().for_each(super::shell::Shell::lock);
    }
}

pub(super) struct Host {
    pub(super) console: Option<Console>,
    pub(super) requests: Requests,
    pub(super) dispatch: Dispatch,
    pub(super) is_recordable: bool,
}

fn execute(
    line: &str,
    reply: &mut Reply,
    ui: &mut Ui,
    (requests, dispatch): (&mut Requests, &mut Dispatch),
    (is_recordable, is_cell_usable): (bool, bool),
) {
    let capabilities = Capabilities {
        can_turn: ui.does_turn_show(),
        can_record: is_recordable,
        can_link: ui.can_link(is_cell_usable),
        can_cell: is_cell_usable,
    };
    let command = match read_line(line, capabilities) {
        Ok(command) => command,
        Err(reason) => {
            reply.err(reason);
            return;
        }
    };
    let (needs_thread, power) = (command.needs_thread(), command.to_power());
    // A tunnel's config is read where the shell's gate is, and kept by the router's thread.
    #[cfg(feature = "debug-shell")]
    if let Command::Tunnel(_) = &command {
        match dispatch.shell.as_ref().map(|shell| shell.read_tunnel(&command)) {
            Some(Ok(config)) => {
                requests.tunnel = Some(config);
                dispatch.queue.end_command(needs_thread, power, reply);
            }
            Some(Err(reason)) => drop(reply.err(reason)),
            None => drop(reply.err(Refusal::NoShellThread.message())),
        }
        return;
    }
    match command {
        Command::Ping(word) => reply.data(pong_answer(word.as_deref())),
        Command::Anim(is_on) => {
            ui.set_pacing(is_on);
            ui.pacing().to_answer().iter().for_each(|line| reply.data(line));
        }
        Command::Reboot | Command::Bootloader | Command::PowerOff => {}
        Command::Input(event) => requests.events.push(event),
        Command::Pose(pose) => requests.pose = Some(pose),
        Command::Clock { utc, offset } => {
            if let Err(e) = clock::set(utc) {
                reply.err(&e.to_string());
                return;
            }
            requests.redraw |= ui.set_offset(offset);
        }
        Command::Time => requests.time = true,
        Command::Radio(is_on) => ui.set_console_radio(is_on),
        Command::Link(link) => ui.set_console_link(link),
        Command::Camera(is_on) => requests.redraw |= ui.set_console_camera(is_on),
        Command::News => requests.edition = true,
        Command::Chat(text) => requests.chat.push(text),
        Command::Record(is_on) => {
            let at_ns = sys::monotonic_ns();
            to_record_answer(is_on, at_ns).iter().for_each(|line| reply.data(line));
            requests.record = Some((is_on, at_ns));
        }
        Command::Key(None) => reply.data(key_answer(ui.is_keyed())),
        Command::Log(logging) => requests.redraw |= ui.set_settings(Settings { logging, ..ui.settings() }),
        Command::Key(Some(key)) => requests.key = Some(key),
        Command::Pin(pin) => requests.pin = Some(pin),
        Command::Attach(is_on) => requests.attach = Some(is_on),
        Command::At(text) => requests.at.push(text),
        #[cfg(feature = "debug-shell")]
        shell_verb => {
            // The thread that draws runs none of the shell's verbs itself.
            let Some(shell) = &dispatch.shell else {
                reply.err(Refusal::NoShellThread.message());
                return;
            };
            shell.hand(shell_verb);
        }
        #[cfg(not(feature = "debug-shell"))]
        _ => {
            reply.err(Refusal::NoShell.message());
            return;
        }
    }
    dispatch.queue.end_command(needs_thread, power, reply);
}

fn run_lines(
    console: &mut Console,
    ui: &mut Ui,
    (requests, dispatch): (&mut Requests, &mut Dispatch),
    abilities: (bool, bool),
) -> Option<Power> {
    let mut reply = Reply::default();
    while let Some(line) = dispatch.queue.next_line() {
        match line {
            Line::Command(line) => execute(&line, &mut reply, ui, (requests, dispatch), abilities),
            Line::TooLong => drop(reply.err(TOO_LONG)),
        }
    }
    console.answer(&reply);
    dispatch.queue.take_power()
}

pub(super) fn open_console(epoll: &mut Epoll) -> Option<Console> {
    watch(epoll, Source::Console, "usb console", Console::open(), Console::fd)
}

impl Host {
    pub(super) fn serve(&mut self, epoll: &mut Epoll, ui: &mut Ui, is_cell_usable: bool) -> Option<Power> {
        let Host { console, requests, dispatch, is_recordable } = self;
        let port = console.as_mut()?;
        match port.read_lines() {
            Ok(lines) => {
                dispatch.queue.add(lines);
                run_lines(port, ui, (requests, dispatch), (*is_recordable, is_cell_usable))
            }
            Err(_) => {
                if let Err(e) = epoll.remove(port.fd()) {
                    log!("usb console: ending its watch: {e}");
                }
                *console = None;
                *console = open_console(epoll);
                dispatch.note_hang_up();
                None
            }
        }
    }

    // Answers the host what keeping a key or a tunnel's config it handed over came to, and runs the lines that waited behind it.
    pub(super) fn answer_keep(
        &mut self,
        result: Result<(), String>,
        ui: &mut Ui,
        is_cell_usable: bool,
    ) -> Option<Power> {
        let Host { console, requests, dispatch, is_recordable } = self;
        let answer = dispatch.queue.take_keep_answer(&result);
        let port = console.as_mut()?;
        answer.iter().for_each(|reply| port.answer(reply));
        run_lines(port, ui, (requests, dispatch), (*is_recordable, is_cell_usable))
    }

    #[cfg(feature = "debug-shell")]
    pub(super) fn answer_shell(&mut self, ui: &mut Ui, is_cell_usable: bool) -> Option<Power> {
        let Host { console, requests, dispatch, is_recordable } = self;
        let replies = dispatch.shell.as_ref().map(super::shell::Shell::latest).unwrap_or_default();
        if replies.is_empty() {
            return None;
        }
        let needs_answer = dispatch.queue.take_answer();
        let port = console.as_mut()?;
        if needs_answer {
            replies.iter().for_each(|reply| port.answer(reply));
        }
        run_lines(port, ui, (requests, dispatch), (*is_recordable, is_cell_usable))
    }
}
