// SPDX-License-Identifier: GPL-3.0-or-later
//! What programs signal through OSCs the emulator ignores: notifications
//! (OSC 9, 99, 777) and shell marks (OSC 133, 633), as the interceptor
//! hands them over ([`crate::intercept::Piece::Osc`], D-116).
//!
//! - **OSC 9** `9;text`: iTerm2's notification. ConEmu's commands share
//!   the number (`9;4;…` is a progress bar); a body that starts with a
//!   number and `;` is one of those, and ignored.
//! - **OSC 777** `777;notify;title;body`: rxvt's, also WezTerm's and
//!   foot's; other `777` extensions are ignored.
//! - **OSC 99**: kitty's, `99;key=value:…;payload`, a notification
//!   possibly sent in parts (`d=0` until the last), its title and body as
//!   separate parts (`p=title`, `p=body`), maybe base64 (`e=1`).
//! - **OSC 133** (FinalTerm, kitty, WezTerm, iTerm2): `A` a prompt starts,
//!   `B` the command line starts, `C` its output starts, `D;status` it
//!   finished. `A;k=s` (a continuation prompt) is ignored.
//! - **OSC 633** (VS Code): the same four, and `E;command line` with `\\`
//!   and `\xAB` escapes.

use base64::Engine as _;

/// Longest title kept.
const MAX_TITLE: usize = 256;
/// Longest body kept.
const MAX_BODY: usize = 1024;
/// Longest command line kept.
const MAX_COMMAND: usize = 4096;
/// kitty notifications being assembled at once.
const MAX_PENDING: usize = 8;

/// A mark a shell sets around its prompts and commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// `A`: a prompt starts.
    PromptStart,
    /// `B`: the prompt ends and the command line starts.
    InputStart,
    /// `C`: the command runs; its output starts.
    OutputStart,
    /// `D;status`: the command finished.
    Finished(Option<i32>),
    /// 633 `E`: the command line, as the shell read it.
    CommandLine(String),
}

/// A notification a program sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

/// What an OSC said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Mark(Mark),
    Notify(Notice),
}

/// Reads OSC `number` (all but 99, which needs [`KittyNotices`]).
pub fn parse(number: u16, body: &[u8]) -> Option<Signal> {
    match number {
        9 => {
            // ConEmu's `9;n;…` commands (progress, sleep, …) are not
            // notifications.
            let digits = body.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0 && matches!(body.get(digits), None | Some(b';')) {
                return None;
            }
            let body = clean(&String::from_utf8_lossy(body), MAX_BODY);
            (!body.is_empty()).then(|| {
                Signal::Notify(Notice {
                    title: String::new(),
                    body,
                })
            })
        }
        777 => {
            let text = String::from_utf8_lossy(body);
            let rest = text.strip_prefix("notify;")?;
            let (title, body) = rest.split_once(';').unwrap_or((rest, ""));
            let notice = Notice {
                title: clean(title, MAX_TITLE),
                body: clean(body, MAX_BODY),
            };
            (!notice.title.is_empty() || !notice.body.is_empty()).then_some(Signal::Notify(notice))
        }
        133 | 633 => mark(number, body).map(Signal::Mark),
        _ => None,
    }
}

fn mark(number: u16, body: &[u8]) -> Option<Mark> {
    let (kind, params) = match body.iter().position(|b| *b == b';') {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, &[][..]),
    };
    let params = String::from_utf8_lossy(params);
    Some(match kind {
        b"A" => {
            // A continuation prompt (`k=s`) is part of the same command.
            if params.split(';').any(|p| p == "k=s" || p == "k=c") {
                return None;
            }
            Mark::PromptStart
        }
        b"B" => Mark::InputStart,
        b"C" => Mark::OutputStart,
        b"D" => Mark::Finished(params.split(';').next().and_then(|s| s.trim().parse().ok())),
        b"E" if number == 633 => {
            // The command line, then perhaps a nonce.
            let line = params.split(';').next().unwrap_or_default();
            Mark::CommandLine(clean_command(&unescape_633(line)))
        }
        _ => return None,
    })
}

