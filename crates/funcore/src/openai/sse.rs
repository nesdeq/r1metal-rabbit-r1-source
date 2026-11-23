use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

const LF_END: &[u8] = b"\n\n";
const CRLF_END: &[u8] = b"\r\n\r\n";
const LONGEST_PARTIAL_END: usize = CRLF_END.len() - 1;

fn find_event_end(buffer: &[u8], start: usize) -> Option<(usize, usize)> {
    (start..buffer.len()).find_map(|at| {
        let rest = &buffer[at..];
        [LF_END, CRLF_END].into_iter().find(|end| rest.starts_with(end)).map(|end| (at, end.len()))
    })
}

#[derive(Default)]
pub(super) struct EventStream {
    buffer: Vec<u8>,
    offset: usize,
}

impl EventStream {
    // Hands on the data of every event `bytes` complete, and returns how many bytes it holds of the one still to end.
    pub(super) fn take(&mut self, bytes: &[u8], mut on_event: impl FnMut(&str)) -> usize {
        self.buffer.extend_from_slice(bytes);
        let mut start = 0;
        while let Some((end, gap)) = find_event_end(&self.buffer, self.offset.max(start)) {
            let event = String::from_utf8_lossy(&self.buffer[start..end]);
            let mut data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|data| data.strip_prefix(' ').unwrap_or(data));
            if let Some(first) = data.next() {
                let mut text = Cow::Borrowed(first);
                for line in data {
                    let all = text.to_mut();
                    all.push('\n');
                    all.push_str(line);
                }
                on_event(&text);
            }
            start = end + gap;
        }
        self.buffer.drain(..start);
        self.offset = self.buffer.len().saturating_sub(LONGEST_PARTIAL_END);
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_at_any_cut() {
        let stream = b"event: response.created\ndata: {\"type\":\"response.created\"}\n\nevent: x\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n: a comment\n\ndata: tail";
        let expected = {
            let mut events = Vec::new();
            let held = EventStream::default().take(stream, |data| events.push(String::from(data)));
            assert_eq!(held, b"data: tail".len(), "what has no end yet is held");
            events
        };
        assert_eq!(expected, ["{\"type\":\"response.created\"}", "{\"a\":\n1}"], "an event's lines of data are joined");
        for cut in 1..stream.len() {
            let (mut parts, mut events) = (EventStream::default(), Vec::new());
            parts.take(&stream[..cut], |data| events.push(String::from(data)));
            parts.take(&stream[cut..], |data| events.push(String::from(data)));
            assert_eq!(events, expected, "cut at {cut}");
        }
    }
}
