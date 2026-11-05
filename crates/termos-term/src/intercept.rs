// SPDX-License-Identifier: GPL-3.0-or-later
//! Takes images, graphics queries, notifications and command marks out of
//! a program's output before the terminal emulator sees it (ARCHITECTURE.md
//! §5.8).
//!
//! `alacritty_terminal` drops Kitty graphics (APC) and Sixel (DCS) on the
//! floor, does not answer the cell-size or XTSMGRAPHICS queries that image
//! tools ask first, and ignores notifications (OSC 9, 99, 777) and shell
//! marks (OSC 133, 633). This streaming filter finds those sequences,
//! across reads, and hands them over separately; everything else passes
//! through unchanged, in order. A sequence longer than [`MAX_SEQUENCE`]
//! (an OSC longer than [`MAX_OSC`]) is dropped (its remainder discarded up
//! to the terminator).

//
//! On Windows the system's ConPTY removes these sequences before they get
//! here; the newer ConPTY the Windows zip ships beside `termos.exe` passes
//! them on (D-107, [`crate::pty::bundled_conpty`]).

/// Largest image sequence accepted.
pub const MAX_SEQUENCE: usize = 32 * 1024 * 1024;

/// Largest notification or mark accepted.
pub const MAX_OSC: usize = 64 * 1024;

/// Longest CSI sequence examined; longer ones pass through.
const MAX_CSI: usize = 64;

/// The OSC numbers taken out: notifications (9: iTerm2's, 99: kitty's,
/// 777: rxvt's `notify`) and shell marks (133: FinalTerm's, 633: VS
/// Code's).
const OSC_TAKEN: &[u16] = &[9, 99, 133, 633, 777];

/// One piece of program output, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Ordinary output for the emulator.
    Bytes(Vec<u8>),
    /// A Kitty graphics command: what follows `ESC _ G` up to `ESC \`.
    Kitty(Vec<u8>),
    /// A Sixel image: the DCS parameters and the data after `q`.
    Sixel { params: Vec<u8>, data: Vec<u8> },
    /// `CSI 16 t`: the cell size in pixels.
    CellSizeQuery,
    /// XTSMGRAPHICS `CSI ? item ; action ; value S`.
    GraphicsAttr { item: u32, action: u32 },
    /// An OSC in [`OSC_TAKEN`]: its number and what follows `number;` up
    /// to the terminator.
    Osc { number: u16, body: Vec<u8> },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,
    /// After `ESC _`: Kitty if the next byte is `G`.
    ApcStart,
    Kitty,
    /// After `ESC P`: parameters until the final byte.
    DcsParams,
    Sixel,
    Csi,
    /// After `ESC ]`: the OSC's number.
    OscNumber,
    /// The body of an OSC in [`OSC_TAKEN`].
    Osc,
    /// An OSC that was too long: skip to BEL or ST.
    OscDiscard,
    /// A sequence that was too long: skip to its terminator.
    Discard,
    /// `ESC` inside a string: `\` ends it.
    StringEsc(Inner),
}

/// Which string an `ESC` interrupted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inner {
    Kitty,
    Sixel,
    Osc,
    Discard,
}

#[derive(Debug, Default)]
pub struct Interceptor {
    state: State,
    /// Bytes of the sequence being read.
    held: Vec<u8>,
    /// DCS parameters (for Sixel).
    params: Vec<u8>,
    /// The number of the OSC being read.
    osc: u16,
}

