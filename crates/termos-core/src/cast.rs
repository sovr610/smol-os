// SPDX-License-Identifier: GPL-3.0-or-later
//! Pane recordings in asciinema's v2 format (`.cast`, M13b, D-120): a JSON
//! header line, then one JSON array per event, `[seconds, "o", text]` for
//! output and `[seconds, "r", "COLSxROWS"]` for a resize.
//!
//! Only output is recorded, never the keys typed (D-119). What a shell
//! echoes is output, so typed text the shell shows is in the file.

use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The header line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub version: u32,
    pub width: u16,
    pub height: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// One event of a recording.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Output(f64, String),
    Resize(f64, u16, u16),
}

impl Event {
    pub fn time(&self) -> f64 {
        match self {
            Event::Output(t, _) | Event::Resize(t, _, _) => *t,
        }
    }
}

/// Writes a recording as it happens.
#[derive(Debug)]
pub struct Writer<W: Write> {
    out: W,
    start: Instant,
    /// The end of the last output when it stopped inside a UTF-8 sequence.
    partial: Vec<u8>,
}

impl<W: Write> Writer<W> {
    /// Starts a recording of a `width`×`height` pane.
    pub fn new(mut out: W, header: &Header) -> io::Result<Self> {
        serde_json::to_writer(&mut out, header)?;
        out.write_all(b"\n")?;
        Ok(Self {
            out,
            start: Instant::now(),
            partial: Vec::new(),
        })
    }

    fn line(&mut self, value: &serde_json::Value) -> io::Result<()> {
        serde_json::to_writer(&mut self.out, value)?;
        self.out.write_all(b"\n")
    }

    fn now(&self) -> f64 {
        (self.start.elapsed().as_secs_f64() * 1e6).round() / 1e6
    }

