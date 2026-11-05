// SPDX-License-Identifier: GPL-3.0-or-later
//! Agents that wait for the user (D-115, D-116).
//!
//! A pane needs the user when its agent CLI's hooks say so (a permission
//! prompt, a question) or its program sends a notification (OSC 9, 99,
//! 777). That is loud: a ring on the pane, a badge on its tab, a toast,
//! an entry in the notification center and, where the outer terminal
//! shows them, a system notification. A finished turn is quiet: a badge on
//! its tab and a center entry. Typing into the pane, or the agent moving
//! on, clears it; a finished turn also clears once its workspace is seen.

use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use termos_core::ids::PaneId;

/// What a pane wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Waiting for the user.
    You,
    /// Finished a turn the user has not looked at yet.
    Done,
}

/// A pane's claim on the user's attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attention {
    pub need: Need,
    pub since: Instant,
    /// Who: `claude`, `codex`, `gemini`, or the program's name.
    pub who: String,
    /// What it said, short.
    pub message: String,
}

/// What an agent's hook reported (`agent.event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    /// A permission prompt or a question.
    NeedsYou,
    /// A turn finished.
    Done,
    /// The user answered: a prompt was submitted.
    Working,
    /// A conversation started or resumed (its id, for restoring).
    Started,
    /// The CLI ended.
    Ended,
}

/// One report from `termos agent event`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentEvent {
    pub pane: u64,
    /// `claude`, `codex` or `gemini`.
    pub cli: String,
    pub kind: EventKind,
    #[serde(default)]
    pub message: String,
    /// The conversation's id.
    #[serde(default)]
    pub conversation: Option<String>,
    /// The TermOS account it runs as.
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

/// An agent CLI a pane's hooks reported, for restoring it (D-115).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hooked {
    pub cli: String,
    pub account: Option<String>,
    /// Its conversation, when reported.
    pub conversation: Option<String>,
    pub cwd: Option<PathBuf>,
    /// Still running (no end reported).
    pub running: bool,
}

/// The pane waiting longest: those that need the user first, then
/// finished turns.
pub fn longest_waiting<'a>(panes: impl Iterator<Item = (PaneId, &'a Attention)>) -> Option<PaneId> {
    panes
        .min_by_key(|(id, a)| (a.need != Need::You, a.since, id.0))
        .map(|(id, _)| id)
}

/// A message cut to `max` characters on one line.
pub fn short(text: &str, max: usize) -> String {
    let line: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn needs_you_comes_before_done_then_the_oldest() {
        let t = Instant::now();
        let a = |need, secs| Attention {
            need,
            since: t + Duration::from_secs(secs),
            who: "claude".into(),
            message: String::new(),
        };
        let list = [
            (PaneId(1), a(Need::Done, 1)),
            (PaneId(2), a(Need::You, 5)),
            (PaneId(3), a(Need::You, 3)),
        ];
        assert_eq!(
            longest_waiting(list.iter().map(|(id, a)| (*id, a))),
            Some(PaneId(3))
        );
        assert_eq!(
            longest_waiting(list[..1].iter().map(|(id, a)| (*id, a))),
            Some(PaneId(1))
        );
        assert_eq!(longest_waiting(std::iter::empty()), None);
    }

    #[test]
    fn messages_are_one_short_line() {
        assert_eq!(short("Allow\n  Bash?", 20), "Allow Bash?");
        assert_eq!(short("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn events_read_from_json() {
        let e: AgentEvent = serde_json::from_str(
            r#"{"pane":3,"cli":"claude","kind":"needs-you","message":"Allow Bash?","conversation":"abc"}"#,
        )
        .expect("parses");
        assert_eq!(e.kind, EventKind::NeedsYou);
        assert_eq!(e.conversation.as_deref(), Some("abc"));
        assert_eq!(e.account, None);
    }
}