/// VS Code's escaping: `\\` is a backslash, `\xAB` a byte.
fn unescape_633(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            if b.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if b.get(i + 1) == Some(&b'x')
                && let Some(hex) = s.get(i + 2..i + 4)
                && let Ok(v) = u8::from_str_radix(hex, 16)
            {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Text without control characters (newlines become spaces), cut to
/// `max` characters.
fn clean(s: &str, max: usize) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.trim_start().to_owned()
}

/// A command line: newlines kept, other control characters dropped.
fn clean_command(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .take(MAX_COMMAND)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// kitty's notifications (OSC 99), assembled from their parts.
#[derive(Debug, Default)]
pub struct KittyNotices {
    /// By id (empty: none given), in arrival order.
    pending: Vec<(String, Notice)>,
}

impl KittyNotices {
    /// Takes one OSC 99 body; the notification, when it is complete.
    pub fn feed(&mut self, body: &[u8]) -> Option<Notice> {
        let (meta, payload) = match body.iter().position(|b| *b == b';') {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => (body, &[][..]),
        };
        let meta = String::from_utf8_lossy(meta);
        let (mut id, mut done, mut part, mut base64) = (String::new(), true, "title", false);
        for kv in meta.split(':') {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            match k {
                "i" => {
                    id = v
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || "-_+.".contains(*c))
                        .take(64)
                        .collect();
                }
                "d" => done = v != "0",
                "e" => base64 = v == "1",
                "p" => {
                    part = match v {
                        "title" => "title",
                        "body" => "body",
                        // Queries, closing, icons, buttons: nothing to show.
                        _ => return None,
                    };
                }
                _ => {}
            }
        }
        let payload = if base64 {
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .ok()?
        } else {
            payload.to_vec()
        };
        let text = String::from_utf8_lossy(&payload);
        let at = match self.pending.iter().position(|(i, _)| *i == id) {
            Some(at) => at,
            None => {
                if self.pending.len() >= MAX_PENDING {
                    self.pending.remove(0);
                }
                self.pending.push((
                    id.clone(),
                    Notice {
                        title: String::new(),
                        body: String::new(),
                    },
                ));
                self.pending.len() - 1
            }
        };
        // Kept as sent until the last part (bounded), then cleaned.
        let notice = &mut self.pending[at].1;
        let (field, max) = if part == "title" {
            (&mut notice.title, MAX_TITLE)
        } else {
            (&mut notice.body, MAX_BODY)
        };
        field.extend(text.chars().take((max * 4).saturating_sub(field.len())));
        if !done {
            return None;
        }
        let (_, raw) = self.pending.remove(at);
        let notice = Notice {
            title: clean(&raw.title, MAX_TITLE),
            body: clean(&raw.body, MAX_BODY),
        };
        (!notice.title.is_empty() || !notice.body.is_empty()).then_some(notice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(title: &str, body: &str) -> Option<Signal> {
        Some(Signal::Notify(Notice {
            title: title.into(),
            body: body.into(),
        }))
    }

    #[test]
    fn notifications() {
        assert_eq!(parse(9, b"Build finished"), notice("", "Build finished"));
        assert_eq!(parse(9, b"5 tests failed"), notice("", "5 tests failed"));
        // ConEmu's progress bar and other numbered commands.
        assert_eq!(parse(9, b"4;1;50"), None);
        assert_eq!(parse(9, b"12"), None);
        assert_eq!(parse(9, b""), None);
        assert_eq!(
            parse(777, b"notify;Codex;Approve the command; it edits files"),
            notice("Codex", "Approve the command; it edits files")
        );
        assert_eq!(
            parse(777, b"notify;Only a title"),
            notice("Only a title", "")
        );
        assert_eq!(parse(777, b"preexec"), None);
        // Control characters go; long text is cut.
        assert_eq!(parse(9, b"a\x1b[31mb\nc"), notice("", "a[31mb c"));
        let long = vec![b'x'; 5000];
        let Some(Signal::Notify(n)) = parse(9, &long) else {
            panic!("a notice");
        };
        assert_eq!(n.body.len(), MAX_BODY);
    }

    #[test]
    fn marks() {
        let m = |b: &[u8]| parse(133, b);
        assert_eq!(m(b"A"), Some(Signal::Mark(Mark::PromptStart)));
        assert_eq!(
            m(b"A;click_events=1"),
            Some(Signal::Mark(Mark::PromptStart))
        );
        assert_eq!(m(b"A;k=s"), None);
        assert_eq!(m(b"B"), Some(Signal::Mark(Mark::InputStart)));
        assert_eq!(
            m(b"C;cmdline_url=ls"),
            Some(Signal::Mark(Mark::OutputStart))
        );
        assert_eq!(m(b"D;2"), Some(Signal::Mark(Mark::Finished(Some(2)))));
        assert_eq!(m(b"D"), Some(Signal::Mark(Mark::Finished(None))));
        assert_eq!(m(b"D;0;aid=7"), Some(Signal::Mark(Mark::Finished(Some(0)))));
        assert_eq!(m(b"E;ls"), None);
        assert_eq!(m(b"Z"), None);
        assert_eq!(
            parse(633, b"E;echo a\\x3bb \\\\ c\\x0anext;nonce"),
            Some(Signal::Mark(Mark::CommandLine(
                "echo a;b \\ c\nnext".into()
            )))
        );
        assert_eq!(parse(633, b"P;Cwd=/x"), None);
    }

    #[test]
    fn kitty_notifications_in_parts() {
        let mut k = KittyNotices::default();
        assert_eq!(
            k.feed(b";Hello"),
            Some(Notice {
                title: "Hello".into(),
                body: String::new(),
            })
        );
        assert_eq!(k.feed(b"i=a:d=0;Build "), None);
        assert_eq!(k.feed(b"i=b:d=0;Other"), None);
        assert_eq!(k.feed(b"i=a:d=0;done"), None);
        assert_eq!(
            k.feed(b"i=a:p=body:e=1;YWxsIGdyZWVu"),
            Some(Notice {
                title: "Build done".into(),
                body: "all green".into(),
            })
        );
        // A query or a close shows nothing.
        assert_eq!(k.feed(b"i=q:p=?;"), None);
        assert_eq!(k.feed(b"i=b:p=close;"), None);
        // Bad base64 is dropped.
        assert_eq!(k.feed(b"e=1;***"), None);
        // Only a few are kept unfinished.
        for i in 0..20 {
            k.feed(format!("i=n{i}:d=0;x").as_bytes());
        }
        assert!(k.pending.len() <= MAX_PENDING);
    }
}
