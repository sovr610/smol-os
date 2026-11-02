// SPDX-License-Identifier: GPL-3.0-or-later
//! Takes images and graphics queries out of a program's output before the
//! terminal emulator sees it (ARCHITECTURE.md §5.8).
//!
//! `alacritty_terminal` drops Kitty graphics (APC) and Sixel (DCS) on the
//! floor, and does not answer the cell-size or XTSMGRAPHICS queries that
//! image tools ask first. This streaming filter finds those sequences,
//! across reads, and hands them over separately; everything else passes
//! through unchanged, in order. A sequence longer than [`MAX_SEQUENCE`] is
//! dropped (its remainder discarded up to the terminator).

// TODO(M9b): images from programs in panes on Windows, where ConPTY can filter APC and DCS sequences out.

/// Largest image sequence accepted.
pub const MAX_SEQUENCE: usize = 32 * 1024 * 1024;

/// Longest CSI sequence examined; longer ones pass through.
const MAX_CSI: usize = 64;

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
    Discard,
}

#[derive(Debug, Default)]
pub struct Interceptor {
    state: State,
    /// Bytes of the sequence being read.
    held: Vec<u8>,
    /// DCS parameters (for Sixel).
    params: Vec<u8>,
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
