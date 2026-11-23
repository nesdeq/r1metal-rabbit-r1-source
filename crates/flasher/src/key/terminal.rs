//! A key drawn in the terminal as the QR code the R1's key window scans.

use crate::Result;
use r1_funcore::codec::qr::{self, Level, QUIET_ZONE};
use std::io::{self, BufRead};

// The code's level of error correction: the middle one, about a seventh of it may be lost, for a picture of a screen with its glare and its moiré.
const LEVEL: Level = Level::Medium;
// The background colour codes of a module drawn dark and light, and what puts the terminal's colours back.
const BACKGROUND: (&str, &str) = ("40", "107");
const PLAIN: &str = "\x1b[0m";
// Characters across a module, a line down it: a character is about half as wide as its line is tall.
const ACROSS_MODULE: usize = 2;
// Lines the terminal shows below a code: what is said after it, and the shell's prompt.
const BELOW: usize = 2;

// A key as a QR code in a terminal, dark on light with the quiet zone about it, and the modules along its side with that zone; none for a key too long for any code. Every module is spaces in its colour as their background, which every terminal fills its whole character with, never a block's glyph: macOS's Terminal draws a glyph in part of its line only, an upper half block from a sixth of the line down, and fonts draw them as they draw them.
pub(super) fn code(key: &str) -> Option<(String, usize)> {
    let symbol = qr::make(key.as_bytes(), LEVEL)?;
    let (side, whole) = (symbol.side(), symbol.side() + 2 * QUIET_ZONE);
    let module = |at: usize| at.checked_sub(QUIET_ZONE).filter(|at| *at < side);
    let dark = |x: usize, y: usize| module(x).zip(module(y)).is_some_and(|(x, y)| symbol.is_dark(x, y));
    let mut drawn = String::new();
    for y in 0..whole {
        let mut drawn_in = None;
        for x in 0..whole {
            let shown = dark(x, y);
            if drawn_in != Some(shown) {
                drawn.push_str(&format!("\x1b[{}m", if shown { BACKGROUND.0 } else { BACKGROUND.1 }));
                drawn_in = Some(shown);
            }
            drawn.extend(std::iter::repeat_n(' ', ACROSS_MODULE));
        }
        drawn.push_str(PLAIN);
        drawn.push('\n');
    }
    Some((drawn, whole))
}

// The characters across and the lines down of the terminal standard output is, where it is one that tells them.
fn window() -> Option<(usize, usize)> {
    let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
    let told = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0 && size.ws_col > 0;
    told.then_some((size.ws_col.into(), size.ws_row.into()))
}

// Asks, until the terminal's window holds a code of `side` modules whole with the lines below it, for the window made larger; at once where standard output is no terminal that tells its size. A code wider than its window breaks across lines, and one taller loses its top.
pub(super) fn room_for(side: usize) -> Result<()> {
    let (across, down) = (side * ACROSS_MODULE, side + BELOW);
    while let Some((window_across, window_down)) = window().filter(|&(x, y)| x < across || y < down) {
        let needs = format!(
            "the code needs a window of {across}×{down} characters, this one shows {window_across}×{window_down}"
        );
        eprint!("{needs}: make it larger or its text smaller, then press Enter");
        if io::stdin().lock().read_line(&mut String::new())? == 0 {
            return Err(needs.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A key as OpenAI's look, made up.
    const KEY: &str = "sk-proj-Fake0Key_-9abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

    // Each line's characters of a drawn code as the modules their colours draw, dark or not.
    fn characters(drawn: &str) -> Vec<Vec<bool>> {
        drawn
            .lines()
            .map(|line| {
                let mut rest = line.strip_suffix(PLAIN).expect("its colours put back");
                let (mut row, mut drawn_in) = (Vec::new(), None);
                while let Some(character) = rest.chars().next() {
                    if let Some(code) = rest.strip_prefix("\x1b[") {
                        let (code, after) = code.split_once('m').expect("a colour code");
                        assert!([BACKGROUND.0, BACKGROUND.1].contains(&code), "{code}");
                        drawn_in = Some(code == BACKGROUND.0);
                        rest = after;
                    } else {
                        assert_eq!(character, ' ', "drawn by colours");
                        row.push(drawn_in.expect("a character in its colours"));
                        rest = &rest[1..];
                    }
                }
                row
            })
            .collect()
    }

    #[test]
    fn key_code_reads_back() {
        let (drawn, whole) = code(KEY).expect("a code");
        let (rows, across) = (characters(&drawn), whole * ACROSS_MODULE);
        assert!(rows.len() == whole && rows.iter().all(|row| row.len() == across), "a square of modules");
        assert!(
            rows.iter().all(|row| row.chunks(ACROSS_MODULE).all(|module| module.iter().all(|dark| *dark == module[0]))),
            "a module alike across its characters"
        );
        let dark = |x: usize, y: usize| rows[y][x * ACROSS_MODULE];
        let side = whole - 2 * QUIET_ZONE;
        let inside = |at: usize| (QUIET_ZONE..QUIET_ZONE + side).contains(&at);
        assert!((0..whole).all(|y| (0..whole).all(|x| inside(x) && inside(y) || !dark(x, y))), "light all about it");
        let modules = (0..side * side).map(|at| dark(QUIET_ZONE + at % side, QUIET_ZONE + at / side)).collect();
        let symbol = qr::Symbol::new(side, modules).expect("a symbol of a version's side");
        assert_eq!(qr::read(&symbol).as_deref(), Some(KEY.as_bytes()));
    }

    #[test]
    fn terminal_picture_scans() {
        // A character 16 pixels across and 36 down, its background filling it, as a screenshot of macOS's Terminal measured; the light of its dark and light, and of the terminal's grey about the code.
        const ACROSS: usize = 16;
        const DOWN: usize = 36;
        const MARGIN: usize = 200;
        let ((dark, light), grey) = ((62, 236), 76);
        let rows = characters(&code(KEY).expect("a code").0);
        let (wide, tall) = (rows[0].len() * ACROSS + 2 * MARGIN, rows.len() * DOWN + 2 * MARGIN);
        let mut picture = vec![grey; wide * tall];
        for (line, row) in rows.iter().enumerate() {
            for (at, &shown) in row.iter().enumerate() {
                for down in 0..DOWN {
                    let start = (MARGIN + line * DOWN + down) * wide + MARGIN + at * ACROSS;
                    picture[start..start + ACROSS].fill(if shown { dark } else { light });
                }
            }
        }
        assert_eq!(qr::Scanner::default().scan(&picture, wide).as_deref(), Some(KEY.as_bytes()));
    }
}
