// SPDX-License-Identifier: GPL-3.0-or-later
//! Agents that wait for the user (D-115, D-116): what their hooks and
//! programs' notifications do to the desktop, and the key that goes to
//! the one waiting longest. Also commands that finish where no one is
//! looking, and `pane.notify-when-done` (D-118).

use std::time::{Duration, Instant};

use termos_core::ids::PaneId;
use termos_core::proto::ServerMsg;

use super::{Desktop, PaneKind};
use crate::attention::{self, AgentEvent, Attention, EventKind, Hooked, Need};
use crate::notify::Level;

/// The longest message shown.
const MESSAGE_CHARS: usize = 120;

/// A pane armed with `pane.notify-when-done` and no shell integration is
/// done once it has been quiet this long after output (D-118).
pub(super) const QUIET: Duration = Duration::from_secs(5);

/// The longest command line quoted.
const COMMAND_CHARS: usize = 60;

/// Whether a command line is `tos notify --when-done` alone.
fn is_arming(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    words == ["tos", "notify", "--when-done"]
}

/// `3m12s`, `45s`, `1h02m`.
pub(crate) fn took(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

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

    /// Whether some client is looking at `id`: it is on the active
    /// workspace and a client's terminal has focus.
    fn pane_seen(&self, id: PaneId) -> bool {
        let focused = self
            .clients
            .values()
            .chain(self.current.map(|_| &self.ui))
            .any(|ui| ui.focused);
        focused && self.placement().visible().contains(&id)
    }

    /// The command line of block `block` in `id`, as typed.
    fn block_command(&self, id: PaneId, block: u64) -> Option<String> {
        match &self.panes.get(&id)?.kind {
            PaneKind::Term { vt, .. } => vt
                .blocks()
                .into_iter()
                .find(|b| b.id == block)
                .map(|b| b.command.trim().to_owned())
                .filter(|c| !c.is_empty()),
            PaneKind::App(_) => None,
        }
    }

    /// The shell said a command finished in `id` (OSC 133 `D`, D-118): a
    /// long one where no one is looking, or one asked for with
    /// `pane.notify-when-done`, is announced.
    pub(super) fn command_finished(&mut self, id: PaneId, f: termos_term::blocks::Finished) {
        let asked = self.armed.remove(&id).is_some();
        let limit = self.config.notifications.long_command;
        let long = limit > 0 && f.ran >= Duration::from_secs(limit);
        if !asked && !(long && !self.pane_seen(id)) {
            return;
        }
        let command = self.block_command(id, f.block);
        // `tos notify --when-done` typed on its own asks about the next
        // command, not itself.
        if asked && command.as_deref().is_some_and(is_arming) {
            self.armed.insert(id, None);
            return;
        }
        self.announce_done(id, command.as_deref(), f.exit, Some(f.ran));
    }

    /// Tells the user a command in `id` finished: a tick or a cross on its
    /// tab, a toast and a center entry; a failure also as a system
    /// notification where the terminal shows them.
    fn announce_done(
        &mut self,
        id: PaneId,
        command: Option<&str>,
        exit: Option<i32>,
        ran: Option<Duration>,
    ) {
        let place = self.place_of(id);
        let what = command.map_or_else(
            || "a command".to_owned(),
            |c| format!("`{}`", attention::short(c, COMMAND_CHARS)),
        );
        let after = ran.map(|d| format!(" after {}", took(d))).unwrap_or_default();
        let failed = exit.is_some_and(|e| e != 0);
        let text = match exit {
            Some(code) if code != 0 => format!("{what} in {place} failed ({code}){after}"),
            _ => format!("{what} in {place} finished{after}"),
        };
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let who = match &pane.kind {
            PaneKind::Term { pty, .. } => pty.foreground_name(),
            PaneKind::App(_) => None,
        }
        .unwrap_or_else(|| pane.display_name());
        // An agent waiting for the user stays the louder mark.
        if pane.attention.as_ref().is_none_or(|a| a.need != Need::You) {
            pane.attention = Some(Attention {
                need: if failed { Need::Failed } else { Need::Done },
                since: Instant::now(),
                who,
                message: text.clone(),
            });
        }
        self.notify(if failed { Level::Warn } else { Level::Info }, text);
        if failed && self.config.notifications.system {
            self.outbox.push((
                None,
                ServerMsg::Notify {
                    title: format!("{what} failed"),
                    body: format!("in {place}{after}"),
                },
            ));
        }
    }

    /// `pane.notify-when-done`: the focused pane's next command is
    /// announced, however long it runs.
    pub(super) fn arm_when_done(&mut self) {
        let Some(id) = self.focused() else {
            return;
        };
        if !matches!(
            self.panes.get(&id).map(|p| &p.kind),
            Some(PaneKind::Term { .. })
        ) {
            self.notify(Level::Info, "only terminal panes run commands");
            return;
        }
        self.armed.insert(id, None);
        let place = self.place_of(id);
        self.notify(
            Level::Info,
            format!("TermOS will tell you when the command in {place} finishes"),
        );
    }

    /// Output from an armed pane: without shell integration, it counts as
    /// done once quiet for [`QUIET`].
    pub(super) fn armed_output(&mut self, id: PaneId) {
        let integrated = match self.panes.get(&id).map(|p| &p.kind) {
            Some(PaneKind::Term { vt, .. }) => vt.command_running().is_some(),
            _ => return,
        };
        if let Some(quiet) = self.armed.get_mut(&id) {
            *quiet = (!integrated).then(|| Instant::now() + QUIET);
        }
    }

    /// When the next armed pane goes quiet.
    pub(super) fn armed_deadline(&self) -> Option<Instant> {
        self.armed.values().flatten().min().copied()
    }

    /// Armed panes that have gone quiet are announced.
    pub(super) fn armed_timer(&mut self, now: Instant) {
        let quiet: Vec<PaneId> = self
            .armed
            .iter()
            .filter(|(_, d)| d.is_some_and(|d| d <= now))
            .map(|(id, _)| *id)
            .collect();
        for id in quiet {
            self.armed.remove(&id);
            let place = self.place_of(id);
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            if pane.attention.as_ref().is_none_or(|a| a.need != Need::You) {
                pane.attention = Some(Attention {
                    need: Need::Done,
                    since: now,
                    who: pane.display_name(),
                    message: String::new(),
                });
            }
            self.notify(
                Level::Info,
                format!("{place} has been quiet for {}", took(QUIET)),
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_bare_arming_command_waits_for_the_next() {
        assert!(is_arming("tos notify  --when-done "));
        assert!(!is_arming("tos notify --when-done; make"));
        assert!(!is_arming("make"));
    }

    #[test]
    fn durations_read_as_people_say_them() {
        assert_eq!(took(Duration::from_secs(45)), "45s");
        assert_eq!(took(Duration::from_secs(192)), "3m12s");
        assert_eq!(took(Duration::from_secs(3720)), "1h02m");
    }
}
