// SPDX-License-Identifier: GPL-3.0-or-later
//! What was copied inside TermOS, newest first (DECISIONS.md D-050): copy
//! mode and mouse selections, programs using OSC 52, apps. It lives in
//! the server's memory only. Copies can hold passwords, so the history is
//! never written to disk, logs or snapshots, and `Debug` shows only how
//! many entries there are.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Entries larger than this keep only their first part.
pub const MAX_ENTRY_BYTES: usize = 1024 * 1024;
/// All entries together; the oldest unpinned ones go first.
pub const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
/// The default number of entries (`[clipboard] history`).
pub const DEFAULT_ENTRIES: usize = 50;

/// Where a copy came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipSource {
    /// Copy mode or a mouse selection.
    Selection,
    /// A program in a pane (OSC 52), by the pane's title.
    Program(String),
    /// An app, by its window title.
    App(String),
}

impl std::fmt::Display for ClipSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Selection => f.write_str("selection"),
            Self::Program(p) if p.is_empty() => f.write_str("a program"),
            Self::Program(p) => write!(f, "{p}"),
            Self::App(a) => write!(f, "{a}"),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ClipEntry {
    pub id: u64,
    pub text: String,
    /// The text was cut to [`MAX_ENTRY_BYTES`].
    pub truncated: bool,
    pub source: ClipSource,
    pub at: SystemTime,
    /// Pinned entries are kept when the history fills up or is cleared.
    pub pinned: bool,
}

impl std::fmt::Debug for ClipEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipEntry")
            .field("id", &self.id)
            .field("bytes", &self.text.len())
            .field("source", &self.source)
            .field("pinned", &self.pinned)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ClipboardHistory {
    entries: VecDeque<ClipEntry>,
    limit: usize,
    next_id: u64,
}

impl std::fmt::Debug for ClipboardHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardHistory")
            .field("entries", &self.entries.len())
            .field("limit", &self.limit)
            .finish()
    }
}

impl Default for ClipboardHistory {
    fn default() -> Self {
        Self::new(DEFAULT_ENTRIES)
    }
}

/// The history shared between the desktop and the clipboard manager.
pub type SharedClipboard = Arc<Mutex<ClipboardHistory>>;

impl ClipboardHistory {
    /// Keeps at most `limit` entries; 0 keeps none.
    pub fn new(limit: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            limit,
            next_id: 1,
        }
    }

    pub fn shared(limit: usize) -> SharedClipboard {
        Arc::new(Mutex::new(Self::new(limit)))
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        self.trim();
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Records a copy. The same text again moves to the top (keeping its
    /// pin) instead of appearing twice.
    pub fn push(&mut self, text: &str, source: ClipSource) {
        if self.limit == 0 || text.is_empty() {
            return;
        }
        let (text, truncated) = if text.len() > MAX_ENTRY_BYTES {
            let mut end = MAX_ENTRY_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            (&text[..end], true)
        } else {
            (text, false)
        };
        let pinned = match self.entries.iter().position(|e| e.text == text) {
            Some(i) => self.entries.remove(i).is_some_and(|e| e.pinned),
            None => false,
        };
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push_front(ClipEntry {
            id,
            text: text.to_owned(),
            truncated,
            source,
            at: SystemTime::now(),
            pinned,
        });
        self.trim();
    }

    /// Newest first.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &ClipEntry> {
        self.entries.iter()
    }

    pub fn get(&self, id: u64) -> Option<&ClipEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        self.entries.len() != before
    }

    /// Pins or unpins; returns the new state.
    pub fn toggle_pin(&mut self, id: u64) -> Option<bool> {
        let e = self.entries.iter_mut().find(|e| e.id == id)?;
        e.pinned = !e.pinned;
        Some(e.pinned)
    }

    /// Forgets every entry that is not pinned.
    pub fn clear(&mut self) {
        self.entries.retain(|e| e.pinned);
    }

    /// Drops the oldest unpinned entries while over the limits.
    fn trim(&mut self) {
        loop {
            let bytes: usize = self.entries.iter().map(|e| e.text.len()).sum();
            if self.entries.len() <= self.limit && bytes <= MAX_TOTAL_BYTES {
                break;
            }
            match self.entries.iter().rposition(|e| !e.pinned) {
                Some(i) => {
                    self.entries.remove(i);
                }
                // Only pins left: they stay, even over the limit.
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(h: &ClipboardHistory) -> Vec<&str> {
        h.entries().map(|e| e.text.as_str()).collect()
    }

    #[test]
    fn newest_first_without_duplicates_and_bounded() {
        let mut h = ClipboardHistory::new(3);
        for t in ["a", "b", "c", "b", ""] {
            h.push(t, ClipSource::Selection);
        }
        assert_eq!(texts(&h), ["b", "c", "a"]);
        h.push("d", ClipSource::Program("vim".into()));
        assert_eq!(texts(&h), ["d", "b", "c"]);
        // A pinned entry survives the limit and clearing.
        let c = h
            .entries()
            .find(|e| e.text == "c")
            .map(|e| e.id)
            .expect("c");
        assert_eq!(h.toggle_pin(c), Some(true));
        h.push("e", ClipSource::Selection);
        h.push("f", ClipSource::Selection);
        assert_eq!(texts(&h), ["f", "e", "c"]);
        h.clear();
        assert_eq!(texts(&h), ["c"]);
        assert!(h.remove(c));
        assert!(h.is_empty());
        // Off: nothing is kept.
        let mut off = ClipboardHistory::new(0);
        off.push("secret", ClipSource::Selection);
        assert!(off.is_empty());
    }

    #[test]
    fn big_copies_are_cut_and_debug_shows_no_text() {
        let mut h = ClipboardHistory::new(5);
        let big = "é".repeat(MAX_ENTRY_BYTES);
        h.push(&big, ClipSource::Selection);
        let e = h.entries().next().expect("one");
        assert!(e.truncated && e.text.len() <= MAX_ENTRY_BYTES);
        h.push("hunter2", ClipSource::Selection);
        let dbg = format!("{h:?} {:?}", h.entries().next());
        assert!(!dbg.contains("hunter2"), "{dbg}");
        // The total stays bounded: nine 1 MiB copies keep at most 8 MiB.
        let mut h = ClipboardHistory::new(50);
        for i in 0..9 {
            h.push(
                &format!("{i}{}", "x".repeat(MAX_ENTRY_BYTES - 1)),
                ClipSource::Selection,
            );
        }
        let total: usize = h.entries().map(|e| e.text.len()).sum();
        assert!(total <= MAX_TOTAL_BYTES);
        assert_eq!(h.len(), 8);
    }
}
