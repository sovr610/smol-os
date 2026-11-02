// SPDX-License-Identifier: GPL-3.0-or-later
//! Server-sent events, as streamed by every provider (D-058: one parser for
//! all of them).
//!
//! Follows the WHATWG event-stream rules: lines end in CRLF, LF or CR;
//! `event:`, `data:` and `id:` fields (one optional space after the
//! colon); several `data:` lines join with newlines; a blank line
//! dispatches; lines starting with `:` are comments; a leading BOM is
//! skipped; an unfinished event at the end of the stream is dropped.
//! Input arrives in arbitrary chunks (a line, even a UTF-8 character, can
//! be split across reads). Fuzzed by `fuzz/fuzz_targets/sse_parse.rs`.

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, or `message` when there was none.
    pub event: String,
    pub data: String,
    /// The last `id:` seen, if any.
    pub id: Option<String>,
}

/// The longest line accepted (a provider never sends one near this).
pub const MAX_LINE: usize = 16 * 1024 * 1024;

/// A line was longer than [`MAX_LINE`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("an event-stream line longer than {MAX_LINE} bytes")]
pub struct LineTooLong;

/// Incremental parser: feed it bytes as they arrive.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Bytes of the current, unfinished line.
    line: Vec<u8>,
    /// The previous chunk ended in CR: a LF starting the next one belongs
    /// to the same line ending.
    after_cr: bool,
    started: bool,
    event: String,
    data: String,
    has_data: bool,
    last_id: Option<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses `bytes`, returning the events they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, LineTooLong> {
        let mut out = Vec::new();
        let mut rest = bytes;
        if !self.started && !rest.is_empty() {
            // A BOM can itself arrive in pieces; wait for three bytes.
            let have = self.line.len() + rest.len();
            if have < 3 && b"\xEF\xBB\xBF".starts_with(&[self.line.as_slice(), rest].concat()) {
                self.line.extend_from_slice(rest);
                return Ok(out);
            }
            self.started = true;
            let mut head = std::mem::take(&mut self.line);
            head.extend_from_slice(rest);
            let start = if head.starts_with(b"\xEF\xBB\xBF") {
                3
            } else {
                0
            };
            return self.feed_started(&head[start..], &mut out).map(|()| out);
        }
        if self.after_cr && !rest.is_empty() {
            self.after_cr = false;
            if let Some(stripped) = rest.strip_prefix(b"\n") {
                rest = stripped;
            }
        }
        self.feed_started(rest, &mut out)?;
        Ok(out)
    }

    fn feed_started(
        &mut self,
        mut rest: &[u8],
        out: &mut Vec<SseEvent>,
    ) -> Result<(), LineTooLong> {
        while let Some(pos) = rest.iter().position(|&b| b == b'\n' || b == b'\r') {
            self.line.extend_from_slice(&rest[..pos]);
            let ending = rest[pos];
            rest = &rest[pos + 1..];
            if ending == b'\r' {
                match rest.first() {
                    Some(b'\n') => rest = &rest[1..],
                    None => self.after_cr = true,
                    Some(_) => {}
                }
            }
            let line = std::mem::take(&mut self.line);
            if let Some(ev) = self.process_line(&line) {
                out.push(ev);
            }
        }
        if self.line.len() + rest.len() > MAX_LINE {
            self.line.clear();
            return Err(LineTooLong);
        }
        self.line.extend_from_slice(rest);
        Ok(())
    }

    fn process_line(&mut self, line: &[u8]) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line[0] == b':' {
            return None;
        }
        let line = String::from_utf8_lossy(line);
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line.as_ref(), ""),
        };
        match field {
            "event" => value.clone_into(&mut self.event),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.last_id = Some(value.to_owned()),
            // `retry` and unknown fields are ignored: reconnection is not
            // how providers stream.
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = std::mem::take(&mut self.event);
        if !self.has_data {
            return None;
        }
        self.has_data = false;
        Some(SseEvent {
            event: if event.is_empty() {
                "message".to_owned()
            } else {
                event
            },
            data: std::mem::take(&mut self.data),
            id: self.last_id.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(input: &[u8]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        p.feed(input).expect("parse")
    }

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.into(),
            id: None,
        }
    }

    #[test]
    fn fields_and_dispatch() {
        let got = all(
            b"event: message_start\ndata: {\"a\":1}\n\n: comment\ndata:x\ndata: y\n\ndata\n\nevent: e\n\n",
        );
        assert_eq!(
            got,
            vec![
                ev("message_start", "{\"a\":1}"),
                ev("message", "x\ny"),
                ev("message", "")
            ]
        );
    }

    #[test]
    fn line_endings_and_bom() {
        let want = vec![ev("message", "a"), ev("message", "b"), ev("message", "c")];
        assert_eq!(
            all(b"\xEF\xBB\xBFdata: a\r\n\r\ndata: b\r\rdata: c\n\n"),
            want
        );
    }

    #[test]
    fn unfinished_events_wait_and_ids_persist() {
        let mut p = SseParser::new();
        assert!(p.feed(b"id: 7\ndata: one").expect("ok").is_empty());
        let got = p.feed(b"\n\ndata: two\n\n").expect("ok");
        assert_eq!(got[0].id.as_deref(), Some("7"));
        assert_eq!(got[1].id.as_deref(), Some("7"));
        assert_eq!(got[1].data, "two");
    }

    #[test]
    fn any_chunking_gives_the_same_events() {
        let input: &[u8] =
            "\u{FEFF}event: x\r\ndata: héllo 界\r\n\r\ndata: a\rdata: b\r\r: c\n\ndata: z\n\n"
                .as_bytes();
        let whole = all(input);
        assert_eq!(whole.len(), 3);
        for split in 0..=input.len() {
            for split2 in split..=input.len() {
                let mut p = SseParser::new();
                let mut got = p.feed(&input[..split]).expect("ok");
                got.extend(p.feed(&input[split..split2]).expect("ok"));
                got.extend(p.feed(&input[split2..]).expect("ok"));
                assert_eq!(got, whole, "split at {split}/{split2}");
            }
        }
    }

    #[test]
    fn huge_lines_are_refused() {
        let mut p = SseParser::new();
        p.feed(b"data: ").expect("ok");
        let chunk = vec![b'a'; MAX_LINE];
        assert_eq!(p.feed(&chunk), Err(LineTooLong));
    }
}
