// SPDX-License-Identifier: GPL-3.0-or-later
//! Hyperlinks in cells (OSC 8, M13a, D-118).
//!
//! A [`crate::cell::Cell`] carries a link id; the id means an address in
//! the [`LinkTable`] of the grid the cell is in. Ids are never reused for
//! another address within a table and the tables copied from it, so a cell
//! whose id did not change still means the same address: the diff can
//! compare ids alone.

use std::collections::HashMap;
use std::sync::Arc;

/// The schemes a link may have; anything else is dropped (D-117).
pub const SCHEMES: [&str; 4] = ["http", "https", "mailto", "file"];

/// The longest address kept, in bytes.
pub const MAX_URL: usize = 2048;

/// The most addresses one table holds; past it, new links are not kept.
pub const MAX_LINKS: usize = 1024;

/// A table larger than this drops the addresses no cell uses after a frame
/// ([`crate::grid::Grid::forget_unused_links`]).
pub const KEEP_LINKS: usize = 256;

/// The scheme of `url`, lowercased, if it has one.
pub fn scheme(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then(|| scheme.to_ascii_lowercase())
}

/// Whether a program's link may be kept: an allowed scheme, no control
/// characters, at most [`MAX_URL`] bytes.
pub fn allowed(url: &str) -> bool {
    url.len() <= MAX_URL
        && !url.chars().any(char::is_control)
        && scheme(url).is_some_and(|s| SCHEMES.contains(&s.as_str()))
}

/// The addresses the cells of one grid link to, by id (0: no link).
#[derive(Debug, Clone)]
pub struct LinkTable {
    by_id: HashMap<u32, Arc<str>>,
    by_url: HashMap<Arc<str>, u32>,
    /// The next id to hand out; ids are never reused.
    next: u32,
    /// Changes whenever an address is added or dropped.
    version: u64,
    enabled: bool,
}

impl Default for LinkTable {
    fn default() -> Self {
        Self {
            by_id: HashMap::new(),
            by_url: HashMap::new(),
            next: 1,
            version: 0,
            enabled: true,
        }
    }
}

impl PartialEq for LinkTable {
    fn eq(&self, other: &Self) -> bool {
        self.by_id == other.by_id
    }
}

impl Eq for LinkTable {}

impl LinkTable {
    /// The id of `url`, adding it if it is new. 0 when links are off, the
    /// address is not [`allowed`], or the table is full.
    pub fn intern(&mut self, url: &str) -> u32 {
        if !self.enabled || !allowed(url) {
            return 0;
        }
        if let Some(id) = self.by_url.get(url) {
            return *id;
        }
        if self.by_id.len() >= MAX_LINKS {
            return 0;
        }
        let id = self.next;
        self.next = self.next.checked_add(1).unwrap_or(1).max(1);
        let url: Arc<str> = Arc::from(url);
        self.by_id.insert(id, Arc::clone(&url));
        self.by_url.insert(url, id);
        self.version += 1;
        id
    }

    /// The address of `id`.
    pub fn get(&self, id: u32) -> Option<&str> {
        self.by_id.get(&id).map(|u| &**u)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Changes whenever the table does.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Links off: [`LinkTable::intern`] keeps nothing (`[ui] links`).
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Drops every address whose id `used` does not hold.
    pub fn retain(&mut self, used: &std::collections::HashSet<u32>) {
        let before = self.by_id.len();
        self.by_id.retain(|id, _| used.contains(id));
        self.by_url.retain(|_, id| used.contains(id));
        if self.by_id.len() != before {
            self.version += 1;
        }
    }

    /// Every id and address, ordered by id (for the wire).
    pub fn entries(&self) -> Vec<(u32, String)> {
        let mut v: Vec<(u32, String)> = self
            .by_id
            .iter()
            .map(|(id, u)| (*id, u.to_string()))
            .collect();
        v.sort_unstable_by_key(|(id, _)| *id);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_safe_schemes_are_kept() {
        for ok in [
            "https://example.com/a?b=c;d",
            "HTTP://x",
            "mailto:me@example.com",
            "file:///home/me/notes.txt",
            "file://host/etc/hosts",
        ] {
            assert!(allowed(ok), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "termos-image:4",
            "termos-block:1:p",
            "ssh://host",
            "no scheme",
            ":empty",
            "1http://x",
            "https://x/\u{7}bell",
            "https://x/\u{1b}]8;;evil",
        ] {
            assert!(!allowed(bad), "{bad}");
        }
        assert!(!allowed(&format!("https://x/{}", "a".repeat(MAX_URL))));
    }

    #[test]
    fn ids_are_stable_and_never_reused() {
        let mut t = LinkTable::default();
        let a = t.intern("https://a");
        let b = t.intern("https://b");
        assert_eq!((a, b), (1, 2));
        assert_eq!(t.intern("https://a"), a);
        let v = t.version();
        t.retain(&[b].into_iter().collect());
        assert_ne!(t.version(), v);
        assert_eq!(t.get(a), None);
        assert_eq!(t.get(b), Some("https://b"));
        // The dropped address comes back under a new id.
        assert_eq!(t.intern("https://a"), 3);
        assert_eq!(
            t.entries(),
            vec![(2, "https://b".to_owned()), (3, "https://a".to_owned())]
        );
    }

    #[test]
    fn a_full_or_disabled_table_keeps_nothing() {
        let mut t = LinkTable::default();
        for i in 0..MAX_LINKS {
            assert_ne!(t.intern(&format!("https://x/{i}")), 0);
        }
        assert_eq!(t.intern("https://one-more"), 0);
        assert_ne!(t.intern("https://x/7"), 0);
        let mut off = LinkTable::default();
        off.set_enabled(false);
        assert_eq!(off.intern("https://a"), 0);
        assert!(off.is_empty());
    }
}
