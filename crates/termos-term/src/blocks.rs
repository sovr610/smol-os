// SPDX-License-Identifier: GPL-3.0-or-later
//! Command blocks: a prompt, the command typed at it, its output and its
//! exit status, from the shell's OSC 133 marks (D-116).
//!
//! The emulator keeps the text; TermOS keeps where each block is by
//! putting hidden links on it, as it anchors images: at `A` the prompt's
//! text gets the link `termos-block:<n>:p`, at `B` the command line's
//! `termos-block:<n>:c`, and when the output starts (`C`, or the line
//! ending the command line) the link stops. The marks then scroll,
//! reflow and clear with the text. Output is the text between the end of
//! a command line and the next block's prompt. The exit status from `D`
//! is kept here by block number, for the last [`MAX_BLOCKS`] blocks.

use std::collections::BTreeMap;

/// The URI scheme of the hidden links.
pub(crate) const SCHEME: &str = "termos-block:";

/// Blocks remembered (their status and command line).
const MAX_BLOCKS: usize = 2000;

/// A command block as the screen and history show it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Its number, counting from the pane's start.
    pub id: u64,
    /// The first line of its prompt: 0 is the top of the screen, negative
    /// lines are history.
    pub prompt_line: i32,
    /// The command line: as the shell reported it (OSC 633 `E`), else as
    /// typed on screen. Empty at a prompt not used yet.
    pub command: String,
    /// The lines of its output, as the program wrote them (rows the
    /// terminal wrapped joined), without trailing blank lines.
    pub output: Vec<String>,
    /// The first and last row of its output, when it has any.
    pub output_rows: Option<(i32, i32)>,
    /// Its exit status, when the shell reported one.
    pub exit: Option<i32>,
    pub state: BlockState,
}

/// Where a block is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState {
    /// At the prompt.
    Prompt,
    /// Its command is running.
    Running,
    /// Its command finished (or the prompt was left without one).
    Finished,
}

impl Block {
    /// Whether the command failed (a non-zero exit status).
    pub fn failed(&self) -> bool {
        self.exit.is_some_and(|e| e != 0)
    }
}

/// Which part of a block a hidden link marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    Prompt,
    Command,
}

/// The block and part a link's URI names, if it is one of ours.
pub(crate) fn anchor(uri: &str) -> Option<(u64, Part)> {
    let rest = uri.strip_prefix(SCHEME)?;
    let (n, part) = rest.split_once(':')?;
    let part = match part {
        "p" => Part::Prompt,
        "c" => Part::Command,
        _ => return None,
    };
    Some((n.parse().ok()?, part))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Phase {
    /// No block, or the last one finished.
    #[default]
    Idle,
    Prompt,
    Input,
    Output,
}

/// What TermOS knows about a block beyond the text.
#[derive(Debug, Clone, Default)]
pub(crate) struct Info {
    pub(crate) exit: Option<i32>,
    /// The shell said its command ran (`C`): its `D` status counts. An
    /// empty line also ends with `D`, without `C`.
    pub(crate) ran: bool,
    pub(crate) finished: bool,
    /// From OSC 633 `E`.
    pub(crate) command: Option<String>,
}

/// The marks seen so far.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    next: u64,
    current: Option<u64>,
    phase: Phase,
    info: BTreeMap<u64, Info>,
    /// Blocks in `info` that failed.
    failed: usize,
}

fn open(id: u64, part: &str) -> Vec<u8> {
    format!("\x1b]8;id=termos-block-{id}-{part};{SCHEME}{id}:{part}\x1b\\").into_bytes()
}

const CLOSE: &[u8] = b"\x1b]8;;\x1b\\";

