// SPDX-License-Identifier: GPL-3.0-or-later
//! Agents that wait for the user (D-115, D-116): what their hooks and
//! programs' notifications do to the desktop, and the key that goes to
//! the one waiting longest.

use std::time::Instant;

use termos_core::ids::PaneId;
use termos_core::proto::ServerMsg;

use super::{Desktop, PaneKind};
use crate::attention::{self, AgentEvent, Attention, EventKind, Hooked, Need};
use crate::notify::Level;

/// The longest message shown.
const MESSAGE_CHARS: usize = 120;

impl Desktop {
    /// Where `id` is, for messages: `1:api`.
    fn place_of(&self, id: PaneId) -> String {
        self.session
            .workspaces
            .iter()
            .enumerate()
            .find(|(_, w)| w.panes().contains(&id))
            .map(|(i, w)| format!("{i}:{}", w.name))
            .unwrap_or_else(|| format!("pane {id}"))
    }

    /// A program in `id` sent a notification (OSC 9, 99, 777).
    pub(super) fn program_notice(&mut self, id: PaneId, title: &str, body: &str) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let who = match &pane.kind {
            PaneKind::Term { pty, .. } => pty.foreground_name(),
            PaneKind::App(_) => None,
        }
        .or_else(|| pane.agent.as_ref().map(|a| a.preset.clone()))
        .unwrap_or_else(|| pane.display_name());
        let message = match (title.is_empty(), body.is_empty()) {
            (false, false) => format!("{title}: {body}"),
            (false, true) => title.to_owned(),
            _ => body.to_owned(),
        };
        self.needs_you(id, &who, &message);
    }

    /// What `termos agent event` reported from an agent CLI's hook.
    pub fn agent_event(&mut self, ev: AgentEvent) -> Result<(), String> {
        let id = PaneId(ev.pane);
        let Some(pane) = self.panes.get_mut(&id) else {
            return Err(format!("no pane {id}"));
        };
        // Remember the CLI and its conversation, for restoring it.
        let hooked = pane.hooked.get_or_insert_with(|| Hooked {
            cli: ev.cli.clone(),
            account: None,
            conversation: None,
            cwd: None,
            running: true,
        });
        if hooked.cli != ev.cli || ev.kind == EventKind::Started {
            *hooked = Hooked {
                cli: ev.cli.clone(),
                account: None,
                conversation: None,
                cwd: None,
                running: true,
            };
        }
        if ev.conversation.is_some() {
            hooked.conversation.clone_from(&ev.conversation);
        }
        if ev.account.is_some() {
            hooked.account.clone_from(&ev.account);
        }
        if ev.cwd.is_some() {
            hooked.cwd.clone_from(&ev.cwd);
        }
        hooked.running = ev.kind != EventKind::Ended;
        let message = attention::short(&ev.message, MESSAGE_CHARS);
        match ev.kind {
            EventKind::NeedsYou => self.needs_you(id, &ev.cli, &message),
            EventKind::Done => self.turn_done(id, &ev.cli, &message),
            EventKind::Working | EventKind::Ended => {
                if let Some(p) = self.panes.get_mut(&id)
                    && p.attention.take().is_some()
                {
                    self.dirty = true;
                }
            }
            EventKind::Started => {}
        }
        Ok(())
    }

    /// `id` waits for the user: loud (D-115).
    pub(super) fn needs_you(&mut self, id: PaneId, who: &str, message: &str) {
        let message = attention::short(message, MESSAGE_CHARS);
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        if let Some(a) = pane.attention.as_mut().filter(|a| a.need == Need::You) {
            // Still waiting: the newer message, no second toast.
            a.message = message;
            self.dirty = true;
            return;
        }
        pane.attention = Some(Attention {
            need: Need::You,
            since: Instant::now(),
            who: who.to_owned(),
            message: message.clone(),
        });
        let place = self.place_of(id);
        let text = if message.is_empty() {
            format!("{who} in {place} needs you")
        } else {
            format!("{who} in {place} needs you: {message}")
        };
        self.notify(Level::Warn, text);
        if self.config.notifications.system {
            self.outbox.push((
                None,
                ServerMsg::Notify {
                    title: format!("{who} needs you"),
                    body: if message.is_empty() {
                        place
                    } else {
                        format!("{message} ({place})")
                    },
                },
            ));
        }
    }

    /// `id` finished a turn: quiet, a tab badge and a center entry
    /// (D-115).
    fn turn_done(&mut self, id: PaneId, who: &str, message: &str) {
        let place = self.place_of(id);
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.attention = Some(Attention {
            need: Need::Done,
            since: Instant::now(),
            who: who.to_owned(),
            message: message.to_owned(),
        });
        self.notes.record(
            Level::Info,
            if message.is_empty() {
                format!("{who} in {place} finished")
            } else {
                format!("{who} in {place} finished: {message}")
            },
        );
        self.dirty = true;
    }

    /// Goes to the agent waiting longest: those that need the user first,
    /// then finished turns (`agent.waiting`).
    pub(super) fn jump_to_waiting(&mut self) {
        let next = attention::longest_waiting(
            self.panes
                .values()
                .filter_map(|p| p.attention.as_ref().map(|a| (p.id, a))),
        );
        match next {
            Some(id) => {
                if let Err(e) = self.focus_pane(Some(id)) {
                    self.notify(Level::Warn, e);
                }
                self.dirty = true;
            }
            None => self.notify(Level::Info, "no agent is waiting"),
        }
    }

    /// The panes that want the user, longest waiting first (`pane.list`,
    /// the command center).
    pub fn waiting(&self) -> Vec<(PaneId, Attention)> {
        let mut list: Vec<(PaneId, Attention)> = self
            .panes
            .values()
            .filter_map(|p| p.attention.clone().map(|a| (p.id, a)))
            .collect();
        list.sort_by_key(|(id, a)| (a.need != Need::You, a.since, id.0));
        list
    }
}