impl Interceptor {
    /// Splits `bytes` into pieces, appending to `out`. Sequences may
    /// continue in the next call.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Piece>) {
        let mut pass = Vec::with_capacity(bytes.len());
        for &b in bytes {
            if let Some(piece) = self.byte(b, &mut pass) {
                if !pass.is_empty() {
                    out.push(Piece::Bytes(std::mem::take(&mut pass)));
                }
                out.push(piece);
            }
        }
        if !pass.is_empty() {
            out.push(Piece::Bytes(pass));
        }
    }

    fn byte(&mut self, b: u8, pass: &mut Vec<u8>) -> Option<Piece> {
        match self.state {
            State::Ground => {
                if b == 0x1b {
                    self.state = State::Esc;
                } else {
                    pass.push(b);
                }
            }
            State::Esc => match b {
                b'_' => self.state = State::ApcStart,
                b'P' => {
                    self.state = State::DcsParams;
                    self.params.clear();
                }
                b'[' => {
                    self.state = State::Csi;
                    self.held.clear();
                }
                b']' => {
                    self.state = State::OscNumber;
                    self.held.clear();
                }
                0x1b => pass.push(0x1b),
                _ => {
                    pass.extend_from_slice(&[0x1b, b]);
                    self.state = State::Ground;
                }
            },
            State::ApcStart => {
                if b == b'G' {
                    self.state = State::Kitty;
                    self.held.clear();
                } else {
                    // Another APC: the emulator ignores it as before.
                    pass.extend_from_slice(&[0x1b, b'_']);
                    self.state = State::Ground;
                    return self.byte(b, pass);
                }
            }
            State::Kitty | State::Sixel => {
                if b == 0x1b {
                    let inner = if self.state == State::Kitty {
                        Inner::Kitty
                    } else {
                        Inner::Sixel
                    };
                    self.state = State::StringEsc(inner);
                } else if self.held.len() >= MAX_SEQUENCE {
                    self.held = Vec::new();
                    self.state = State::Discard;
                } else {
                    self.held.push(b);
                }
            }
            State::DcsParams => match b {
                0x30..=0x3f => {
                    if self.params.len() < MAX_CSI {
                        self.params.push(b);
                    }
                }
                b'q' => {
                    self.state = State::Sixel;
                    self.held.clear();
                }
                _ => {
                    // Not Sixel (DECRQSS, XTGETTCAP, tmux passthrough…):
                    // hand it on exactly as it came.
                    pass.extend_from_slice(b"\x1bP");
                    pass.append(&mut self.params);
                    self.state = State::Ground;
                    return self.byte(b, pass);
                }
            },
            State::Csi => {
                self.held.push(b);
                if (0x40..=0x7e).contains(&b) {
                    self.state = State::Ground;
                    let piece = self.csi_query();
                    if piece.is_none() {
                        pass.extend_from_slice(b"\x1b[");
                        pass.append(&mut self.held);
                    }
                    self.held.clear();
                    return piece;
                }
                if b == 0x1b || self.held.len() > MAX_CSI {
                    // Not one of ours; the emulator decides what it was.
                    pass.extend_from_slice(b"\x1b[");
                    if b == 0x1b {
                        self.held.pop();
                        pass.append(&mut self.held);
                        self.state = State::Esc;
                    } else {
                        pass.append(&mut self.held);
                        self.state = State::Ground;
                    }
                }
            }
            State::OscNumber => {
                if b.is_ascii_digit() && self.held.len() < 4 {
                    self.held.push(b);
                    return None;
                }
                let number = std::str::from_utf8(&self.held)
                    .ok()
                    .and_then(|n| n.parse::<u16>().ok())
                    .filter(|n| OSC_TAKEN.contains(n));
                match number {
                    Some(n) if b == b';' => {
                        self.osc = n;
                        self.held.clear();
                        self.state = State::Osc;
                    }
                    _ => {
                        // Not one of ours: the emulator reads the rest.
                        pass.extend_from_slice(b"\x1b]");
                        pass.append(&mut self.held);
                        self.state = State::Ground;
                        return self.byte(b, pass);
                    }
                }
            }
            State::Osc => match b {
                0x07 => {
                    self.state = State::Ground;
                    return Some(Piece::Osc {
                        number: self.osc,
                        body: std::mem::take(&mut self.held),
                    });
                }
                0x1b => self.state = State::StringEsc(Inner::Osc),
                _ if self.held.len() >= MAX_OSC => {
                    self.held = Vec::new();
                    self.state = State::OscDiscard;
                }
                _ => self.held.push(b),
            },
            State::OscDiscard => match b {
                0x07 => self.state = State::Ground,
                0x1b => self.state = State::StringEsc(Inner::Discard),
                _ => {}
            },
            State::Discard => {
                if b == 0x1b {
                    self.state = State::StringEsc(Inner::Discard);
                }
            }
            State::StringEsc(inner) => {
                if b == b'\\' {
                    self.state = State::Ground;
                    let held = std::mem::take(&mut self.held);
                    return match inner {
                        Inner::Kitty => Some(Piece::Kitty(held)),
                        Inner::Sixel => Some(Piece::Sixel {
                            params: std::mem::take(&mut self.params),
                            data: held,
                        }),
                        Inner::Osc => Some(Piece::Osc {
                            number: self.osc,
                            body: held,
                        }),
                        Inner::Discard => None,
                    };
                }
                // ESC without `\` ends the string unfinished: drop it and
                // read the ESC as the start of something new.
                self.held.clear();
                self.state = State::Esc;
                return self.byte(b, pass);
            }
        }
        None
    }

    /// The query in `held` (`CSI` … final byte), if it is one of ours.
    fn csi_query(&self) -> Option<Piece> {
        // Control bytes inside a sequence are executed by the emulator and
        // do not change it; ignore them here.
        let seq: Vec<u8> = self.held.iter().copied().filter(|b| *b >= 0x20).collect();
        if seq == b"16t" {
            return Some(Piece::CellSizeQuery);
        }
        let body = seq.strip_prefix(b"?")?.strip_suffix(b"S")?;
        let mut nums = body
            .split(|c| *c == b';')
            .map(|p| std::str::from_utf8(p).ok()?.parse::<u32>().ok());
        let item = nums.next()??;
        let action = nums.next()??;
        Some(Piece::GraphicsAttr { item, action })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> Vec<Piece> {
        let mut i = Interceptor::default();
        let mut out = Vec::new();
        i.feed(input, &mut out);
        out
    }

    /// The same input byte by byte, with adjacent byte pieces merged.
    fn run_bytewise(input: &[u8]) -> Vec<Piece> {
        let mut i = Interceptor::default();
        let mut out: Vec<Piece> = Vec::new();
        for b in input {
            let mut pieces = Vec::new();
            i.feed(std::slice::from_ref(b), &mut pieces);
            for p in pieces {
                match (out.last_mut(), p) {
                    (Some(Piece::Bytes(prev)), Piece::Bytes(more)) => prev.extend(more),
                    (_, p) => out.push(p),
                }
            }
        }
        out
    }

    #[test]
    fn ordinary_output_passes_through_unchanged() {
        let text: &[u8] = b"hi \x1b[1;31mred\x1b[0m \x1b]0;t\x07 \x1bP$qm\x1b\\ \x1b_Xother\x1b\\ \x1b[?1049h \x1bc";
        assert_eq!(run(text), vec![Piece::Bytes(text.to_vec())]);
        assert_eq!(run_bytewise(text), vec![Piece::Bytes(text.to_vec())]);
    }

    #[test]
    fn images_and_queries_come_out_in_order() {
        let input: &[u8] =
            b"a\x1b_Ga=T,f=100;AAAA\x1b\\b\x1bP0;1;0q#0~~\x1b\\c\x1b[16t\x1b[?1;1;0Sd";
        let want = vec![
            Piece::Bytes(b"a".to_vec()),
            Piece::Kitty(b"a=T,f=100;AAAA".to_vec()),
            Piece::Bytes(b"b".to_vec()),
            Piece::Sixel {
                params: b"0;1;0".to_vec(),
                data: b"#0~~".to_vec(),
            },
            Piece::Bytes(b"c".to_vec()),
            Piece::CellSizeQuery,
            Piece::GraphicsAttr { item: 1, action: 1 },
            Piece::Bytes(b"d".to_vec()),
        ];
        assert_eq!(run(input), want);
        assert_eq!(run_bytewise(input), want);
    }

    #[test]
    fn notifications_and_marks_come_out_other_oscs_pass() {
        let input: &[u8] = b"a\x1b]133;A\x07$ \x1b]133;B\x1b\\ls\x1b]0;title\x07\x1b]9;done\x07\x1b]777;notify;T;B\x1b\\\x1b]8;;http://x\x1b\\z\x1b]1337;x\x07\x1b]99;i=1;hi\x1b\\";
        let want = vec![
            Piece::Bytes(b"a".to_vec()),
            Piece::Osc {
                number: 133,
                body: b"A".to_vec(),
            },
            Piece::Bytes(b"$ ".to_vec()),
            Piece::Osc {
                number: 133,
                body: b"B".to_vec(),
            },
            Piece::Bytes(b"ls\x1b]0;title\x07".to_vec()),
            Piece::Osc {
                number: 9,
                body: b"done".to_vec(),
            },
            Piece::Osc {
                number: 777,
                body: b"notify;T;B".to_vec(),
            },
            Piece::Bytes(b"\x1b]8;;http://x\x1b\\z\x1b]1337;x\x07".to_vec()),
            Piece::Osc {
                number: 99,
                body: b"i=1;hi".to_vec(),
            },
        ];
        assert_eq!(run(input), want);
        assert_eq!(run_bytewise(input), want);
        // An OSC longer than allowed is dropped up to its BEL.
        let mut big = b"\x1b]9;".to_vec();
        big.extend(std::iter::repeat_n(b'x', MAX_OSC + 10));
        big.extend_from_slice(b"\x07after");
        assert_eq!(run(&big), vec![Piece::Bytes(b"after".to_vec())]);
        // `ESC ] 9` without `;`, and five digits, are not ours.
        assert_eq!(
            run(b"\x1b]9\x07\x1b]13399;x\x07"),
            vec![Piece::Bytes(b"\x1b]9\x07\x1b]13399;x\x07".to_vec())]
        );
    }

    #[test]
    fn unfinished_and_oversized_sequences_are_dropped() {
        // ESC inside the image ends it unfinished; the ESC starts anew.
        assert_eq!(
            run(b"\x1b_Ga=T;AA\x1b[1mX"),
            vec![Piece::Bytes(b"\x1b[1mX".to_vec())]
        );
        let mut big = b"\x1bPq".to_vec();
        big.extend(std::iter::repeat_n(b'~', MAX_SEQUENCE + 10));
        big.extend_from_slice(b"\x1b\\after");
        assert_eq!(run(&big), vec![Piece::Bytes(b"after".to_vec())]);
    }
}