    /// Program output. A UTF-8 sequence cut between two reads waits for
    /// its end; other invalid bytes become U+FFFD.
    pub fn output(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(bytes);
        let keep = incomplete_tail(&data);
        self.partial = data.split_off(data.len() - keep);
        if data.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&data).into_owned();
        let t = self.now();
        self.line(&serde_json::json!([t, "o", text]))
    }

    /// The pane changed size.
    pub fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        let t = self.now();
        self.line(&serde_json::json!([t, "r", format!("{width}x{height}")]))
    }

    /// Writes what is left and gives the output back.
    pub fn finish(mut self) -> io::Result<W> {
        if !self.partial.is_empty() {
            let rest = std::mem::take(&mut self.partial);
            let text = String::from_utf8_lossy(&rest).into_owned();
            let t = self.now();
            self.line(&serde_json::json!([t, "o", text]))?;
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

/// How many bytes at the end of `data` start a UTF-8 sequence that is not
/// complete yet (0 to 3).
fn incomplete_tail(data: &[u8]) -> usize {
    for back in 1..=3.min(data.len()) {
        let b = data[data.len() - back];
        if b & 0b1100_0000 != 0b1000_0000 {
            // The lead byte: how long its sequence is.
            let len = match b {
                0xC2..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF4 => 4,
                _ => 1,
            };
            return if len > back { back } else { 0 };
        }
    }
    0
}

/// Why a recording cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("cannot read the recording: {0}")]
    Io(#[from] io::Error),
    #[error("line {line}: {message}")]
    Invalid { line: usize, message: String },
}

/// A recording read back: its header and events, in time order.
pub fn read(input: impl BufRead) -> Result<(Header, Vec<Event>), ReadError> {
    let mut lines = input.lines();
    let first = lines.next().ok_or(ReadError::Invalid {
        line: 1,
        message: "the file is empty".into(),
    })??;
    let header: Header = serde_json::from_str(&first).map_err(|e| ReadError::Invalid {
        line: 1,
        message: format!("not an asciinema header: {e}"),
    })?;
    if header.version != 2 {
        return Err(ReadError::Invalid {
            line: 1,
            message: format!("version {} (TermOS plays version 2)", header.version),
        });
    }
    let mut events = Vec::new();
    let mut last = 0.0f64;
    for (i, line) in lines.enumerate() {
        let n = i + 2;
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let bad = |message: &str| ReadError::Invalid {
            line: n,
            message: message.to_owned(),
        };
        let (t, kind, data): (f64, String, String) =
            serde_json::from_str(&line).map_err(|_| bad("not an event [time, kind, data]"))?;
        if !t.is_finite() || t < 0.0 {
            return Err(bad("a time before the start"));
        }
        let t = t.max(last);
        last = t;
        match kind.as_str() {
            "o" => events.push(Event::Output(t, data)),
            "r" => {
                let (w, h) = data
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .ok_or_else(|| bad("a resize that is not COLSxROWS"))?;
                events.push(Event::Resize(t, w, h));
            }
            // Input, markers and other kinds: not played.
            _ => {}
        }
    }
    Ok((header, events))
}

/// When each event plays, from the start: the recorded times divided by
/// `speed`, with pauses longer than `idle_limit` cut to it.
pub fn schedule(events: &[Event], speed: f64, idle_limit: Option<f64>) -> Vec<Duration> {
    let speed = if speed.is_finite() && speed > 0.0 {
        speed
    } else {
        1.0
    };
    let mut at = 0.0f64;
    let mut prev = 0.0f64;
    events
        .iter()
        .map(|e| {
            let mut gap = (e.time() - prev).max(0.0);
            prev = e.time();
            if let Some(limit) = idle_limit.filter(|l| *l > 0.0) {
                gap = gap.min(limit);
            }
            at += gap / speed;
            Duration::from_secs_f64(at)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_round_trips() {
        let header = Header {
            version: 2,
            width: 80,
            height: 24,
            timestamp: Some(1_700_000_000),
            title: Some("shell".into()),
        };
        let mut w = Writer::new(Vec::new(), &header).expect("header");
        w.output(b"hello \xe2\x9c").expect("write");
        w.output(b"\x93\r\n").expect("write");
        w.resize(100, 30).expect("write");
        w.output(b"bad \xff byte").expect("write");
        let bytes = w.finish().expect("finish");
        let (h, events) = read(bytes.as_slice()).expect("valid");
        assert_eq!(h, header);
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::Output(_, t) => Some(t.as_str()),
                Event::Resize(..) => None,
            })
            .collect();
        assert_eq!(texts, vec!["hello ", "✓\r\n", "bad \u{fffd} byte"]);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Resize(_, 100, 30)))
        );
        // Each line is valid JSON: what asciinema itself reads.
        for line in String::from_utf8(bytes).expect("utf8").lines() {
            serde_json::from_str::<serde_json::Value>(line).expect("JSON");
        }
    }

    #[test]
    fn bad_recordings_say_where() {
        assert!(read(&b""[..]).is_err());
        let e = read(&b"{\"version\":1,\"width\":1,\"height\":1}\n"[..]).expect_err("v1");
        assert!(e.to_string().contains("version 1"), "{e}");
        let e =
            read(&b"{\"version\":2,\"width\":1,\"height\":1}\n[1,\"o\"]\n"[..]).expect_err("bad");
        assert!(e.to_string().starts_with("line 2"), "{e}");
        let e = read(&b"{\"version\":2,\"width\":1,\"height\":1}\n[1,\"r\",\"big\"]\n"[..])
            .expect_err("bad resize");
        assert!(e.to_string().contains("COLSxROWS"), "{e}");
    }

    #[test]
    fn playback_times_follow_speed_and_idle_limit() {
        let events = vec![
            Event::Output(1.0, "a".into()),
            Event::Output(11.0, "b".into()),
            Event::Output(12.0, "c".into()),
        ];
        let secs = |v: Vec<Duration>| v.iter().map(Duration::as_secs_f64).collect::<Vec<_>>();
        assert_eq!(secs(schedule(&events, 1.0, None)), vec![1.0, 11.0, 12.0]);
        assert_eq!(secs(schedule(&events, 2.0, Some(2.0))), vec![0.5, 1.5, 2.0]);
        assert_eq!(secs(schedule(&events, 0.0, None)), vec![1.0, 11.0, 12.0]);
    }
}
