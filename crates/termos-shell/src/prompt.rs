// SPDX-License-Identifier: GPL-3.0-or-later
//! Status-line prompts, tmux-style: text input (rename) and y/n
//! confirmations (`confirm-before`).

use termos_core::ids::PaneId;
use termos_core::input::{KeyCode, KeyEvent};

use crate::keys::{is_cancel, printable};

/// Restart or shut down a TermOS image (D-124).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerOff {
    Restart,
    PowerOff,
}

impl PowerOff {
    /// The `termos system` verb.
    pub fn verb(self) -> &'static str {
        match self {
            PowerOff::Restart => "restart",
            PowerOff::PowerOff => "power-off",
        }
    }
}

/// Actions that need a "y" first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmed {
    KillPane(PaneId),
    KillWorkspace(usize),
    Quit,
    /// Restart or shut down the image.
    Power(PowerOff),
    /// Restore the sessions saved by an earlier server.
    Restore,
}

/// What a prompt is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    RenameWorkspace(usize),
    RenamePane(PaneId),
    RenameSession,
    Confirm(Confirmed),
    /// A saved command's next parameter, with the values given so far
    /// (D-120).
    SnippetParam {
        snippet: termos_core::snippets::Snippet,
        values: Vec<(String, String)>,
    },
}

/// What the prompt wants after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAction {
    None,
    Cancel,
    Submit(String),
}

/// An open prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub input: String,
}

impl Prompt {
    pub fn rename(index: usize, current: &str) -> Self {
        Self {
            kind: PromptKind::RenameWorkspace(index),
            label: "(rename-workspace) ".into(),
            input: current.to_owned(),
        }
    }

    pub fn rename_pane(pane: PaneId, current: &str) -> Self {
        Self {
            kind: PromptKind::RenamePane(pane),
            label: "(rename-pane) ".into(),
            input: current.to_owned(),
        }
    }

    pub fn rename_session(current: &str) -> Self {
        Self {
            kind: PromptKind::RenameSession,
            label: "(rename-session) ".into(),
            input: current.to_owned(),
        }
    }

    pub fn confirm(what: Confirmed, question: impl Into<String>) -> Self {
        Self {
            kind: PromptKind::Confirm(what),
            label: format!("{}? (y/n) ", question.into()),
            input: String::new(),
        }
    }

    /// Asks for `param` of `snippet`, its default typed in already.
    pub fn snippet_param(
        snippet: termos_core::snippets::Snippet,
        values: Vec<(String, String)>,
        param: &termos_core::snippets::Param,
    ) -> Self {
        Self {
            label: format!("({}) {}: ", snippet.name, param.name),
            input: param.default.clone().unwrap_or_default(),
            kind: PromptKind::SnippetParam { snippet, values },
        }
    }

    fn is_confirm(&self) -> bool {
        matches!(self.kind, PromptKind::Confirm(_))
    }

    pub fn insert_str(&mut self, s: &str) {
        if !self.is_confirm() {
            self.input.extend(s.chars().filter(|c| !c.is_control()));
        }
    }

    pub fn handle_key(&mut self, k: KeyEvent) -> PromptAction {
        if is_cancel(k) {
            return PromptAction::Cancel;
        }
        if self.is_confirm() {
            return match printable(k) {
                Some('y' | 'Y') => PromptAction::Submit("y".into()),
                _ => PromptAction::Cancel,
            };
        }
        match k.code {
            KeyCode::Enter => PromptAction::Submit(self.input.trim().to_owned()),
            KeyCode::Backspace => {
                self.input.pop();
                PromptAction::None
            }
            _ if k == KeyEvent::ctrl('u') => {
                self.input.clear();
                PromptAction::None
            }
            _ => {
                if let Some(c) = printable(k) {
                    self.input.push(c);
                }
                PromptAction::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_edits_and_submits() {
        let mut p = Prompt::rename(0, "bash");
        p.handle_key(KeyEvent::plain(KeyCode::Backspace));
        p.handle_key(KeyEvent::ch('!'));
        assert_eq!(
            p.handle_key(KeyEvent::plain(KeyCode::Enter)),
            PromptAction::Submit("bas!".into())
        );
        assert_eq!(
            p.handle_key(KeyEvent::plain(KeyCode::Esc)),
            PromptAction::Cancel
        );
    }

    #[test]
    fn confirm_needs_y() {
        let mut p = Prompt::confirm(Confirmed::Quit, "quit");
        assert_eq!(p.label, "quit? (y/n) ");
        assert_eq!(
            p.handle_key(KeyEvent::ch('y')),
            PromptAction::Submit("y".into())
        );
        assert_eq!(p.handle_key(KeyEvent::ch('n')), PromptAction::Cancel);
        assert_eq!(p.handle_key(KeyEvent::ch('x')), PromptAction::Cancel);
    }
}
