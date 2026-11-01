// SPDX-License-Identifier: GPL-3.0-or-later
//! Active capability queries (ARCHITECTURE.md §5.7).
//!
//! The client writes [`QUERIES`] once at attach and reads what comes back
//! until the DA1 reply, which every terminal sends and sends last
//! (terminals answer in order). [`parse_replies`] is pure: it takes the
//! bytes read so far and says what they contain, so it can be tested
//! against recorded replies and fuzzed.
//!
//! Bytes that are not replies (keys typed while the probe ran) are kept in
//! order in [`Replies::other`], so the client can hand them on as input.

use crate::caps::PxSize;
use crate::color::Rgb;

/// The query batch, in the order terminals answer it. DA1 comes last: its
/// reply ends the probe.
pub const QUERIES: &str = concat!(
    // XTVERSION: the terminal's name and version.
    "\x1b[>q",
    // Kitty graphics: query support with a 1x1 RGB image (id 31).
    "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\",
    // DECRQM 2026: synchronized output.
    "\x1b[?2026$p",
    // Kitty keyboard protocol: the current flags.
    "\x1b[?u",
    // Cell size, then text-area size, in pixels.
    "\x1b[16t",
    "\x1b[14t",
    // Background colour.
    "\x1b]11;?\x1b\\",
    // Primary device attributes: the terminator.
    "\x1b[c",
);

/// What the replies said. `None` means "no reply", which for every query
/// but DA1 means the terminal does not support it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Replies {
    /// DA1 attributes (`CSI ? 62 ; 4 ; 22 c` → `[62, 4, 22]`). Present
    /// once the probe is complete.
    pub da1: Option<Vec<u16>>,
    /// XTVERSION text, e.g. `kitty(0.35.2)` or `tmux 3.4`.
    pub xtversion: Option<String>,
    /// The Kitty graphics query was answered: `true` for `OK`, `false`
    /// for an error (the protocol exists but refused the query).
    pub kitty_graphics: Option<bool>,
    /// DECRQM 2026: whether synchronized output is supported.
    pub sync_output: Option<bool>,
    /// Kitty keyboard protocol flags currently set.
    pub kitty_keyboard: Option<u32>,
    pub cell_px: Option<PxSize>,
    pub window_px: Option<PxSize>,
    pub background: Option<Rgb>,
    /// Bytes that were not replies, in order (keys typed meanwhile, and
    /// anything after the DA1 reply).
    pub other: Vec<u8>,
    /// An unfinished escape sequence at the end of the input. More bytes
    /// may complete it; when the probe gives up waiting it is input.
    pub partial: Vec<u8>,
}

impl Replies {
    /// Whether the terminating DA1 reply has arrived.
    pub fn complete(&self) -> bool {
        self.da1.is_some()
    }