impl Tracker {
    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }

    pub(crate) fn info(&self, id: u64) -> Option<&Info> {
        self.info.get(&id)
    }

    /// Whether some remembered block failed.
    pub(crate) fn any_failed(&self) -> bool {
        self.failed > 0
    }

    fn finish_current(&mut self, exit: Option<i32>) {
        if let Some(id) = self.current
            && let Some(info) = self.info.get_mut(&id)
            && !info.finished
        {
            info.finished = true;
            info.exit = if info.ran { exit } else { None };
            if info.exit.is_some_and(|e| e != 0) {
                self.failed += 1;
            }
        }
        self.phase = Phase::Idle;
    }

    /// `A`: what to feed the emulator.
    pub(crate) fn prompt_start(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if matches!(self.phase, Phase::Prompt | Phase::Input) {
            out.extend_from_slice(CLOSE);
        }
        self.finish_current(None);
        let id = self.next;
        self.next += 1;
        self.current = Some(id);
        self.info.insert(id, Info::default());
        while self.info.len() > MAX_BLOCKS {
            if let Some((_, old)) = self.info.pop_first()
                && old.exit.is_some_and(|e| e != 0)
            {
                self.failed -= 1;
            }
        }
        self.phase = Phase::Prompt;
        out.extend(open(id, "p"));
        out
    }

    /// `B`.
    pub(crate) fn input_start(&mut self) -> Option<Vec<u8>> {
        let id = self.current?;
        (self.phase == Phase::Prompt).then(|| {
            self.phase = Phase::Input;
            open(id, "c")
        })
    }

    /// `C` (`explicit`), or the end of the command line.
    pub(crate) fn output_start(&mut self, explicit: bool) -> Option<Vec<u8>> {
        let id = self.current?;
        if explicit && let Some(info) = self.info.get_mut(&id) {
            info.ran = true;
        }
        matches!(self.phase, Phase::Prompt | Phase::Input).then(|| {
            self.phase = Phase::Output;
            CLOSE.to_vec()
        })
    }

    /// `D;status`.
    pub(crate) fn finished(&mut self, exit: Option<i32>) -> Option<Vec<u8>> {
        let close = matches!(self.phase, Phase::Prompt | Phase::Input).then(|| CLOSE.to_vec());
        if self.phase != Phase::Idle {
            self.finish_current(exit);
        }
        close
    }

    /// 633 `E`.
    pub(crate) fn command_line(&mut self, line: String) {
        if let Some(id) = self.current
            && let Some(info) = self.info.get_mut(&id)
            && !info.finished
        {
            info.command = Some(line);
        }
    }

    /// Whether `id` is the block still being used.
    pub(crate) fn is_current(&self, id: u64) -> bool {
        self.current == Some(id) && self.phase != Phase::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_round_trip() {
        assert_eq!(anchor("termos-block:12:p"), Some((12, Part::Prompt)));
        assert_eq!(anchor("termos-block:3:c"), Some((3, Part::Command)));
        assert_eq!(anchor("termos-block:3:x"), None);
        assert_eq!(anchor("https://x"), None);
        let link = String::from_utf8(open(4, "c")).expect("utf8");
        assert!(link.ends_with("termos-block:4:c\x1b\\"), "{link}");
    }

    #[test]
    fn a_status_counts_only_after_the_command_ran() {
        let mut t = Tracker::default();
        t.prompt_start();
        assert!(t.input_start().is_some());
        assert!(t.output_start(false).is_some());
        // An empty line: `D` without `C`.
        assert_eq!(t.finished(Some(1)), None);
        assert_eq!(t.info(0).and_then(|i| i.exit), None);
        assert!(!t.any_failed());
        t.prompt_start();
        t.input_start();
        assert!(t.output_start(false).is_some());
        // `C` after the newline: no second close.
        assert_eq!(t.output_start(true), None);
        t.finished(Some(2));
        assert_eq!(t.info(1).and_then(|i| i.exit), Some(2));
        assert!(t.any_failed());
        // A prompt left without a command closes its link.
        t.prompt_start();
        let again = t.prompt_start();
        assert!(again.starts_with(CLOSE));
        assert_eq!(t.info(2).map(|i| i.finished), Some(true));
    }

    #[test]
    fn only_so_many_blocks_are_remembered() {
        let mut t = Tracker::default();
        for _ in 0..MAX_BLOCKS + 50 {
            t.prompt_start();
            t.output_start(true);
            t.finished(Some(1));
        }
        assert_eq!(t.info.len(), MAX_BLOCKS);
        assert_eq!(t.failed, MAX_BLOCKS);
        assert!(t.info(0).is_none());
    }
}
