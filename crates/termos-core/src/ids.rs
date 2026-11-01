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
