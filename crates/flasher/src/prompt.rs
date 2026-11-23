//! A line asked for at the terminal without being shown as it is typed: an OpenAI key, a passphrase.

use crate::Result;
use crate::interrupt::{self, Taken};
use std::io::{self, BufRead, IsTerminal};

// What is said where the terminal cannot be told to hide what is typed.
const SHOWS: &str = "this terminal does not hide what is typed: it shows as it is typed";

// The terminal's settings as they stand, where standard input is a terminal that tells them.
fn settings() -> Option<libc::termios> {
    let mut shown = unsafe { std::mem::zeroed::<libc::termios>() };
    (unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut shown) } == 0).then_some(shown)
}

// Puts the terminal's settings back.
fn restore(shown: &libc::termios) {
    unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, shown) };
}

// Has the terminal show nothing of what is typed, and a Ctrl-C put `shown` back before it ends `r1` for as long as what is returned is held; beside it, false where the terminal cannot be told so.
fn hide(shown: libc::termios) -> Result<(Taken, bool)> {
    let taken = interrupt::take(move || {
        restore(&shown);
        std::process::exit(interrupt::ENDED);
    })?;
    let hidden = libc::termios { c_lflag: shown.c_lflag & !libc::ECHO, ..shown };
    Ok((taken, unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &hidden) } == 0))
}

/// What a try with a line asked for came to: its value, or why the line is refused where another may be taken.
pub enum Attempt<T> {
    Taken(T),
    Refused(Box<dyn std::error::Error>),
}

/// Asks the question and hands each line to `attempt` until one is taken; a line refused is told why and asked for again at a terminal, and is the error anywhere else.
pub fn ask<T>(question: &str, mut attempt: impl FnMut(Option<String>) -> Result<Attempt<T>>) -> Result<T> {
    let terminal = io::stdin().is_terminal();
    loop {
        match attempt(line(question)?)? {
            Attempt::Taken(taken) => return Ok(taken),
            Attempt::Refused(why) if terminal => eprintln!("{why}"),
            Attempt::Refused(why) => return Err(why),
        }
    }
}

// Asks the question and reads a line of standard input, not shown where that is a terminal, without its end; none for an empty line or the input's end.
fn line(question: &str) -> Result<Option<String>> {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let shown = if terminal { settings() } else { None };
    let taken = shown.map(hide).transpose()?;
    let hidden = taken.as_ref().is_some_and(|(_, hidden)| *hidden);
    if terminal && !hidden {
        eprintln!("{SHOWS}");
    }
    eprint!("{question}");
    let mut text = String::new();
    let read = stdin.lock().read_line(&mut text);
    if let (Some(shown), Some((taken, _))) = (shown, taken) {
        restore(&shown);
        taken.give_back()?;
    }
    if hidden {
        eprintln!();
    }
    read?;
    let typed = text.trim_end_matches(['\r', '\n']);
    Ok((!typed.is_empty()).then(|| typed.to_string()))
}
