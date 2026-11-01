// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounds the Kitty keyboard-mode stack a program can build.
//!
//! `alacritty_terminal` 0.26 keeps a stack of keyboard modes per screen
//! (`CSI > flags u` pushes, `CSI < n u` pops). Past 4096 entries it
//! removes from the wrong stack, which panics once that one is empty: a
//! program printing 16 KB of pushes would take the emulator down. This
//! filter sits in front of the emulator, mirrors the depth of the main and
//! alternate screens' stacks, and drops pushes beyond [`MAX_DEPTH`].
//!
//! It follows the emulator's parser where that matters for counting: C0
//! controls inside a sequence are executed without ending it, ESC starts a
//! new sequence, CAN/SUB cancel. Parameters are parsed as they stream, so
//! a long sequence (an alternate-screen switch hidden in padding) is still
//! seen. Only `CSI >` sequences are held back until their final byte,
//! since only they may be dropped; one longer than any real one is
//! dropped whole. Where the two could disagree, the guard counts too many
//! entries, never too few.

/// Deepest stack we let through (kitty itself keeps a handful).
pub(crate) const MAX_DEPTH: usize = 256;

/// Longest `CSI >` sequence we hold back; longer ones are dropped.
const MAX_HELD: usize = 64;

#[derive(Debug, Default)]
pub(crate) struct KeyboardStackGuard {
    /// Depth of the main and alternate screens' stacks.
    depth: [usize; 2],
    alt: bool,
    state: State,
    csi: Csi,
    /// A `CSI >` sequence held back until its final byte.
    held: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,
    /// Right after `ESC [`: the next byte may be a marker.
    CsiStart,
    Csi,
}

/// What is known about the CSI sequence being read.
#[derive(Debug, Default, Clone, Copy)]
struct Csi {
    marker: u8,
    /// The parameter being read (saturating).
    param: Option<usize>,
    first: Option<usize>,
    params_done: usize,
    /// Some parameter was 47, 1047 or 1049 (alternate screen).
    alt_screen: bool,
    /// A `CSI >` sequence too long to hold: dropped whole.
    swallowing: bool,
}

impl Csi {
    fn end_param(&mut self) {
        if matches!(self.param, Some(47 | 1047 | 1049)) {
            self.alt_screen = true;
        }
        if self.params_done == 0 {
            self.first = self.param;
        }
        self.params_done += 1;
        self.param = None;
    }
}

impl KeyboardStackGuard {
    /// Copies `bytes` to `out`, minus pushes that would exceed the limit.
    /// Sequences may be split across calls.
    pub(crate) fn filter(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &b in bytes {
            self.byte(b, out);
        }
    }

    fn holding(&self) -> bool {
        self.csi.marker == b'>'
    }

    /// Emits a byte of the current CSI sequence (or holds it back).
    fn emit(&mut self, b: u8, out: &mut Vec<u8>) {
        if !self.holding() {
            out.push(b);
        } else if !self.csi.swallowing {
            self.held.push(b);
            if self.held.len() > MAX_HELD {
                self.csi.swallowing = true;
                self.held.clear();
            }
        }
    }

