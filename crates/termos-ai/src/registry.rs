// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent sessions running in this server (D-093), for the command
//! center: what each is doing, what it has used, what it waits for, and a
//! way to act on it (stop it, approve or deny its pending call).
//!
//! Sessions register themselves when they start; the registry holds weak
//! references, so a closed window's session drops out by itself.

use std::sync::{Mutex, Weak};

use termos_core::app::AppWaker;
use tokio::sync::mpsc;

use crate::agent::{Handle, Input, Pending, Phase, View};
use crate::provider::Usage;

/// What a live session is doing, in one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Starting,
    /// Waiting for the user's message.
    Ready,
    /// A turn is running.
    Busy,
    /// A tool call waits for approval.
    Asking,
    /// Waiting for a passphrase, a provider or a model.
    Setup,
    Failed,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Ready => "ready",
            State::Busy => "working",
            State::Asking => "asking",
            State::Setup => "setting up",
            State::Failed => "failed",
        }
    }
}

/// A live session, as the command center shows it.
#[derive(Debug, Clone)]
pub struct Live {
    /// Identifies it in the registry for [`Registry::act`].
    pub key: u64,
    /// The saved conversation's and audit log's id (empty while starting).
    pub session: String,
    pub profile: String,
    pub provider: String,
    pub model: String,
    pub state: State,
    /// What it is doing now ("thinking…").
    pub status: String,
    pub usage: Usage,
    /// US dollars so far, when the model's prices are known.
    pub cost: Option<f64>,
    /// The call waiting for approval.
    pub pending: Option<Pending>,
    /// Unix seconds.
    pub started: i64,
}

/// What can be done to a live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Stop the running turn.
    Stop,
    /// Approve the pending call (once).
    Approve,
    /// Deny the pending call.
    Deny,
}

struct Entry {
    key: u64,
    started: i64,
    view: Weak<Mutex<View>>,
    input: mpsc::UnboundedSender<Input>,
    waker: Weak<Mutex<Option<AppWaker>>>,
}

/// The live sessions.
#[derive(Default)]
pub struct Registry {
    entries: Mutex<Vec<Entry>>,
    next: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

impl Registry {
    /// Adds a session that just started.
    pub fn register(&self, handle: &Handle) {
        let key = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let entry = Entry {
            key,
            started: termos_core::journal::now(),
            view: std::sync::Arc::downgrade(&handle.view),
            input: handle.input.clone(),
            waker: handle.waker_weak(),
        };
        if let Ok(mut e) = self.entries.lock() {
            e.push(entry);
        }
    }

    /// The sessions still running, oldest first.
    pub fn list(&self) -> Vec<Live> {
        let Ok(mut entries) = self.entries.lock() else {
            return Vec::new();
        };
        entries.retain(|e| e.view.strong_count() > 0 && !e.input.is_closed());
        entries
            .iter()
            .filter_map(|e| {
                let view = e.view.upgrade()?;
                let v = view.lock().ok()?;
                let (state, pending) = match &v.phase {
                    Phase::Starting => (State::Starting, None),
                    Phase::Ready => (State::Ready, None),
                    Phase::Busy => (State::Busy, None),
                    Phase::Confirm(p) => (State::Asking, Some(p.clone())),
                    Phase::NeedPassphrase { .. }
                    | Phase::PickProvider { .. }
                    | Phase::PickModel { .. } => (State::Setup, None),
                    Phase::Failed(_) => (State::Failed, None),
                };
                Some(Live {
                    key: e.key,
                    session: v.session_id.clone(),
                    profile: v.profile.clone(),
                    provider: v.provider.clone(),
                    model: v.model.clone(),
                    state,
                    status: v.status.clone(),
                    usage: v.usage,
                    cost: v.cost,
                    pending,
                    started: e.started,
                })
            })
            .collect()
    }

    /// Acts on the session `key`. Returns whether it applied: approving
    /// or denying needs a pending call, stopping a running turn.
    pub fn act(&self, key: u64, act: Act) -> bool {
        let Some(live) = self.list().into_iter().find(|l| l.key == key) else {
            return false;
        };
        let input = match (act, live.state) {
            (Act::Approve, State::Asking) => Input::Approve { remember: false },
            (Act::Deny, State::Asking) => Input::Deny,
            (Act::Stop, State::Busy | State::Asking | State::Starting) => Input::Cancel,
            _ => return false,
        };
        let Ok(entries) = self.entries.lock() else {
            return false;
        };
        let Some(e) = entries.iter().find(|e| e.key == key) else {
            return false;
        };
        let sent = e.input.send(input).is_ok();
        // Its window shows what happened.
        if let Some(w) = e.waker.upgrade().and_then(|w| w.lock().ok()?.clone()) {
            w.wake();
        }
        sent
    }
}
