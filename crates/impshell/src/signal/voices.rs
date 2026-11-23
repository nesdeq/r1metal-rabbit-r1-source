use super::{Resources, Shared};
use crate::log;
use crate::sound::{self, SoundStream};
use crate::storage::Data;
use r1_funcore::domain::signal::state::State;
use r1_funcore::storage::paths::split_file;
use std::sync::Arc;

// The voice note's sound, played from its start; its end, or its stopping, is told by its number.
fn play(shared: &Arc<Shared>, data: &Data, (file, round): (&str, u32)) -> Option<SoundStream> {
    let (directory, name) = split_file(file);
    let ended = Arc::clone(shared);
    let on_end = move || {
        ended.board.lock().note_voice_ended(round);
        ended.board.raise();
    };
    let played = match data.read(directory, name) {
        Ok(Some(bytes)) => sound::play_voice(bytes, on_end).map_err(|e| e.to_string()),
        Ok(None) => Err(String::from("no such file")),
        Err(e) => Err(e.to_string()),
    };
    played
        .inspect_err(|why| {
            log!("signal: a voice note is not played: {why}");
            shared.board.lock().note_voice_ended(round);
            shared.board.raise();
        })
        .ok()
}

// The thread that plays the voice notes of a conversation, one at a time: a new order stops the one that plays.
pub(super) fn work(shared: &Arc<Shared>, resources: &Resources) -> ! {
    let mut stream: Option<SoundStream> = None;
    loop {
        let order = shared.board.wait(State::take_voice_order);
        drop(stream.take());
        stream = order.and_then(|(file, round)| play(shared, &resources.data, (&file, round)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Board;
    use crate::sys::Wake;

    #[test]
    fn unplayable_voice_note_ends() {
        let shared = Arc::new(Shared { board: Board::new(State::default()).unwrap(), poke: Wake::new().unwrap() });
        assert!(play(&shared, &Data::unusable(), ("attachments/a.m4a", 7)).is_none());
        assert_eq!(shared.board.lock().take_report().ended, [7], "the screen is told, by the number of its playing");
    }
}