    /// Lets the held bytes through (when a sequence ends early).
    fn release(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.held);
    }

    fn byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Ground => {
                if b == 0x1b {
                    self.state = State::Esc;
                } else {
                    out.push(b);
                }
            }
            State::Esc => match b {
                b'[' => {
                    self.state = State::CsiStart;
                    self.csi = Csi::default();
                }
                0x1b => out.push(0x1b),
                _ => {
                    out.extend_from_slice(&[0x1b, b]);
                    self.state = State::Ground;
                }
            },
            State::CsiStart => {
                self.state = State::Csi;
                if matches!(b, b'?' | b'<' | b'=' | b'>') {
                    self.csi.marker = b;
                    if b == b'>' {
                        self.held.clear();
                        self.held.extend_from_slice(b"\x1b[>");
                    } else {
                        out.extend_from_slice(&[0x1b, b'[', b]);
                    }
                } else {
                    out.extend_from_slice(b"\x1b[");
                    self.csi_byte(b, out);
                }
            }
            State::Csi => self.csi_byte(b, out),
        }
    }

    fn csi_byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match b {
            // ESC ends the sequence and starts a new one.
            0x1b => {
                self.release(out);
                self.state = State::Esc;
            }
            // CAN and SUB cancel it.
            0x18 | 0x1a => {
                self.emit(b, out);
                self.release(out);
                self.state = State::Ground;
            }
            b'0'..=b'9' => {
                self.emit(b, out);
                let digit = usize::from(b - b'0');
                self.csi.param = Some(
                    self.csi
                        .param
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(digit),
                );
            }
            b';' | b':' => {
                self.emit(b, out);
                self.csi.end_param();
            }
            0x40..=0x7e => {
                self.csi.end_param();
                self.state = State::Ground;
                let allowed = self.finish(b);
                if self.holding() {
                    if allowed && !self.csi.swallowing {
                        self.held.push(b);
                        self.release(out);
                    } else {
                        self.held.clear();
                    }
                } else {
                    out.push(b);
                }
            }
            // Intermediates, other parameter bytes, and C0 controls (which
            // the parser executes without ending the sequence), DEL and
            // anything above ASCII: part of the sequence.
            _ => self.emit(b, out),
        }
    }

    /// Applies a complete sequence with final byte `f` to the mirrored
    /// stacks; `false` drops it.
    fn finish(&mut self, f: u8) -> bool {
        let screen = usize::from(self.alt);
        match (self.csi.marker, f) {
            (b'>', b'u') => {
                if self.csi.swallowing || self.depth[screen] >= MAX_DEPTH {
                    return false;
                }
                self.depth[screen] += 1;
            }
            (b'<', b'u') => {
                let n = self.csi.first.unwrap_or(1).max(1);
                self.depth[screen] = self.depth[screen].saturating_sub(n);
            }
            (b'?', b'h' | b'l') if self.csi.alt_screen => {
                self.alt = f == b'h';
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(g: &mut KeyboardStackGuard, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        g.filter(input, &mut out);
        out
    }

    #[test]
    fn ordinary_output_passes_unchanged() {
        let mut g = KeyboardStackGuard::default();
        let text: &[u8] = b"plain \x1b[1;31mred\x1b[0m \x1b]0;title\x07 \x1bc \x1b\x1b[A \
                            \x1b[>1u\x1b[<u \x1b[>q \x1b[>4;1m \x1b[?1049h\x1b[?1049l \xe2\x94\x80";
        assert_eq!(run(&mut g, text), text);
        // Byte by byte, too.
        let mut g = KeyboardStackGuard::default();
        let mut out = Vec::new();
        for &b in text {
            g.filter(&[b], &mut out);
        }
        assert_eq!(out, text);
    }

    #[test]
    fn pushes_beyond_the_limit_are_dropped() {
        let mut g = KeyboardStackGuard::default();
        let push = b"\x1b[>1u";
        let out = run(&mut g, &push.repeat(MAX_DEPTH + 10));
        assert_eq!(out.len(), push.len() * MAX_DEPTH);
        // Popping makes room again.
        assert_eq!(run(&mut g, b"\x1b[<5u\x1b[>1u"), b"\x1b[<5u\x1b[>1u");
    }

    #[test]
    fn split_sequences_are_tracked() {
        let mut g = KeyboardStackGuard::default();
        let input = b"\x1b[>1u".repeat(MAX_DEPTH + 1);
        let mut out = Vec::new();
        for chunk in input.chunks(3) {
            g.filter(chunk, &mut out);
        }
        assert_eq!(out.len(), 5 * MAX_DEPTH);
    }

    #[test]
    fn each_screen_has_its_own_stack() {
        let mut g = KeyboardStackGuard::default();
        run(&mut g, &b"\x1b[>1u".repeat(MAX_DEPTH));
        // Popping on the alternate screen does not empty the main one.
        run(&mut g, b"\x1b[?1049h\x1b[<300u\x1b[?1049l");
        assert!(run(&mut g, b"\x1b[>1u").is_empty(), "main is still full");
        assert_eq!(run(&mut g, b"\x1b[?1049h\x1b[>1u"), b"\x1b[?1049h\x1b[>1u");
    }

    #[test]
    fn evasions_are_still_counted() {
        let mut g = KeyboardStackGuard::default();
        run(&mut g, &b"\x1b[>1u".repeat(MAX_DEPTH));
        // A control byte inside the push, padding zeros, a push that ends
        // an aborted sequence: all still pushes, all dropped.
        for evasion in [
            &b"\x1b[>\x071u"[..],
            &b"\x1b[>00000000000000000000000000000000000000000000000000000000000000000001u"[..],
            &b"\x1b[12\x1b[>1u"[..],
        ] {
            let out = run(&mut g, evasion);
            assert!(!out.ends_with(b"u"), "{evasion:?} got through: {out:?}");
        }
        // An alternate-screen switch hidden in a long parameter list is
        // seen: the pops below hit the alternate stack, not the main one.
        let mut long = b"\x1b[?".to_vec();
        long.extend(b"0;".repeat(200));
        long.extend(b"1049h");
        run(&mut g, &long);
        run(&mut g, b"\x1b[<999u");
        run(&mut g, b"\x1b[?1049l");
        assert!(run(&mut g, b"\x1b[>1u").is_empty(), "main is still full");
    }
}
