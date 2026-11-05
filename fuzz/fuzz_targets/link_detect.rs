// SPDX-License-Identifier: GPL-3.0-or-later
//! Link and path detection (M13a, D-118): never panics, and every match
//! lies on character boundaries inside the line, in order, without
//! overlapping.
#![no_main]

use libfuzzer_sys::fuzz_target;
use termos_term::links::{ViewRow, find, find_in_line, split_line};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let found = find_in_line(text);
    let mut last_end = 0;
    for m in &found {
        assert!(m.start < m.end && m.end <= text.len());
        assert!(m.start <= m.path_end && m.path_end <= m.end);
        assert!(text.is_char_boundary(m.start) && text.is_char_boundary(m.end));
        assert!(m.start >= last_end, "matches overlap");
        last_end = m.end;
    }
    let _ = split_line(text);
    // As rows of a pane, wrapped where the text has newlines.
    let mut rows: Vec<ViewRow> = text.split('\n').map(ViewRow::plain).collect();
    let n = rows.len();
    for r in rows.iter_mut().take(n.saturating_sub(1)) {
        r.wraps = true;
    }
    for m in find(&rows, None, None, &mut |_| true) {
        assert!(!m.cells.is_empty());
        for (row, x0, x1) in &m.cells {
            assert!(usize::from(*row) < n && x0 < x1);
        }
    }
});
