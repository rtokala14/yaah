//! Minimal blocking Server-Sent Events reader over any `Read`.

use std::io::{BufRead, BufReader, Read};

pub struct SseEvent {
    pub event: String,
    pub data: String,
}

pub struct SseReader<R: Read> {
    lines: std::io::Lines<BufReader<R>>,
}

impl<R: Read> SseReader<R> {
    pub fn new(reader: R) -> Self {
        Self { lines: BufReader::new(reader).lines() }
    }
}

impl<R: Read> Iterator for SseReader<R> {
    type Item = std::io::Result<SseEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut event = String::from("message");
        let mut data: Vec<String> = Vec::new();
        loop {
            match self.lines.next() {
                None => {
                    // EOF: flush any trailing event
                    if data.is_empty() {
                        return None;
                    }
                    return Some(Ok(SseEvent { event, data: data.join("\n") }));
                }
                Some(Err(e)) => return Some(Err(e)),
                Some(Ok(line)) => {
                    let line = line.trim_end_matches('\r');
                    if line.is_empty() {
                        if data.is_empty() {
                            // blank line with no pending data: keep scanning
                            event = String::from("message");
                            continue;
                        }
                        return Some(Ok(SseEvent { event, data: data.join("\n") }));
                    } else if let Some(rest) = line.strip_prefix("event:") {
                        event = rest.trim().to_string();
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
                    }
                    // comments (":...") and other fields ignored
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_and_multiline_data() {
        let input = "event: delta\ndata: {\"a\":1}\n\ndata: l1\ndata: l2\n\n: comment\n\ndata: [DONE]\n\n";
        let events: Vec<_> = SseReader::new(input.as_bytes()).map(|e| e.unwrap()).collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event, "delta");
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(events[1].data, "l1\nl2");
        assert_eq!(events[2].data, "[DONE]");
    }

    #[test]
    fn handles_crlf() {
        let input = "event: ping\r\ndata: {}\r\n\r\n";
        let events: Vec<_> = SseReader::new(input.as_bytes()).map(|e| e.unwrap()).collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "ping");
    }
}