    /// DA1 attribute 4: Sixel graphics.
    pub fn sixel(&self) -> bool {
        self.da1.as_ref().is_some_and(|a| a.contains(&4))
    }
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
/// Longest reply we accept; anything longer is not one of ours.
const MAX_SEQUENCE: usize = 4096;

/// Parses everything read since the queries were sent.
pub fn parse_replies(bytes: &[u8]) -> Replies {
    let mut r = Replies::default();
    let mut i = 0;
    while i < bytes.len() {
        if r.complete() {
            r.other.extend_from_slice(&bytes[i..]);
            break;
        }
        if bytes[i] != ESC {
            r.other.push(bytes[i]);
            i += 1;
            continue;
        }
        match sequence(&bytes[i..]) {
            Seq::Partial => {
                r.partial.extend_from_slice(&bytes[i..]);
                break;
            }
            Seq::Done(len, kind) => {
                let seq = &bytes[i..i + len];
                if !apply(&mut r, kind, seq) {
                    r.other.extend_from_slice(seq);
                }
                i += len;
            }
        }
    }
    r
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Csi,
    /// DCS, APC or OSC: the body is between the introducer and ST/BEL.
    String {
        body_start: usize,
        body_end: usize,
    },
    /// ESC plus one byte (Alt+key), or a lone ESC.
    Other,
}

enum Seq {
    Done(usize, Kind),
    Partial,
}

/// Length and kind of the escape sequence at the start of `b` (`b[0]` is
/// ESC).
fn sequence(b: &[u8]) -> Seq {
    let Some(&intro) = b.get(1) else {
        return Seq::Partial;
    };
    match intro {
        b'[' => {
            for (k, &c) in b.iter().enumerate().skip(2) {
                if (0x40..=0x7e).contains(&c) {
                    return Seq::Done(k + 1, Kind::Csi);
                }
                if !(0x20..=0x3f).contains(&c) || k > MAX_SEQUENCE {
                    // Not a well-formed CSI: hand over ESC [ as input.
                    return Seq::Done(2, Kind::Other);
                }
            }
            Seq::Partial
        }
        b'P' | b'_' | b']' => {
            let mut k = 2;
            while k < b.len() {
                if k > MAX_SEQUENCE {
                    return Seq::Done(2, Kind::Other);
                }
                match b[k] {
                    BEL if intro == b']' => {
                        return Seq::Done(
                            k + 1,
                            Kind::String {
                                body_start: 2,
                                body_end: k,
                            },
                        );
                    }
                    ESC => {
                        return match b.get(k + 1) {
                            None => Seq::Partial,
                            Some(b'\\') => Seq::Done(
                                k + 2,
                                Kind::String {
                                    body_start: 2,
                                    body_end: k,
                                },
                            ),
                            // An unterminated string: not a reply.
                            Some(_) => Seq::Done(2, Kind::Other),
                        };
                    }
                    _ => k += 1,
                }
            }
            Seq::Partial
        }
        _ => Seq::Done(2, Kind::Other),
    }
}

/// Records a reply; `false` when `seq` is not one.
fn apply(r: &mut Replies, kind: Kind, seq: &[u8]) -> bool {
    match kind {
        Kind::Other => false,
        Kind::Csi => csi(r, seq),
        Kind::String {
            body_start,
            body_end,
        } => {
            let body = &seq[body_start..body_end];
            match seq[1] {
                b'P' => match body.strip_prefix(b">|") {
                    Some(text) => {
                        r.xtversion = Some(String::from_utf8_lossy(text).trim().to_owned());
                        true
                    }
                    None => false,
                },
                b'_' => match body.strip_prefix(b"G") {
                    // `G i=31;OK`, or `G i=31;ENOTSUPPORTED:...`.
                    Some(rest) => {
                        let status = rest.splitn(2, |&c| c == b';').nth(1).unwrap_or(b"");
                        r.kitty_graphics = Some(status == b"OK");
                        true
                    }
                    None => false,
                },
                b']' => match body.strip_prefix(b"11;").and_then(parse_osc_color) {
                    Some(rgb) => {
                        r.background = Some(rgb);
                        true
                    }
                    None => false,
                },
                _ => false,
            }
        }
    }
}

fn csi(r: &mut Replies, seq: &[u8]) -> bool {
    let final_byte = seq[seq.len() - 1];
    let mut body = &seq[2..seq.len() - 1];
    let private = body.first() == Some(&b'?');
    if private {
        body = &body[1..];
    }
    // Intermediates (`$` in DECRQM) sit between the parameters and the
    // final byte.
    let intermediate = body.last().filter(|c| (0x20..=0x2f).contains(*c)).copied();
    if intermediate.is_some() {
        body = &body[..body.len() - 1];
    }
    let Some(params) = parse_params(body) else {
        return false;
    };
    match (private, intermediate, final_byte) {
        (true, None, b'c') => {
            r.da1 = Some(params);
            true
        }
        (true, Some(b'$'), b'y') if params.first() == Some(&2026) => {
            // 1 set, 2 reset: supported. 0 unknown, 4 permanently reset.
            r.sync_output = Some(matches!(params.get(1), Some(1..=3)));
            true
        }
        (true, None, b'u') => {
            r.kitty_keyboard = Some(params.first().copied().map_or(0, u32::from));
            true
        }
        (false, None, b't') => match params.as_slice() {
            [6, h, w] => {
                r.cell_px = px(*w, *h);
                true
            }
            [4, h, w] => {
                r.window_px = px(*w, *h);
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn px(width: u16, height: u16) -> Option<PxSize> {
    (width > 0 && height > 0).then(|| PxSize {
        width: u32::from(width),
        height: u32::from(height),
    })
}

/// `62;4;22` → `[62, 4, 22]`; an empty parameter is 0.
fn parse_params(body: &[u8]) -> Option<Vec<u16>> {
    if body.is_empty() {
        return Some(Vec::new());
    }
    body.split(|&c| c == b';')
        .map(|p| {
            if p.is_empty() {
                Some(0)
            } else {
                std::str::from_utf8(p).ok()?.parse().ok()
            }
        })
        .collect()
}

/// `rgb:RRRR/GGGG/BBBB` (1 to 4 hex digits each), `rgba:…/AAAA`, or
/// `#RRGGBB`.
pub fn parse_osc_color(spec: &[u8]) -> Option<Rgb> {
    let spec = std::str::from_utf8(spec).ok()?.trim();
    if let Some(hex) = spec.strip_prefix('#') {
        if hex.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        let [_, r, g, b] = v.to_be_bytes();
        return Some(Rgb::new(r, g, b));
    }
    let rest = spec
        .strip_prefix("rgb:")
        .or_else(|| spec.strip_prefix("rgba:"))?;
    let mut parts = rest.split('/');
    let mut channel = || -> Option<u8> {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(p, 16).ok()?;
        let max = (1u32 << (4 * p.len())) - 1;
        // v <= max, so the result is at most 255.
        u8::try_from((v * 255 + max / 2) / max).ok()
    };
    Some(Rgb::new(channel()?, channel()?, channel()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_reply_set() {
        let input = b"\x1bP>|kitty(0.35.2)\x1b\\\x1b_Gi=31;OK\x1b\\\x1b[?2026;2$y\x1b[?0u\
                      \x1b[6;20;10t\x1b[4;800;1200t\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62;c";
        let r = parse_replies(input);
        assert!(r.complete());
        assert_eq!(r.xtversion.as_deref(), Some("kitty(0.35.2)"));
        assert_eq!(r.kitty_graphics, Some(true));
        assert_eq!(r.sync_output, Some(true));
        assert_eq!(r.kitty_keyboard, Some(0));
        assert_eq!(
            r.cell_px,
            Some(PxSize {
                width: 10,
                height: 20
            })
        );
        assert_eq!(
            r.window_px,
            Some(PxSize {
                width: 1200,
                height: 800
            })
        );
        assert_eq!(r.background, Some(Rgb::new(0x1e, 0x1e, 0x2e)));
        assert_eq!(r.da1, Some(vec![62, 0]));
        assert!(!r.sixel());
        assert!(r.other.is_empty() && r.partial.is_empty());
    }

    #[test]
    fn keys_typed_meanwhile_are_kept_in_order() {
        let r = parse_replies(b"ab\x1b[A\x1b[?62;4c\x1bxq");
        assert!(r.complete() && r.sixel());
        // The arrow key before DA1, Alt+x and q after it.
        assert_eq!(r.other, b"ab\x1b[A\x1bxq");
    }

    #[test]
    fn unfinished_sequences_wait_for_more() {
        let osc = b"\x1b]11;rgb:ffff/ffff/ffff\x07";
        let full = b"\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[?1;2c";
        for cut in 0..full.len() {
            let r = parse_replies(&full[..cut]);
            assert!(!r.complete(), "complete after {cut} bytes");
            let mut seen = r.other.clone();
            seen.extend_from_slice(&r.partial);
            // Bytes are either a reply already understood or kept.
            let unparsed = if cut >= osc.len() {
                assert!(r.background.is_some(), "OSC 11 read at {cut}");
                &full[osc.len()..cut]
            } else {
                &full[..cut]
            };
            assert_eq!(seen, unparsed, "nothing lost at {cut}");
        }
        let r = parse_replies(full);
        assert_eq!(r.background, Some(Rgb::new(255, 255, 255)));
        assert!(r.complete());
    }

    #[test]
    fn error_replies_and_unknown_modes() {
        let r = parse_replies(
            b"\x1b_Gi=31;ENOTSUPPORTED:no\x1b\\\x1b[?2026;0$y\x1b[?2027;1$y\x1b[?64;4c",
        );
        assert_eq!(r.kitty_graphics, Some(false));
        assert_eq!(r.sync_output, Some(false));
        // A mode we did not ask about is not a reply.
        assert_eq!(r.other, b"\x1b[?2027;1$y");
    }

    #[test]
    fn colour_specs() {
        assert_eq!(parse_osc_color(b"rgb:f/0/8"), Some(Rgb::new(255, 0, 136)));
        assert_eq!(
            parse_osc_color(b"rgb:ffff/8080/0000"),
            Some(Rgb::new(255, 128, 0))
        );
        assert_eq!(
            parse_osc_color(b"rgba:0000/0000/0000/ffff"),
            Some(Rgb::BLACK)
        );
        assert_eq!(parse_osc_color(b"#102030"), Some(Rgb::new(16, 32, 48)));
        assert_eq!(parse_osc_color(b"rgb:12345/0/0"), None);
        assert_eq!(parse_osc_color(b"blue"), None);
    }

    #[test]
    fn junk_never_panics_and_loses_nothing() {
        let samples: &[&[u8]] = &[
            b"\x1b",
            b"\x1b[",
            b"\x1b[\x01",
            b"\x1bP",
            b"\x1b_G\x1bx",
            b"\x1b[;;;c",
            b"\x1b[?99999999c",
        ];
        for s in samples {
            let r = parse_replies(s);
            if !r.complete() {
                let mut seen = r.other.clone();
                seen.extend_from_slice(&r.partial);
                assert_eq!(&seen, s);
            }
        }
        // A size of zero is an answer, but no size.
        let r = parse_replies(b"\x1b[6;0;0t");
        assert!(r.cell_px.is_none() && r.other.is_empty());
    }
}
