use super::{Account, boundary};
use crate::log;
use crate::wifi::radio::NetworkDown;
use r1_funcore::codec::form;
use r1_funcore::codec::http::POST;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::models::transcriber;
use r1_funcore::openai::request::{finished_stream, parse_target};
use r1_funcore::openai::transcript::{self, Stream, Subject};
use std::io;
use std::time::Instant;

/// Makes the connection a recording's words go over ahead, in the stay `down` ends, for an account that may pay for them: the words then wait for no handshake.
pub fn prepare(account: &Account, down: &NetworkDown) {
    let ready = account.paying().and_then(|_| {
        let target = parse_target(transcriber::ADDRESS)?;
        Ok(account.prepare_connection(&target, down)?)
    });
    if let Err(reason) = ready {
        log!("openai: no connection stands ready for a recording's words: {reason}");
    }
}

/// Has a recording of `seconds` written down, prompted as `prompt` tells, and books it as its subject's; `on_words` gets the words as they come, and `down` tells when the network is gone. Returns the words, empty where nothing was said.
pub fn transcribe(
    account: &Account,
    down: &NetworkDown,
    (recording, seconds): (&[u8], u64),
    (subject, prompt): (Subject, &str),
    on_words: Option<&mut dyn FnMut(&str)>,
) -> Result<String, Fault> {
    let (key, target, start) = (account.paying()?, parse_target(transcriber::ADDRESS)?, Instant::now());
    let boundary = boundary()?;
    let kind = form::content_type(&boundary);
    let body = transcript::request_form(&boundary, recording, prompt, on_words.is_some());
    let transcript = match on_words {
        None => {
            let (status, answer) = key.ask((&target, down), POST, Some(&kind), &body, |_| {})?;
            transcript::parse_answer(&answer).map_err(|error| error.to_fault(status))?
        }
        Some(on_words) => {
            let mut stream = Stream::default();
            let result = key.ask_stream((&target, down), &kind, &body, |data| {
                if let Some(words) = stream.take(data) {
                    on_words(&words);
                }
            });
            let (status, answer) = accept_ended(result, stream.has_end())?;
            stream.finish(status, &answer)?
        }
    };
    // What was written down was paid for, whatever becomes of it here.
    let paid = transcript.paid_seconds(seconds);
    let micros = transcript::sound_micros(paid);
    account.add(|ledger, year, month| {
        subject.book(ledger, (year, month), paid, micros);
        true
    });
    log!("{}", transcript.to_log_line(recording.len(), subject, start.elapsed().as_millis(), paid));
    Ok(transcript.words)
}

fn accept_ended(result: io::Result<(u16, Vec<u8>)>, has_end: bool) -> io::Result<(u16, Vec<u8>)> {
    result.or_else(|e| match finished_stream(has_end) {
        Some(answer) => {
            log!("openai: the connection failed after the answer's end, which stands: {e}");
            Ok(answer)
        }
        None => Err(e),
    })
}

/// Books a note whose transcript is complete and stored; its sound's pieces were booked as they were paid.
pub fn book_note(account: &Account) {
    account.add(|ledger, year, month| {
        ledger.add_note(year, month);
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::radio::tests::up;
    use r1_funcore::domain::{pokedex, web};

    #[test]
    fn no_key_no_transcript() {
        let (account, down) = (crate::openai::tests::unusable(), up());
        let prompt = pokedex::prompt(pokedex::Language::German);
        let subjects = [
            (Subject::Note, ""),
            (Subject::PokedexName, prompt.as_str()),
            (Subject::WebRequest, web::state::PROMPT),
            (Subject::Message, ""),
            (Subject::Conversation, ""),
        ];
        for subject in subjects {
            assert!(
                matches!(transcribe(&account, &down, (b"sound", 3), subject, None), Err(Fault::Temporary(_))),
                "{subject:?}: a partition that cannot be read"
            );
        }
        let mut on_words = |_: &str| {};
        let streamed = transcribe(&account, &down, (b"sound", 3), (Subject::Conversation, ""), Some(&mut on_words));
        assert!(matches!(streamed, Err(Fault::Temporary(_))), "nor are its words asked for as they come");
        prepare(&account, &down);
        assert_eq!(account.0.pool.idle_count(), 0, "and no connection is made ahead for words nobody can pay for");
        book_note(&account);
    }
}
