// SPDX-License-Identifier: GPL-3.0-or-later
//! Identifiers shared across crates.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A pane (a TermOS window). Displayed tmux-style as `%N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PaneId(pub u64);

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// An attached frontend (one terminal showing a session).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClientId(pub u64);

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "client{}", self.0)
    }
}

/// Server-wide allocator for pane ids, shared by every session so PTY
/// events can be routed by id alone (D-032).
#[derive(Debug, Clone, Default)]
pub struct PaneIds(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl PaneIds {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next(&self) -> PaneId {
        PaneId(self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    /// Makes sure future ids are above `id` (after restoring a snapshot).
    pub fn bump_past(&self, id: PaneId) {
        self.0
            .fetch_max(id.0 + 1, std::sync::atomic::Ordering::Relaxed);
    }
}
