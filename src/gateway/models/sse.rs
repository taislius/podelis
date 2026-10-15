//! Server-sent events, as both APIs stream answers: `event:` and `data:`
//! lines, an event ending at a blank line.

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Event {
    /// `event:` (empty when the stream doesn't name events).
    pub name: String,
    /// The `data:` lines, joined with `\n`.
    pub data: String,
}

/// Collects bytes as they come and hands out whole events.
#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    event: Event,
    has_data: bool,
}

impl Parser {
    /// At the end of the stream: an event its server didn't end with a
    /// blank line (the last line, too, if it had no line break).
    pub fn finish(&mut self) -> Option<Event> {
        let mut events = self.push(b"\n");
        events.extend(self.push(b"\n"));
        events.pop()
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if self.has_data {
                    events.push(std::mem::take(&mut self.event));
                }
                self.event = Event::default();
                self.has_data = false;
                continue;
            }
            // A comment (keep-alive).
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((&line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => self.event.name = value.to_string(),
                "data" => {
                    if self.has_data {
                        self.event.data.push('\n');
                    }
                    self.event.data.push_str(value);
                    self.has_data = true;
                }
                _ => {}
            }
        }
        events
    }
}
